//! CLAP hosting (via clack) and audio output (via cpal) with real-time-safe
//! queues between the MIDI, control, and audio threads.
//!
//! Structure follows the clack cpal host example (read end to end at rev
//! 27ca283), minus plugin GUIs, which zoog never opens.

pub mod audio;
pub mod discovery;
pub mod events;
pub mod host;
pub mod params;
pub mod presets;

// Re-exported so binaries drive the host without adding their own
// (version-pinned, git) clack dependency.
pub use clack_host;
