// SPDX-License-Identifier: GPL-3.0-only
// SPDX-FileCopyrightText: 2026 Corey T. White

//! MIDI port enumeration and device discovery by name match.

use std::fmt;

use midir::{MidiInput, MidiOutput};

/// The default case-insensitive substring used to find the controller.
///
/// The device exposes four ALSA port pairs (observed 2026-09-18):
/// `Minilab3:Minilab3 MIDI 36:0`, `... DIN THRU 36:1`, `... MCU/HUI 36:2`,
/// and `... ALV 36:3`. Notes, CCs, and the verified SysEx feedback all go
/// through the `MIDI` port, so the matcher includes it to avoid depending
/// on enumeration order.
pub const DEFAULT_PORT_MATCH: &str = "minilab3 midi";

#[derive(Debug)]
pub enum MidiError {
    Init(String),
    NoMatchingPort { matcher: String },
    Connect(String),
    Send(String),
    Disconnected,
}

impl fmt::Display for MidiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MidiError::Init(e) => write!(f, "MIDI init failed: {e}"),
            MidiError::NoMatchingPort { matcher } => {
                write!(f, "no MIDI port matching {matcher:?}")
            }
            MidiError::Connect(e) => write!(f, "MIDI connect failed: {e}"),
            MidiError::Send(e) => write!(f, "MIDI send failed: {e}"),
            MidiError::Disconnected => write!(f, "device disconnected"),
        }
    }
}

impl std::error::Error for MidiError {}

/// Names of all MIDI ports currently visible to the system.
#[derive(Debug, Clone, Default)]
pub struct PortListing {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

pub fn list_ports(client_name: &str) -> Result<PortListing, MidiError> {
    let input = MidiInput::new(client_name).map_err(|e| MidiError::Init(e.to_string()))?;
    let output = MidiOutput::new(client_name).map_err(|e| MidiError::Init(e.to_string()))?;
    let inputs = input
        .ports()
        .iter()
        .filter_map(|p| input.port_name(p).ok())
        .collect();
    let outputs = output
        .ports()
        .iter()
        .filter_map(|p| output.port_name(p).ok())
        .collect();
    Ok(PortListing { inputs, outputs })
}

/// Case-insensitive substring match on a port name.
pub fn name_matches(port_name: &str, matcher: &str) -> bool {
    port_name.to_lowercase().contains(&matcher.to_lowercase())
}

/// The opaque unique id of the first input port matching `matcher`, or
/// `None` when absent. The id changes when the device re-enumerates (e.g.
/// USB replug), which is how stale connections are detected.
pub fn find_input_port_id(client_name: &str, matcher: &str) -> Result<Option<String>, MidiError> {
    let input = MidiInput::new(client_name).map_err(|e| MidiError::Init(e.to_string()))?;
    Ok(input
        .ports()
        .into_iter()
        .find(|p| {
            input
                .port_name(p)
                .map(|n| name_matches(&n, matcher))
                .unwrap_or(false)
        })
        .map(|p| p.id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_matching_is_case_insensitive() {
        assert!(name_matches("Minilab3 MIDI In", "minilab"));
        assert!(name_matches("MINILAB3", "MiniLab"));
        assert!(!name_matches("Midi Through Port-0", "minilab"));
    }

    #[test]
    fn default_match_selects_only_the_midi_port() {
        // Real port names observed via `labctl ports` on 2026-09-18.
        assert!(name_matches(
            "Minilab3:Minilab3 MIDI 36:0",
            DEFAULT_PORT_MATCH
        ));
        for other in [
            "Minilab3:Minilab3 DIN THRU 36:1",
            "Minilab3:Minilab3 MCU/HUI 36:2",
            "Minilab3:Minilab3 ALV 36:3",
            "Midi Through:Midi Through Port-0 14:0",
        ] {
            assert!(!name_matches(other, DEFAULT_PORT_MATCH));
        }
    }
}
