//! Debug and development CLI. Subcommands arrive with each phase:
//! `ports`/`monitor` (Phase 1), `display`/`pad` (Phase 2), `play` (Phase 3),
//! `presets` (Phase 5).

mod hostutil;
mod presets;

use std::fs::File;
use std::io::{BufWriter, Write};
use std::process::ExitCode;
use std::sync::mpsc::channel;
use std::thread;
use std::time::Duration;

use lab_midi::capture::{CaptureLine, write_line};
use lab_midi::device::{Device, MidirDevice};
use lab_midi::event::{MidiMessage, TimedMessage};
use lab_midi::ports::{DEFAULT_PORT_MATCH, list_ports};
use lab_midi::sysex::{ColorTarget, display_text, init, pad_color};

const CLIENT_NAME: &str = "benchlab";

const USAGE: &str = "\
labctl: benchlab debug CLI

USAGE:
    labctl ports
        List all MIDI input and output ports.

    labctl monitor [--match SUBSTR] [--capture FILE]
        Connect to the controller (default match: \"minilab\",
        case-insensitive) and print every incoming message, raw hex and
        decoded. Waits and reconnects when the device is missing. With
        --capture, messages are also written to FILE in the capture format;
        typing a line of text + Enter inserts it as a '# marker' comment.
        Ctrl-C to stop.

    labctl display LINE1 [LINE2]
        Send the init handshake, then show two lines of text on the device
        display. DAW mode only.

    labctl pad <1-8|all> R G B
        Set a pad's temporary color (bank A IDs, DAW-mode message).
        Components are 0-127.

    labctl plugins
        Scan the CLAP search paths and list every plugin found.

    labctl params [--plugin MATCH] [--find SUBSTR]
        List a plugin's parameters (default plugin: \"Surge XT\"), optionally
        filtered by a case-insensitive substring of the name or module.

    labctl presets <scan|list|search|categories|load|favorite> ...
        Preset library commands; run `labctl presets` for details.

    labctl play [--plugin MATCH] [--mapping FILE] [--arturia-mode]
                [--rate HZ] [--frames N]
        Load a CLAP plugin (default match: \"Surge XT\"), connect the
        controller to it, and play. Encoders and faders drive the macro
        mapping (default file: mappings/surge-xt.toml) with soft takeover
        and display feedback; the control map defaults to DAW mode
        (--arturia-mode switches it). The main encoder browses the preset
        library (turn scrolls, Shift+turn changes category, click loads).
        Prints callback stats every 5 seconds. Ctrl-C to stop.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("ports") => cmd_ports(),
        Some("monitor") => cmd_monitor(&args[1..]),
        Some("display") => cmd_display(&args[1..]),
        Some("pad") => cmd_pad(&args[1..]),
        Some("plugins") => cmd_plugins(),
        Some("params") => cmd_params(&args[1..]),
        Some("presets") => presets::cmd_presets(&args[1..]),
        Some("play") => cmd_play(&args[1..]),
        Some("--help" | "-h" | "help") | None => {
            print!("{USAGE}");
            ExitCode::from(if args.is_empty() { 2 } else { 0 })
        }
        Some(other) => {
            eprintln!("labctl: unknown subcommand {other:?}\n");
            print!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn cmd_ports() -> ExitCode {
    match list_ports(CLIENT_NAME) {
        Ok(listing) => {
            println!("MIDI inputs:");
            for name in &listing.inputs {
                println!("  {name}");
            }
            println!("MIDI outputs:");
            for name in &listing.outputs {
                println!("  {name}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("labctl: {e}");
            ExitCode::FAILURE
        }
    }
}

struct MonitorArgs {
    matcher: String,
    capture_path: Option<String>,
}

fn parse_monitor_args(args: &[String]) -> Result<MonitorArgs, String> {
    let mut matcher = DEFAULT_PORT_MATCH.to_string();
    let mut capture_path = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--match" => {
                matcher = it
                    .next()
                    .ok_or_else(|| "--match needs a value".to_string())?
                    .clone();
            }
            "--capture" => {
                capture_path = Some(
                    it.next()
                        .ok_or_else(|| "--capture needs a value".to_string())?
                        .clone(),
                );
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(MonitorArgs {
        matcher,
        capture_path,
    })
}

fn cmd_monitor(args: &[String]) -> ExitCode {
    let args = match parse_monitor_args(args) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("labctl monitor: {e}\n");
            print!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    let mut capture = match args.capture_path.as_deref() {
        Some(path) => match File::create(path) {
            Ok(f) => {
                println!("capturing to {path}");
                Some(BufWriter::new(f))
            }
            Err(e) => {
                eprintln!("labctl monitor: cannot create {path}: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };

    // Markers typed on stdin become comments in the capture file.
    let (marker_tx, marker_rx) = channel::<String>();
    thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            let text = line.trim().to_string();
            if !text.is_empty() && marker_tx.send(text).is_err() {
                break;
            }
        }
    });

    loop {
        println!(
            "waiting for a MIDI input port matching {:?}...",
            args.matcher
        );
        let mut device = loop {
            match MidirDevice::open(CLIENT_NAME, &args.matcher) {
                Ok(device) => break device,
                Err(_) => thread::sleep(Duration::from_secs(1)),
            }
        };
        println!(
            "connected to {:?} (output port: {})",
            device.input_port_name(),
            if device.has_output() { "yes" } else { "no" }
        );

        // Inner loop: print until the port disappears, then reconnect.
        // The presence poll opens a fresh ALSA client, so throttle it.
        let mut last_presence_check = std::time::Instant::now();
        loop {
            if let Some(msg) = device.recv_timeout(Duration::from_millis(100)) {
                print_message(&msg);
                if let Some(w) = capture.as_mut() {
                    log_line(w, &CaptureLine::Message(msg));
                }
            }
            while let Ok(text) = marker_rx.try_recv() {
                println!("--- marker: {text}");
                if let Some(w) = capture.as_mut() {
                    log_line(w, &CaptureLine::Comment(text));
                }
            }
            if last_presence_check.elapsed() >= Duration::from_secs(2) {
                last_presence_check = std::time::Instant::now();
                if !port_still_present(&args.matcher) {
                    println!("device disconnected");
                    break;
                }
            }
        }
    }
}

fn cmd_plugins() -> ExitCode {
    let plugins = lab_engine::discovery::scan_all();
    if plugins.is_empty() {
        println!("no CLAP plugins found in the search paths");
    }
    for plugin in &plugins {
        println!("{plugin}\n    {}", plugin.path.display());
    }
    ExitCode::SUCCESS
}

fn cmd_params(args: &[String]) -> ExitCode {
    let mut plugin_match = "Surge XT".to_string();
    let mut find: Option<String> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--plugin" => match it.next() {
                Some(v) => plugin_match = v.clone(),
                None => {
                    eprintln!("labctl params: --plugin needs a value");
                    return ExitCode::from(2);
                }
            },
            "--find" => match it.next() {
                Some(v) => find = Some(v.to_lowercase()),
                None => {
                    eprintln!("labctl params: --find needs a value");
                    return ExitCode::from(2);
                }
            },
            other => {
                eprintln!("labctl params: unknown argument {other:?}");
                return ExitCode::from(2);
            }
        }
    }
    match run_params(&plugin_match, find.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("labctl params: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_params(plugin_match: &str, find: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    use lab_engine::host::{BenchHost, BenchHostMainThread, BenchHostShared, host_info};

    let plugin = lab_engine::discovery::find_plugin(plugin_match)?;
    let (host_tx, _host_rx) = channel();
    let plugin_id = std::ffi::CString::new(plugin.id.as_str())?;
    let mut instance = lab_engine::clack_host::prelude::PluginInstance::<BenchHost>::new(
        |_| BenchHostShared::new(host_tx),
        |_| BenchHostMainThread::new(),
        &plugin.entry,
        &plugin_id,
        &host_info(),
    )?;

    let params = lab_engine::params::list_params(&mut instance);
    println!("{plugin}: {} parameters", params.len());
    for p in params.iter().filter(|p| {
        find.is_none_or(|f| {
            p.name.to_lowercase().contains(f) || p.module.to_lowercase().contains(f)
        })
    }) {
        let value = match (&p.value_text, p.value) {
            (Some(text), _) => text.clone(),
            (None, Some(v)) => format!("{v}"),
            (None, None) => "?".to_string(),
        };
        println!(
            "  id={:<10} {:<40} [{}..{}] default={} value={} {}",
            p.id,
            format!("{}/{}", p.module, p.name),
            p.min_value,
            p.max_value,
            p.default_value,
            value,
            if p.is_automatable {
                ""
            } else {
                "(not automatable)"
            },
        );
    }
    Ok(())
}

struct PlayArgs {
    plugin: String,
    config: lab_engine::audio::EngineConfig,
    mapping: Option<String>,
    arturia_mode: bool,
}

fn parse_play_args(args: &[String]) -> Result<PlayArgs, String> {
    let mut out = PlayArgs {
        plugin: "Surge XT".to_string(),
        config: lab_engine::audio::EngineConfig::default(),
        mapping: None,
        arturia_mode: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "--plugin" => out.plugin = value("--plugin")?,
            "--mapping" => out.mapping = Some(value("--mapping")?),
            "--arturia-mode" => out.arturia_mode = true,
            "--rate" => {
                out.config.sample_rate = value("--rate")?
                    .parse()
                    .map_err(|_| "--rate must be a number".to_string())?;
            }
            "--frames" => {
                out.config.buffer_frames = value("--frames")?
                    .parse()
                    .map_err(|_| "--frames must be a number".to_string())?;
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(out)
}

/// Loads and binds the macro mapping for `plugin_id`, if one applies.
/// An explicitly requested mapping that fails is an error; the default
/// mapping file is optional.
fn cmd_play(args: &[String]) -> ExitCode {
    let args = match parse_play_args(args) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("labctl play: {e}");
            return ExitCode::from(2);
        }
    };
    match run_play(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("labctl play: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_play(args: &PlayArgs) -> Result<(), Box<dyn std::error::Error>> {
    use lab_core::app::{CoreConfig, CoreEvent, start};

    let config = CoreConfig {
        plugin_match: args.plugin.clone(),
        mapping_path: args.mapping.clone().map(std::path::PathBuf::from),
        arturia_mode: args.arturia_mode,
        engine: args.config,
        ..CoreConfig::default()
    };
    let (_handle, events) = start(config);

    println!("starting; Ctrl-C to stop.");
    let mut last_stats = std::time::Instant::now();
    for event in events {
        match event {
            CoreEvent::Ready {
                plugin_title,
                engine_running,
                device_connected,
                presets,
                controls,
                audio,
                ..
            } => {
                let audio_line = audio
                    .map(|a| {
                        format!(
                            "{} at {} Hz ({}), {} frames requested",
                            a.device_name
                                .unwrap_or_else(|| "unknown device".to_string()),
                            a.sample_rate,
                            a.sample_format,
                            a.buffer_frames
                        )
                    })
                    .unwrap_or_else(|| "no audio".to_string());
                println!(
                    "ready: {plugin_title} (engine: {}, controller: {}), {} presets browsable, {} controls mapped, {audio_line}",
                    if engine_running { "running" } else { "stub" },
                    if device_connected {
                        "connected"
                    } else {
                        "absent"
                    },
                    presets.len(),
                    controls.len(),
                );
            }
            CoreEvent::PresetLoaded { name, .. } => println!("loaded preset {name:?}"),
            CoreEvent::DeviceConnected(connected) => println!(
                "controller {}",
                if connected {
                    "connected"
                } else {
                    "disconnected"
                }
            ),
            CoreEvent::PresetLoadFailed { name } => println!("preset load FAILED: {name:?}"),
            CoreEvent::Stats {
                callbacks,
                overruns,
                max_callback_ms,
                min_frames,
                max_frames,
                stream_errors,
                dsp_load,
                output_peak,
            } => {
                if last_stats.elapsed() >= Duration::from_secs(5) {
                    last_stats = std::time::Instant::now();
                    println!(
                        "callbacks={callbacks} overruns={overruns} max_callback={max_callback_ms:.2}ms dsp={:.1}% peak={output_peak:.3} frames={min_frames}..{max_frames} stream_errors={stream_errors}",
                        dsp_load * 100.0
                    );
                }
            }
            CoreEvent::Error(e) => eprintln!("core: {e}"),
            _ => {}
        }
    }
    Ok(())
}

fn open_device_or_exit() -> Result<MidirDevice, ExitCode> {
    match MidirDevice::open(CLIENT_NAME, DEFAULT_PORT_MATCH) {
        Ok(device) if device.has_output() => Ok(device),
        Ok(_) => {
            eprintln!("labctl: device found but it has no MIDI output port");
            Err(ExitCode::FAILURE)
        }
        Err(e) => {
            eprintln!("labctl: {e}");
            Err(ExitCode::FAILURE)
        }
    }
}

fn cmd_display(args: &[String]) -> ExitCode {
    let (line1, line2) = match args {
        [l1] => (l1.as_str(), ""),
        [l1, l2] => (l1.as_str(), l2.as_str()),
        _ => {
            eprintln!("labctl display: expected LINE1 [LINE2]");
            return ExitCode::from(2);
        }
    };
    let mut device = match open_device_or_exit() {
        Ok(d) => d,
        Err(code) => return code,
    };
    let result = device
        .send(&init())
        .and_then(|_| device.send(&display_text(line1, line2)));
    match result {
        Ok(()) => {
            println!("sent: {line1:?} / {line2:?} (visible in DAW mode only)");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("labctl display: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_pad(args: &[String]) -> ExitCode {
    let usage = "labctl pad: expected <1-8|all> R G B (components 0-127)";
    let [pad, r, g, b] = args else {
        eprintln!("{usage}");
        return ExitCode::from(2);
    };
    let parse = |s: &String| s.parse::<u8>().ok().filter(|v| *v <= 127);
    let (Some(r), Some(g), Some(b)) = (parse(r), parse(g), parse(b)) else {
        eprintln!("{usage}");
        return ExitCode::from(2);
    };
    let pads: Vec<u8> = if pad == "all" {
        (0..8).collect()
    } else {
        match pad.parse::<u8>() {
            Ok(n) if (1..=8).contains(&n) => vec![n - 1],
            _ => {
                eprintln!("{usage}");
                return ExitCode::from(2);
            }
        }
    };
    let mut device = match open_device_or_exit() {
        Ok(d) => d,
        Err(code) => return code,
    };
    for index in pads {
        if let Err(e) = device.send(&pad_color(ColorTarget::PadTemporary(index), r, g, b)) {
            eprintln!("labctl pad: {e}");
            return ExitCode::FAILURE;
        }
    }
    println!("sent color ({r}, {g}, {b})");
    ExitCode::SUCCESS
}

fn port_still_present(matcher: &str) -> bool {
    list_ports(CLIENT_NAME)
        .map(|listing| {
            listing
                .inputs
                .iter()
                .any(|name| lab_midi::ports::name_matches(name, matcher))
        })
        .unwrap_or(false)
}

fn print_message(msg: &TimedMessage) {
    let hex: Vec<String> = msg.bytes.iter().map(|b| format!("{b:02X}")).collect();
    println!(
        "[{:>12}] {:<24} {}",
        msg.timestamp_us,
        hex.join(" "),
        MidiMessage::decode(&msg.bytes)
    );
}

fn log_line<W: Write>(w: &mut W, line: &CaptureLine) {
    if write_line(w, line).and_then(|_| w.flush()).is_err() {
        eprintln!("labctl monitor: capture write failed");
    }
}
