//! Debug and development CLI. Subcommands arrive with each phase:
//! `ports`/`monitor` (Phase 1), `display`/`pad` (Phase 2), `play` (Phase 3),
//! `presets` (Phase 5).

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

    labctl play [--plugin MATCH] [--rate HZ] [--frames N]
        Load a CLAP plugin (default match: \"surge\"), connect the
        controller's notes to it, and play. Prints callback stats every 5
        seconds. Ctrl-C to stop.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("ports") => cmd_ports(),
        Some("monitor") => cmd_monitor(&args[1..]),
        Some("display") => cmd_display(&args[1..]),
        Some("pad") => cmd_pad(&args[1..]),
        Some("plugins") => cmd_plugins(),
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

struct PlayArgs {
    plugin: String,
    config: lab_engine::audio::EngineConfig,
}

fn parse_play_args(args: &[String]) -> Result<PlayArgs, String> {
    let mut out = PlayArgs {
        plugin: "surge".to_string(),
        config: lab_engine::audio::EngineConfig::default(),
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
    use lab_engine::audio::activate_to_stream;
    use lab_engine::discovery::find_plugin;
    use lab_engine::events::RtMidi;
    use lab_engine::host::{BenchHost, BenchHostShared, HostThreadMessage, host_info};

    let plugin = find_plugin(&args.plugin)?;
    println!("loading {plugin}");

    let (host_tx, host_rx) = channel();
    let plugin_id = std::ffi::CString::new(plugin.id.as_str())?;
    let mut instance = lab_engine::clack_host::prelude::PluginInstance::<BenchHost>::new(
        |_| BenchHostShared::new(host_tx),
        |_| lab_engine::host::BenchHostMainThread::new(),
        &plugin.entry,
        &plugin_id,
        &host_info(),
    )?;

    // MIDI: device thread -> mpsc -> forwarder thread -> rtrb -> audio.
    let (mut producer, consumer) = rtrb::RingBuffer::<RtMidi>::new(1024);
    let mut device = MidirDevice::open(CLIENT_NAME, DEFAULT_PORT_MATCH)
        .map(Some)
        .unwrap_or_else(|e| {
            println!("no controller ({e}); running audio without MIDI input");
            None
        });
    let _forwarder = device.take().map(|mut device| {
        println!("controller connected: {:?}", device.input_port_name());
        thread::spawn(move || {
            loop {
                if let Some(msg) = device.recv_timeout(Duration::from_millis(50))
                    && let Some(rt) = RtMidi::from_bytes(msg.timestamp_us, &msg.bytes)
                {
                    let _ = producer.push(rt);
                }
            }
        })
    });

    let (_stream, stats) = activate_to_stream(&mut instance, consumer, args.config)?;
    println!(
        "audio running ({} Hz requested, {} frames requested). Play the keys; Ctrl-C to stop.",
        args.config.sample_rate, args.config.buffer_frames
    );

    let timers = instance.access_handler(|h| h.timer_support().map(|ext| (h.timers.clone(), ext)));
    let mut last_stats = std::time::Instant::now();
    loop {
        if let Ok(HostThreadMessage::RunOnMainThread) =
            host_rx.recv_timeout(Duration::from_millis(30))
        {
            instance.call_on_main_thread_callback();
        }
        if let Some((timers, timer_ext)) = &timers {
            timers.tick(timer_ext, &instance.plugin_handle());
        }
        if last_stats.elapsed() >= Duration::from_secs(5) {
            last_stats = std::time::Instant::now();
            let callbacks = stats.callbacks.load(std::sync::atomic::Ordering::Relaxed);
            let overruns = stats.overruns.load(std::sync::atomic::Ordering::Relaxed);
            let max_ms = stats
                .max_callback_ns
                .load(std::sync::atomic::Ordering::Relaxed) as f64
                / 1e6;
            println!("callbacks={callbacks} overruns={overruns} max_callback={max_ms:.2}ms");
        }
    }
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
