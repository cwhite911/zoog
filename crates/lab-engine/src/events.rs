// SPDX-License-Identifier: GPL-3.0-only
// SPDX-FileCopyrightText: 2026 Corey T. White

//! Bridging MIDI input to CLAP events for the audio thread.
//!
//! `RtMidi` is the fixed-size message pushed through the rtrb ring buffer
//! (no allocation on either side). `EventCollector` drains the ring buffer
//! inside the audio callback and fills a reusable CLAP `EventBuffer`.
//! Conversion and timestamp interpolation follow the clack cpal host
//! example.

use clack_extensions::note_ports::{NoteDialects, NotePortInfoBuffer, PluginNotePorts};
use clack_host::events::event_types::{MidiEvent, NoteOffEvent, NoteOnEvent, ParamValueEvent};
use clack_host::events::{EventFlags, Match};
use clack_host::prelude::*;
use clack_host::utils::Cookie;
use rtrb::Consumer;

use crate::host::BenchHost;
use lab_midi::event::MidiMessage;

/// A raw short MIDI message with its midir timestamp (microseconds).
/// SysEx and other long messages never enter the ring buffer.
#[derive(Debug, Clone, Copy)]
pub struct RtMidi {
    pub timestamp_us: u64,
    pub len: u8,
    pub bytes: [u8; 3],
}

impl RtMidi {
    /// Packs a short message; returns `None` for SysEx or anything longer
    /// than 3 bytes (those stay on the control path).
    pub fn from_bytes(timestamp_us: u64, bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() || bytes.len() > 3 || bytes[0] == 0xF0 {
            return None;
        }
        let mut buf = [0u8; 3];
        buf[..bytes.len()].copy_from_slice(bytes);
        Some(RtMidi {
            timestamp_us,
            len: bytes.len() as u8,
            bytes: buf,
        })
    }
}

/// A parameter change headed for the audio thread (plain value, as defined
/// by the plugin's own min/max range).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamChange {
    pub param_id: u32,
    pub value: f64,
    /// The param's cookie from `clap.params` info (pointer as usize; 0 for
    /// null). Some plugins (Odin2 2.4.1) crash on a null cookie despite the
    /// spec allowing it, so the real cookie always rides along.
    pub cookie: usize,
}

/// Where the plugin wants its note events, discovered via `clap.note-ports`.
#[derive(Debug, Clone, Copy)]
pub struct NotePortConfig {
    pub port_index: u16,
    /// True when the port does NOT support CLAP note events and MIDI bytes
    /// must be sent instead.
    pub prefers_midi: bool,
}

/// Finds the plugin's first usable note-in port, like the clack example.
pub fn find_main_note_port(instance: &mut PluginInstance<BenchHost>) -> Option<NotePortConfig> {
    let handle = instance.plugin_handle();
    let note_ports = handle.get_extension::<PluginNotePorts>()?;

    let mut buffer = NotePortInfoBuffer::new();
    let count = note_ports.count(&handle, true).min(u16::MAX as u32);

    for i in 0..count {
        let Some(info) = note_ports.get(&handle, i, true, &mut buffer) else {
            continue;
        };
        if !info
            .supported_dialects
            .intersects(NoteDialects::CLAP | NoteDialects::MIDI)
        {
            continue;
        }
        return Some(NotePortConfig {
            port_index: i as u16,
            prefers_midi: !info.supported_dialects.intersects(NoteDialects::CLAP),
        });
    }
    None
}

/// Owned by the audio thread: drains the MIDI ring buffer into a reusable
/// CLAP event buffer each process cycle. The buffer is pre-allocated;
/// nothing here allocates in the steady state.
pub struct EventCollector {
    clap_events: EventBuffer,
    note_port: NotePortConfig,
    sample_rate: u64,
}

impl EventCollector {
    pub fn new(sample_rate: u64, note_port: NotePortConfig) -> Self {
        Self {
            clap_events: EventBuffer::with_capacity(256),
            note_port,
            sample_rate,
        }
    }

    /// Drains pending parameter changes and MIDI, returning the CLAP input
    /// events for one block. Parameter events go first at time 0, keeping
    /// the buffer time-ordered; MIDI timestamps are interpolated across
    /// `0..sample_count` from the first drained event, following the clack
    /// example.
    pub fn collect(
        &mut self,
        consumer: &mut Consumer<RtMidi>,
        looper: Option<&mut Consumer<RtMidi>>,
        params: Option<&mut Consumer<ParamChange>>,
        sample_count: u64,
    ) -> InputEvents<'_> {
        self.clap_events.clear();

        // Looper playback: already scheduled in wall time by its thread,
        // so events land at the start of the current block.
        if let Some(looper) = looper {
            while let Ok(msg) = looper.pop() {
                self.push_message(&msg, 0);
            }
        }

        if let Some(params) = params {
            while let Ok(change) = params.pop() {
                // SAFETY: the cookie was read from this same plugin
                // instance's param_info and is refreshed whenever params
                // are re-enumerated (preset loads); this host never
                // invalidates cookies via a full rescan.
                let event = unsafe {
                    ParamValueEvent::new(
                        0,
                        ClapId::new(change.param_id),
                        Pckn::match_all(),
                        change.value,
                    )
                    .with_cookie(Cookie::from_raw(change.cookie as *mut std::ffi::c_void))
                };
                self.clap_events.push(&event);
            }
        }

        let mut first_timestamp = None;
        while let Ok(msg) = consumer.pop() {
            let first = *first_timestamp.get_or_insert(msg.timestamp_us);
            let sample_time =
                micro_timestamp_to_sample_time(msg.timestamp_us, first, self.sample_rate)
                    .min(sample_count.saturating_sub(1)) as u32;
            self.push_message(&msg, sample_time);
        }

        self.clap_events.as_input()
    }

    fn push_message(&mut self, msg: &RtMidi, sample_time: u32) {
        let bytes = &msg.bytes[..msg.len as usize];
        let port = self.note_port.port_index;

        if !self.note_port.prefers_midi {
            match MidiMessage::decode(bytes) {
                MidiMessage::NoteOn {
                    channel,
                    note,
                    velocity,
                } if velocity > 0 => {
                    self.clap_events.push(
                        &NoteOnEvent::new(
                            sample_time,
                            Pckn::new(port, channel as u16, note as u16, Match::All),
                            velocity as f64 / 127.0,
                        )
                        .with_flags(EventFlags::IS_LIVE),
                    );
                    return;
                }
                // NoteOn with velocity 0 is a note off per the MIDI spec.
                MidiMessage::NoteOn { channel, note, .. } => {
                    self.clap_events.push(
                        &NoteOffEvent::new(
                            sample_time,
                            Pckn::new(port, channel as u16, note as u16, Match::All),
                            0.0,
                        )
                        .with_flags(EventFlags::IS_LIVE),
                    );
                    return;
                }
                MidiMessage::NoteOff {
                    channel,
                    note,
                    velocity,
                } => {
                    self.clap_events.push(
                        &NoteOffEvent::new(
                            sample_time,
                            Pckn::new(port, channel as u16, note as u16, Match::All),
                            velocity as f64 / 127.0,
                        )
                        .with_flags(EventFlags::IS_LIVE),
                    );
                    return;
                }
                _ => {}
            }
        }

        // Everything else (pitch bend, CCs, pressure), and all note events
        // when the plugin lacks the CLAP dialect, goes through as raw MIDI.
        if msg.len == 3 {
            self.clap_events.push(
                &MidiEvent::new(sample_time, port, msg.bytes).with_flags(EventFlags::IS_LIVE),
            );
        }
    }
}

/// Interpolates a midir micro-timestamp into a sample offset, taking the
/// first event of the batch as sample 0. From the clack example.
fn micro_timestamp_to_sample_time(timestamp: u64, first_timestamp: u64, sample_rate: u64) -> u64 {
    timestamp
        .saturating_sub(first_timestamp)
        .saturating_mul(sample_rate)
        / 1_000_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rt_midi_packs_short_messages_only() {
        let m = RtMidi::from_bytes(5, &[0x90, 60, 100]).unwrap();
        assert_eq!(m.len, 3);
        assert_eq!(m.bytes, [0x90, 60, 100]);
        assert_eq!(RtMidi::from_bytes(5, &[0xD0, 3]).unwrap().len, 2);
        assert!(RtMidi::from_bytes(5, &[0xF0, 0x00, 0x20, 0x6B, 0xF7]).is_none());
        assert!(RtMidi::from_bytes(5, &[]).is_none());
    }

    #[test]
    fn timestamp_interpolation() {
        // 1000us at 48kHz = 48 samples.
        assert_eq!(micro_timestamp_to_sample_time(1_000, 0, 48_000), 48);
        assert_eq!(micro_timestamp_to_sample_time(500, 500, 48_000), 0);
        // Earlier-than-first saturates to 0.
        assert_eq!(micro_timestamp_to_sample_time(400, 500, 48_000), 0);
    }
}
