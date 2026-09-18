//! MiniLab 3 device discovery, event decoding, SysEx encoding, and the
//! [`Device`](device::Device) trait with a mock implementation.
//!
//! The mapping from raw MIDI to typed device events is data-driven via
//! [`event::ControlMap`]. No CC numbers or channels are hardcoded here; they
//! are captured from hardware with `labctl monitor` and recorded in
//! `docs/minilab3-control-map.md`.

pub mod capture;
pub mod device;
pub mod event;
pub mod ports;
pub mod rate;
pub mod sysex;
