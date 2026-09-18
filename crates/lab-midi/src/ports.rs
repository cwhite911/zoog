//! MIDI port enumeration and device discovery by name match.

use std::fmt;

use midir::{MidiInput, MidiOutput};

/// The default case-insensitive substring used to find the controller.
/// Unverified until the Phase 1 capture confirms the actual ALSA port name;
/// `labctl ports` lists what the system reports.
pub const DEFAULT_PORT_MATCH: &str = "minilab";

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_matching_is_case_insensitive() {
        assert!(name_matches("Minilab3 MIDI In", "minilab"));
        assert!(name_matches("MINILAB3", "MiniLab"));
        assert!(!name_matches("Midi Through Port-0", "minilab"));
    }
}
