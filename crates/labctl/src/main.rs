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
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("ports") => cmd_ports(),
        Some("monitor") => cmd_monitor(&args[1..]),
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
