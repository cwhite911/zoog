// SPDX-License-Identifier: GPL-3.0-only
// SPDX-FileCopyrightText: 2026 Corey T. White

//! Zoog: the GUI application.

use std::process::exit;

use lab_gui::Boot;

const USAGE: &str = "\
Zoog [--plugin MATCH] [--mapping FILE] [--arturia-mode]
         [--rate HZ] [--frames N] [--mock-device] [--stub-engine]
";

fn main() {
    let mut boot = Boot::default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next().cloned().unwrap_or_else(|| {
                eprintln!("{name} needs a value\n{USAGE}");
                exit(2);
            })
        };
        match arg.as_str() {
            "--plugin" => boot.plugin_match = value("--plugin"),
            "--mapping" => boot.mapping_path = Some(value("--mapping")),
            "--arturia-mode" => boot.arturia_mode = true,
            "--rate" => {
                boot.sample_rate = value("--rate").parse().unwrap_or_else(|_| {
                    eprintln!("--rate must be a number");
                    exit(2);
                });
            }
            "--frames" => {
                boot.buffer_frames = value("--frames").parse().unwrap_or_else(|_| {
                    eprintln!("--frames must be a number");
                    exit(2);
                });
            }
            "--mock-device" => boot.mock_device = true,
            "--stub-engine" => boot.stub_engine = true,
            "--help" | "-h" => {
                print!("{USAGE}");
                return;
            }
            other => {
                eprintln!("unknown argument {other:?}\n{USAGE}");
                exit(2);
            }
        }
    }

    if let Err(e) = lab_gui::run(boot) {
        eprintln!("Zoog: {e}");
        exit(1);
    }
}
