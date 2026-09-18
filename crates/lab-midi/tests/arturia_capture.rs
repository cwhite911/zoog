//! Replays the real Arturia-mode hardware capture through `MockDevice` and
//! the Arturia control map. Skips when the capture file is not present
//! (e.g. a fresh clone before any hardware session).

use std::collections::BTreeSet;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use lab_midi::capture::{CaptureLine, read_capture};
use lab_midi::device::{Device, MockDevice};
use lab_midi::event::{ControlMap, DeviceEvent, MidiMessage};

const CAPTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/captures/arturia.cap"
);

/// Shift+Pad3 ("Prog") switches the device to another program mid-capture;
/// messages after this marker do not belong to Arturia mode.
const PROGRAM_SWITCH_MARKER: &str = "shift pad 3 prog";

#[test]
fn arturia_capture_maps_every_control() {
    let path = Path::new(CAPTURE_PATH);
    if !path.exists() {
        eprintln!("skipping: {CAPTURE_PATH} not found");
        return;
    }
    let lines = read_capture(BufReader::new(File::open(path).unwrap())).unwrap();

    // Keep only the Arturia-mode portion.
    let arturia: Vec<CaptureLine> = lines
        .iter()
        .take_while(|line| !matches!(line, CaptureLine::Comment(c) if c == PROGRAM_SWITCH_MARKER))
        .cloned()
        .collect();
    assert!(
        arturia.len() < lines.len(),
        "program-switch marker missing; capture format changed?"
    );

    let map = ControlMap::minilab3_arturia();
    let mut device = MockDevice::from_capture(&arturia);

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
            DeviceEvent::Unmapped(m) => panic!("unmapped Arturia-mode message: {m}"),
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
    for expected in [
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
    ] {
        assert!(saw.contains(expected), "capture never produced {expected}");
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
        DeviceEvent::Unmapped(_) => "Unmapped",
    }
}
