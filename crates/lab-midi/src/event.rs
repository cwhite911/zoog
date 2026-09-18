//! Standard MIDI decoding and the typed device event layer.
//!
//! Two layers:
//! 1. [`MidiMessage`]: a faithful decode of standard MIDI, spec-defined only.
//! 2. [`DeviceEvent`]: what a control on the MiniLab 3 means. Produced by a
//!    [`ControlMap`], which is populated from hardware captures, never from
//!    guessed CC numbers.

use std::collections::{HashMap, HashSet};
use std::fmt;

/// Raw MIDI bytes with the timestamp reported by the input backend
/// (microseconds since an unspecified epoch, per midir).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedMessage {
    pub timestamp_us: u64,
    pub bytes: Vec<u8>,
}

/// A decoded standard MIDI message. Channels are 0-based (0..=15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiMessage {
    NoteOff {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    NoteOn {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    PolyPressure {
        channel: u8,
        note: u8,
        pressure: u8,
    },
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    ProgramChange {
        channel: u8,
        program: u8,
    },
    ChannelPressure {
        channel: u8,
        pressure: u8,
    },
    /// 14-bit value, 0..=16383, center 8192.
    PitchBend {
        channel: u8,
        value: u16,
    },
    /// Complete SysEx including the leading 0xF0 and trailing 0xF7.
    SysEx(Vec<u8>),
    /// Anything else (system common/realtime, truncated messages).
    Other(Vec<u8>),
}

impl MidiMessage {
    /// Decode one complete MIDI message as delivered by the input backend.
    pub fn decode(bytes: &[u8]) -> MidiMessage {
        use MidiMessage::*;
        match bytes {
            [s, d1, d2] if s & 0xF0 == 0x80 => NoteOff {
                channel: s & 0x0F,
                note: *d1,
                velocity: *d2,
            },
            [s, d1, d2] if s & 0xF0 == 0x90 => NoteOn {
                channel: s & 0x0F,
                note: *d1,
                velocity: *d2,
            },
            [s, d1, d2] if s & 0xF0 == 0xA0 => PolyPressure {
                channel: s & 0x0F,
                note: *d1,
                pressure: *d2,
            },
            [s, d1, d2] if s & 0xF0 == 0xB0 => ControlChange {
                channel: s & 0x0F,
                controller: *d1,
                value: *d2,
            },
            [s, d1] if s & 0xF0 == 0xC0 => ProgramChange {
                channel: s & 0x0F,
                program: *d1,
            },
            [s, d1] if s & 0xF0 == 0xD0 => ChannelPressure {
                channel: s & 0x0F,
                pressure: *d1,
            },
            [s, lsb, msb] if s & 0xF0 == 0xE0 => PitchBend {
                channel: s & 0x0F,
                value: (u16::from(*msb) << 7) | u16::from(*lsb),
            },
            [0xF0, .., 0xF7] => SysEx(bytes.to_vec()),
            _ => Other(bytes.to_vec()),
        }
    }
}

impl fmt::Display for MidiMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use MidiMessage::*;
        match self {
            NoteOff {
                channel,
                note,
                velocity,
            } => write!(f, "NoteOff ch{channel} note={note} vel={velocity}"),
            NoteOn {
                channel,
                note,
                velocity,
            } => write!(f, "NoteOn ch{channel} note={note} vel={velocity}"),
            PolyPressure {
                channel,
                note,
                pressure,
            } => write!(f, "PolyPressure ch{channel} note={note} val={pressure}"),
            ControlChange {
                channel,
                controller,
                value,
            } => write!(f, "CC ch{channel} cc={controller} val={value}"),
            ProgramChange { channel, program } => {
                write!(f, "ProgramChange ch{channel} prog={program}")
            }
            ChannelPressure { channel, pressure } => {
                write!(f, "ChannelPressure ch{channel} val={pressure}")
            }
            PitchBend { channel, value } => write!(f, "PitchBend ch{channel} val={value}"),
            SysEx(bytes) => write!(f, "SysEx ({} bytes)", bytes.len()),
            Other(bytes) => write!(f, "Other ({} bytes)", bytes.len()),
        }
    }
}

/// A typed event from the MiniLab 3. Indices are 0-based: encoders 0..=7,
/// faders 0..=3, pads 0..=7.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceEvent {
    NoteOn {
        note: u8,
        velocity: u8,
    },
    NoteOff {
        note: u8,
    },
    PitchBend {
        value: u16,
    },
    ModStrip {
        value: u8,
    },
    Encoder {
        index: u8,
        value: u8,
    },
    Fader {
        index: u8,
        value: u8,
    },
    PadDown {
        index: u8,
        velocity: u8,
    },
    PadUp {
        index: u8,
    },
    PadPressure {
        pressure: u8,
    },
    MainEncoderTurn {
        delta: i8,
    },
    MainEncoderClick {
        pressed: bool,
    },
    Shift {
        pressed: bool,
    },
    /// Decoded MIDI the control map has no entry for. Everything is unmapped
    /// until a hardware capture populates the map.
    Unmapped(MidiMessage),
}

/// How a relative encoder encodes its delta in a 7-bit CC value. Which one
/// the MiniLab 3 uses (and in which mode) is determined from the Phase 1
/// capture, not assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelativeEncoding {
    /// 64 is zero; 65 = +1, 63 = -1.
    OffsetFrom64,
    /// 7-bit two's complement: 1 = +1, 127 = -1.
    TwosComplement,
}

impl RelativeEncoding {
    pub fn delta(self, value: u8) -> i8 {
        match self {
            RelativeEncoding::OffsetFrom64 => value as i8 - 64,
            RelativeEncoding::TwosComplement => (value << 1) as i8 >> 1,
        }
    }
}

/// What a CC message maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CcTarget {
    Encoder(u8),
    Fader(u8),
    ModStrip,
    MainEncoderTurn(RelativeEncoding),
    /// Button-style CC: value >= 64 is pressed. Verify against capture.
    MainEncoderClick,
    /// Button-style CC: value >= 64 is pressed. Verify against capture.
    Shift,
}

/// Data-driven mapping from decoded MIDI to [`DeviceEvent`]. Starts empty;
/// populate from the hardware capture (`docs/minilab3-control-map.md`).
#[derive(Debug, Clone, Default)]
pub struct ControlMap {
    keyboard_channels: HashSet<u8>,
    /// (channel, note) -> pad index.
    pads: HashMap<(u8, u8), u8>,
    /// (channel, controller) -> target.
    ccs: HashMap<(u8, u8), CcTarget>,
    /// Channels where channel pressure means pad pressure.
    pad_pressure_channels: HashSet<u8>,
}

impl ControlMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn keyboard_channel(mut self, channel: u8) -> Self {
        self.keyboard_channels.insert(channel);
        self
    }

    pub fn pad(mut self, channel: u8, note: u8, index: u8) -> Self {
        self.pads.insert((channel, note), index);
        self
    }

    pub fn cc(mut self, channel: u8, controller: u8, target: CcTarget) -> Self {
        self.ccs.insert((channel, controller), target);
        self
    }

    pub fn pad_pressure_channel(mut self, channel: u8) -> Self {
        self.pad_pressure_channels.insert(channel);
        self
    }

    /// Map one decoded message to a typed event. Never drops input: anything
    /// without a map entry comes back as [`DeviceEvent::Unmapped`].
    pub fn map(&self, message: MidiMessage) -> DeviceEvent {
        match message {
            MidiMessage::NoteOn {
                channel,
                note,
                velocity,
            } => {
                if let Some(&index) = self.pads.get(&(channel, note)) {
                    // Per the MIDI spec, NoteOn with velocity 0 is a note off.
                    if velocity == 0 {
                        DeviceEvent::PadUp { index }
                    } else {
                        DeviceEvent::PadDown { index, velocity }
                    }
                } else if self.keyboard_channels.contains(&channel) {
                    if velocity == 0 {
                        DeviceEvent::NoteOff { note }
                    } else {
                        DeviceEvent::NoteOn { note, velocity }
                    }
                } else {
                    DeviceEvent::Unmapped(MidiMessage::NoteOn {
                        channel,
                        note,
                        velocity,
                    })
                }
            }
            MidiMessage::NoteOff {
                channel,
                note,
                velocity,
            } => {
                if let Some(&index) = self.pads.get(&(channel, note)) {
                    DeviceEvent::PadUp { index }
                } else if self.keyboard_channels.contains(&channel) {
                    DeviceEvent::NoteOff { note }
                } else {
                    DeviceEvent::Unmapped(MidiMessage::NoteOff {
                        channel,
                        note,
                        velocity,
                    })
                }
            }
            MidiMessage::ControlChange {
                channel,
                controller,
                value,
            } => match self.ccs.get(&(channel, controller)) {
                Some(CcTarget::Encoder(index)) => DeviceEvent::Encoder {
                    index: *index,
                    value,
                },
                Some(CcTarget::Fader(index)) => DeviceEvent::Fader {
                    index: *index,
                    value,
                },
                Some(CcTarget::ModStrip) => DeviceEvent::ModStrip { value },
                Some(CcTarget::MainEncoderTurn(encoding)) => DeviceEvent::MainEncoderTurn {
                    delta: encoding.delta(value),
                },
                Some(CcTarget::MainEncoderClick) => DeviceEvent::MainEncoderClick {
                    pressed: value >= 64,
                },
                Some(CcTarget::Shift) => DeviceEvent::Shift {
                    pressed: value >= 64,
                },
                None => DeviceEvent::Unmapped(MidiMessage::ControlChange {
                    channel,
                    controller,
                    value,
                }),
            },
            MidiMessage::PitchBend { channel, value } => {
                if self.keyboard_channels.contains(&channel) {
                    DeviceEvent::PitchBend { value }
                } else {
                    DeviceEvent::Unmapped(MidiMessage::PitchBend { channel, value })
                }
            }
            MidiMessage::ChannelPressure { channel, pressure } => {
                if self.pad_pressure_channels.contains(&channel) {
                    DeviceEvent::PadPressure { pressure }
                } else {
                    DeviceEvent::Unmapped(MidiMessage::ChannelPressure { channel, pressure })
                }
            }
            other => DeviceEvent::Unmapped(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_note_on_off() {
        assert_eq!(
            MidiMessage::decode(&[0x90, 60, 100]),
            MidiMessage::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100
            }
        );
        assert_eq!(
            MidiMessage::decode(&[0x83, 61, 0]),
            MidiMessage::NoteOff {
                channel: 3,
                note: 61,
                velocity: 0
            }
        );
    }

    #[test]
    fn decodes_cc_and_pitch_bend() {
        assert_eq!(
            MidiMessage::decode(&[0xB1, 74, 42]),
            MidiMessage::ControlChange {
                channel: 1,
                controller: 74,
                value: 42
            }
        );
        // lsb=0, msb=0x40 -> center 8192
        assert_eq!(
            MidiMessage::decode(&[0xE0, 0x00, 0x40]),
            MidiMessage::PitchBend {
                channel: 0,
                value: 8192
            }
        );
        // full range
        assert_eq!(
            MidiMessage::decode(&[0xE0, 0x7F, 0x7F]),
            MidiMessage::PitchBend {
                channel: 0,
                value: 16383
            }
        );
    }

    #[test]
    fn decodes_pressure_and_program() {
        assert_eq!(
            MidiMessage::decode(&[0xD9, 88]),
            MidiMessage::ChannelPressure {
                channel: 9,
                pressure: 88
            }
        );
        assert_eq!(
            MidiMessage::decode(&[0xA0, 36, 55]),
            MidiMessage::PolyPressure {
                channel: 0,
                note: 36,
                pressure: 55
            }
        );
        assert_eq!(
            MidiMessage::decode(&[0xC2, 7]),
            MidiMessage::ProgramChange {
                channel: 2,
                program: 7
            }
        );
    }

    #[test]
    fn decodes_sysex_and_other() {
        let sysex = [0xF0, 0x00, 0x20, 0x6B, 0xF7];
        assert_eq!(
            MidiMessage::decode(&sysex),
            MidiMessage::SysEx(sysex.to_vec())
        );
        assert_eq!(MidiMessage::decode(&[0xF8]), MidiMessage::Other(vec![0xF8]));
        // Truncated note-on is Other, not a bogus decode.
        assert_eq!(
            MidiMessage::decode(&[0x90, 60]),
            MidiMessage::Other(vec![0x90, 60])
        );
    }

    #[test]
    fn relative_encodings() {
        let e = RelativeEncoding::OffsetFrom64;
        assert_eq!(e.delta(64), 0);
        assert_eq!(e.delta(65), 1);
        assert_eq!(e.delta(63), -1);
        assert_eq!(e.delta(70), 6);

        let e = RelativeEncoding::TwosComplement;
        assert_eq!(e.delta(0), 0);
        assert_eq!(e.delta(1), 1);
        assert_eq!(e.delta(127), -1);
        assert_eq!(e.delta(125), -3);
    }

    fn test_map() -> ControlMap {
        // Synthetic map for tests only; real values come from the capture.
        ControlMap::new()
            .keyboard_channel(0)
            .pad(9, 36, 0)
            .pad(9, 37, 1)
            .pad_pressure_channel(9)
            .cc(0, 10, CcTarget::Encoder(0))
            .cc(0, 11, CcTarget::Fader(2))
            .cc(0, 1, CcTarget::ModStrip)
            .cc(
                0,
                20,
                CcTarget::MainEncoderTurn(RelativeEncoding::OffsetFrom64),
            )
            .cc(0, 21, CcTarget::MainEncoderClick)
            .cc(0, 22, CcTarget::Shift)
    }

    #[test]
    fn maps_keyboard_and_pads() {
        let m = test_map();
        assert_eq!(
            m.map(MidiMessage::decode(&[0x90, 60, 100])),
            DeviceEvent::NoteOn {
                note: 60,
                velocity: 100
            }
        );
        // NoteOn velocity 0 is note off.
        assert_eq!(
            m.map(MidiMessage::decode(&[0x90, 60, 0])),
            DeviceEvent::NoteOff { note: 60 }
        );
        assert_eq!(
            m.map(MidiMessage::decode(&[0x99, 36, 80])),
            DeviceEvent::PadDown {
                index: 0,
                velocity: 80
            }
        );
        assert_eq!(
            m.map(MidiMessage::decode(&[0x89, 37, 0])),
            DeviceEvent::PadUp { index: 1 }
        );
        assert_eq!(
            m.map(MidiMessage::decode(&[0xD9, 42])),
            DeviceEvent::PadPressure { pressure: 42 }
        );
    }

    #[test]
    fn maps_ccs() {
        let m = test_map();
        assert_eq!(
            m.map(MidiMessage::decode(&[0xB0, 10, 99])),
            DeviceEvent::Encoder {
                index: 0,
                value: 99
            }
        );
        assert_eq!(
            m.map(MidiMessage::decode(&[0xB0, 11, 5])),
            DeviceEvent::Fader { index: 2, value: 5 }
        );
        assert_eq!(
            m.map(MidiMessage::decode(&[0xB0, 1, 64])),
            DeviceEvent::ModStrip { value: 64 }
        );
        assert_eq!(
            m.map(MidiMessage::decode(&[0xB0, 20, 63])),
            DeviceEvent::MainEncoderTurn { delta: -1 }
        );
        assert_eq!(
            m.map(MidiMessage::decode(&[0xB0, 21, 127])),
            DeviceEvent::MainEncoderClick { pressed: true }
        );
        assert_eq!(
            m.map(MidiMessage::decode(&[0xB0, 22, 0])),
            DeviceEvent::Shift { pressed: false }
        );
    }

    #[test]
    fn unmapped_falls_through() {
        let m = test_map();
        let msg = MidiMessage::decode(&[0xB5, 99, 1]);
        assert_eq!(m.map(msg.clone()), DeviceEvent::Unmapped(msg));
        // Empty map leaves everything unmapped.
        let empty = ControlMap::new();
        let note = MidiMessage::decode(&[0x90, 60, 100]);
        assert_eq!(empty.map(note.clone()), DeviceEvent::Unmapped(note));
    }
}
