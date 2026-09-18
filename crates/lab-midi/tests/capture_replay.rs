//! Replays the real hardware captures through `MockDevice` and the
//! corresponding control maps. Each test skips when its capture file is not
//! present (e.g. a fresh clone before any hardware session).

use std::collections::BTreeSet;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use lab_midi::capture::{CaptureLine, read_capture};
use lab_midi::device::{Device, MockDevice};
use lab_midi::event::{ControlMap, DeviceEvent, MidiMessage};

fn capture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/captures")
        .join(name)
}

/// Replay `lines` through a mock device with `map`; panic on any unmapped
/// non-SysEx message and return coverage (encoders, faders, pads, variants).
fn replay(
    lines: &[CaptureLine],
    map: &ControlMap,
) -> (
    BTreeSet<u8>,
    BTreeSet<u8>,
    BTreeSet<u8>,
    BTreeSet<&'static str>,
) {
    let mut device = MockDevice::from_capture(lines);
    let mut encoders = BTreeSet::new();
    let mut faders = BTreeSet::new();
    let mut pads = BTreeSet::new();
    let mut saw = BTreeSet::new();
    while let Some(msg) = device.try_recv() {
        let decoded = MidiMessage::decode(&msg.bytes);
        // Device-originated SysEx notifications are expected and unmapped.
        if matches!(decoded, MidiMessage::SysEx(_)) {
            continue;
        }
        match map.map(decoded) {
            DeviceEvent::Unmapped(m) => panic!("unmapped message: {m}"),
            DeviceEvent::Encoder { index, .. } => {
                encoders.insert(index);
            }
            DeviceEvent::Fader { index, .. } => {
                faders.insert(index);
            }
            DeviceEvent::PadDown { index, .. } => {
                pads.insert(index);
            }
            event => {
                saw.insert(variant_name(&event));
            }
        }
    }
    (encoders, faders, pads, saw)
}

fn assert_full_coverage(
    (encoders, faders, pads, saw): (
        BTreeSet<u8>,
        BTreeSet<u8>,
        BTreeSet<u8>,
        BTreeSet<&'static str>,
    ),
    extra_variants: &[&str],
) {
    assert_eq!(
        encoders.into_iter().collect::<Vec<_>>(),
        (0..8).collect::<Vec<_>>()
    );
    assert_eq!(
        faders.into_iter().collect::<Vec<_>>(),
        (0..4).collect::<Vec<_>>()
    );
    assert_eq!(
        pads.into_iter().collect::<Vec<_>>(),
        (0..8).collect::<Vec<_>>()
    );
    let base = [
        "NoteOn",
        "NoteOff",
        "PitchBend",
        "ModStrip",
        "PadUp",
        "PadPressure",
        "MainEncoderTurn",
        "MainEncoderShiftTurn",
        "MainEncoderClick",
        "Shift",
    ];
    for expected in base.iter().chain(extra_variants) {
        assert!(saw.contains(expected), "capture never produced {expected}");
    }
}

#[test]
fn arturia_capture_maps_every_control() {
    let path = capture_path("arturia.cap");
    if !path.exists() {
        eprintln!("skipping: {} not found", path.display());
        return;
    }
    let lines = read_capture(BufReader::new(File::open(path).unwrap())).unwrap();

    // Shift+Pad3 ("Prog") switches the device to the DAW program
    // mid-capture; messages after this marker do not belong to Arturia mode.
    let marker = "shift pad 3 prog";
    let arturia: Vec<CaptureLine> = lines
        .iter()
        .take_while(|line| !matches!(line, CaptureLine::Comment(c) if c == marker))
        .cloned()
        .collect();
    assert!(
        arturia.len() < lines.len(),
        "program-switch marker missing; capture format changed?"
    );

    assert_full_coverage(replay(&arturia, &ControlMap::minilab3_arturia()), &[]);
}

#[test]
fn daw_capture_maps_every_control() {
    let path = capture_path("daw.cap");
    if !path.exists() {
        eprintln!("skipping: {} not found", path.display());
        return;
    }
    let lines = read_capture(BufReader::new(File::open(path).unwrap())).unwrap();
    // No transport pads were touched in this capture; the transport CCs are
    // covered by the tail of arturia.cap (see daw_transport_tail below).
    assert_full_coverage(replay(&lines, &ControlMap::minilab3_daw()), &[]);
}

#[test]
fn daw_transport_tail_of_arturia_capture_maps() {
    // After Shift+Pad3 the device switched into the DAW program; its
    // transport CCs and Shift CC were recorded at the end of arturia.cap.
    let path = capture_path("arturia.cap");
    if !path.exists() {
        eprintln!("skipping: {} not found", path.display());
        return;
    }
    let lines = read_capture(BufReader::new(File::open(path).unwrap())).unwrap();
    let marker = "shift pad 4 repeat";
    let tail: Vec<CaptureLine> = lines
        .iter()
        .skip_while(|line| !matches!(line, CaptureLine::Comment(c) if c == marker))
        .cloned()
        .collect();
    assert!(!tail.is_empty(), "transport marker missing");

    let map = ControlMap::minilab3_daw();
    let mut device = MockDevice::from_capture(&tail);
    let mut transports = BTreeSet::new();
    while let Some(msg) = device.try_recv() {
        let decoded = MidiMessage::decode(&msg.bytes);
        if matches!(decoded, MidiMessage::SysEx(_)) {
            continue;
        }
        match map.map(decoded) {
            DeviceEvent::Unmapped(m) => panic!("unmapped message: {m}"),
            DeviceEvent::Transport { control, .. } => {
                transports.insert(format!("{control:?}"));
            }
            _ => {}
        }
    }
    for expected in ["Loop", "Stop", "Play", "Record", "Tap"] {
        assert!(
            transports.contains(expected),
            "tail never produced Transport::{expected}"
        );
    }
}

fn variant_name(event: &DeviceEvent) -> &'static str {
    match event {
        DeviceEvent::NoteOn { .. } => "NoteOn",
        DeviceEvent::NoteOff { .. } => "NoteOff",
        DeviceEvent::PitchBend { .. } => "PitchBend",
        DeviceEvent::ModStrip { .. } => "ModStrip",
        DeviceEvent::Encoder { .. } => "Encoder",
        DeviceEvent::Fader { .. } => "Fader",
        DeviceEvent::PadDown { .. } => "PadDown",
        DeviceEvent::PadUp { .. } => "PadUp",
        DeviceEvent::PadPressure { .. } => "PadPressure",
        DeviceEvent::MainEncoderTurn { .. } => "MainEncoderTurn",
        DeviceEvent::MainEncoderShiftTurn { .. } => "MainEncoderShiftTurn",
        DeviceEvent::MainEncoderClick { .. } => "MainEncoderClick",
        DeviceEvent::Shift { .. } => "Shift",
        DeviceEvent::Transport { .. } => "Transport",
        DeviceEvent::Unmapped(_) => "Unmapped",
    }
}
