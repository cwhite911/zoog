//! MIDI phrase looper (scope amendment to PLAN.md: record/loop on the
//! DAW-mode transport pads).
//!
//! Two parts:
//! * [`LooperLogic`]: a pure state machine (button presses and note events
//!   in, scheduler commands and display text out), unit tested.
//! * [`spawn`]: the playback thread, which replays the captured phrase into
//!   its own lock-free ring buffer toward the audio callback.
//!
//! Workflow on the hardware (Shift + pads in DAW mode):
//! * **Record**: arm; recording starts at your first note. Record again
//!   closes the loop and playback starts immediately. While playing,
//!   Record toggles overdub.
//! * **Loop**: closes the loop while recording; otherwise restarts
//!   playback from the top.
//! * **Play**: (re)starts playback of the captured loop.
//! * **Stop**: while recording, discards the take; while playing, stops;
//!   pressed again when stopped, clears the loop.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use lab_engine::events::RtMidi;

/// Shortest closable loop; a shorter take keeps recording instead.
const MIN_LOOP: Duration = Duration::from_millis(200);

/// One captured MIDI message, offset from the loop start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoopEvent {
    pub offset: Duration,
    pub bytes: [u8; 3],
    pub len: u8,
}

/// Commands to the playback thread.
#[derive(Debug)]
pub enum LooperCommand {
    /// Replace the loop and start playing from the top.
    Start {
        events: Vec<LoopEvent>,
        length: Duration,
    },
    /// Add overdubbed events to the current loop.
    Overdub(Vec<LoopEvent>),
    /// Restart playback from the top (no-op when no loop is loaded).
    Restart,
    /// Stop playback (keeps the loop; releases hanging notes).
    Stop,
    /// Drop the loop entirely.
    Clear,
    /// Swap in a fresh producer after an audio stream rebuild.
    Ring(rtrb::Producer<RtMidi>),
}

/// The transport buttons the looper responds to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LooperButton {
    Record,
    Loop,
    Play,
    Stop,
}

#[derive(Debug)]
enum State {
    Empty,
    /// Record pressed; waiting for the first note.
    Armed,
    Recording {
        started: Instant,
        events: Vec<LoopEvent>,
    },
    Playing {
        length: Duration,
        epoch: Instant,
        overdub: bool,
    },
    Stopped {
        length: Duration,
    },
}

/// What a state transition asks the caller to do.
#[derive(Debug, Default)]
pub struct Output {
    pub command: Option<LooperCommand>,
    /// New status line for displays, when it changed.
    pub status: Option<String>,
}

impl Output {
    fn status(text: impl Into<String>) -> Self {
        Output {
            command: None,
            status: Some(text.into()),
        }
    }

    fn with_command(mut self, command: LooperCommand) -> Self {
        self.command = Some(command);
        self
    }
}

/// Pure looper state machine.
#[derive(Debug)]
pub struct LooperLogic {
    state: State,
}

impl Default for LooperLogic {
    fn default() -> Self {
        Self::new()
    }
}

impl LooperLogic {
    pub fn new() -> Self {
        LooperLogic {
            state: State::Empty,
        }
    }

    pub fn status(&self) -> String {
        match &self.state {
            State::Empty => "loop: empty".to_string(),
            State::Armed => "loop: armed (play a note)".to_string(),
            State::Recording { started, .. } => {
                format!("loop: REC {:.1}s", started.elapsed().as_secs_f32())
            }
            State::Playing {
                length, overdub, ..
            } => format!(
                "loop: {} {:.1}s",
                if *overdub { "OVERDUB" } else { "playing" },
                length.as_secs_f32()
            ),
            State::Stopped { length } => {
                format!(
                    "loop: stopped {:.1}s (Stop again clears)",
                    length.as_secs_f32()
                )
            }
        }
    }

    /// Handles a transport button press.
    pub fn press(&mut self, button: LooperButton, now: Instant) -> Output {
        match button {
            LooperButton::Record => self.press_record(now),
            LooperButton::Loop => match &self.state {
                State::Recording { .. } => self.close_loop(now),
                State::Playing { .. } | State::Stopped { .. } => self.press_play(now),
                _ => Output::default(),
            },
            LooperButton::Play => self.press_play(now),
            LooperButton::Stop => self.press_stop(),
        }
    }

    fn press_record(&mut self, now: Instant) -> Output {
        match &mut self.state {
            State::Empty | State::Stopped { .. } => {
                self.state = State::Armed;
                Output::status(self.status())
            }
            State::Armed => {
                self.state = State::Empty;
                Output::status(self.status())
            }
            State::Recording { .. } => self.close_loop(now),
            State::Playing { overdub, .. } => {
                *overdub = !*overdub;
                Output::status(self.status())
            }
        }
    }

    fn close_loop(&mut self, now: Instant) -> Output {
        let State::Recording { started, events } = &mut self.state else {
            return Output::default();
        };
        let length = now.duration_since(*started);
        if length < MIN_LOOP || events.is_empty() {
            // Too short to be a phrase: keep recording.
            return Output::default();
        }
        let events = std::mem::take(events);
        self.state = State::Playing {
            length,
            epoch: now,
            overdub: false,
        };
        Output::status(self.status()).with_command(LooperCommand::Start { events, length })
    }

    fn press_play(&mut self, now: Instant) -> Output {
        match &self.state {
            State::Playing { length, .. } | State::Stopped { length } => {
                let length = *length;
                self.state = State::Playing {
                    length,
                    epoch: now,
                    overdub: false,
                };
                Output::status(self.status()).with_command(LooperCommand::Restart)
            }
            _ => Output::default(),
        }
    }

    fn press_stop(&mut self) -> Output {
        match &self.state {
            State::Recording { .. } | State::Armed => {
                self.state = State::Empty;
                Output::status(format!("loop: take discarded; {}", self.status()))
                    .with_command(LooperCommand::Clear)
            }
            State::Playing { length, .. } => {
                let length = *length;
                self.state = State::Stopped { length };
                Output::status(self.status()).with_command(LooperCommand::Stop)
            }
            State::Stopped { .. } => {
                self.state = State::Empty;
                Output::status(self.status()).with_command(LooperCommand::Clear)
            }
            State::Empty => Output::default(),
        }
    }

    /// Feeds a playable MIDI message (notes only). Returns a command when
    /// the message must reach the playback thread (overdub).
    pub fn note(&mut self, bytes: [u8; 3], len: u8, now: Instant) -> Option<LooperCommand> {
        match &mut self.state {
            State::Armed => {
                self.state = State::Recording {
                    started: now,
                    events: vec![LoopEvent {
                        offset: Duration::ZERO,
                        bytes,
                        len,
                    }],
                };
                None
            }
            State::Recording { started, events } => {
                events.push(LoopEvent {
                    offset: now.duration_since(*started),
                    bytes,
                    len,
                });
                None
            }
            State::Playing {
                length,
                epoch,
                overdub: true,
            } => {
                let offset_ns = now.duration_since(*epoch).as_nanos() % length.as_nanos().max(1);
                Some(LooperCommand::Overdub(vec![LoopEvent {
                    offset: Duration::from_nanos(offset_ns as u64),
                    bytes,
                    len,
                }]))
            }
            _ => None,
        }
    }

    /// Whether the looper wants note events right now.
    pub fn captures_notes(&self) -> bool {
        matches!(
            self.state,
            State::Armed | State::Recording { .. } | State::Playing { overdub: true, .. }
        )
    }
}

/// Spawns the playback thread. It owns its own producer into the audio
/// callback and replays the loop with sleep-based scheduling (about a
/// millisecond of jitter, well under one audio block).
pub fn spawn(mut producer: rtrb::Producer<RtMidi>) -> Sender<LooperCommand> {
    let (tx, rx) = channel();
    thread::Builder::new()
        .name("benchlab-looper".to_string())
        .spawn(move || playback_thread(&mut producer, &rx))
        .expect("spawning the looper thread cannot fail");
    tx
}

fn playback_thread(producer: &mut rtrb::Producer<RtMidi>, commands: &Receiver<LooperCommand>) {
    let mut events: Vec<LoopEvent> = Vec::new();
    let mut length = Duration::ZERO;
    let mut playing = false;
    let mut epoch = Instant::now();
    let mut cycle: u64 = 0;
    let mut next_index = 0usize;
    // Notes the loop has switched on, to release on stop ([status, key]).
    let mut held: Vec<[u8; 2]> = Vec::new();

    let push = |producer: &mut rtrb::Producer<RtMidi>, ev: &LoopEvent, held: &mut Vec<[u8; 2]>| {
        let status = ev.bytes[0] & 0xF0;
        if status == 0x90 && ev.bytes[2] > 0 {
            held.push([ev.bytes[0], ev.bytes[1]]);
        } else if status == 0x80 || (status == 0x90 && ev.bytes[2] == 0) {
            held.retain(|h| !(h[0] & 0x0F == ev.bytes[0] & 0x0F && h[1] == ev.bytes[1]));
        }
        let _ = producer.push(RtMidi {
            timestamp_us: 0,
            len: ev.len,
            bytes: ev.bytes,
        });
    };

    let release_held = |producer: &mut rtrb::Producer<RtMidi>, held: &mut Vec<[u8; 2]>| {
        for [status, key] in held.drain(..) {
            let _ = producer.push(RtMidi {
                timestamp_us: 0,
                len: 3,
                bytes: [0x80 | (status & 0x0F), key, 0],
            });
        }
    };

    loop {
        // How long may we sleep before the next due event?
        let timeout = if playing && !events.is_empty() {
            let now = Instant::now();
            let due = if next_index < events.len() {
                epoch + length * cycle as u32 + events[next_index].offset
            } else {
                epoch + length * (cycle + 1) as u32
            };
            due.saturating_duration_since(now)
                .min(Duration::from_millis(20))
        } else {
            Duration::from_millis(50)
        };

        match commands.recv_timeout(timeout) {
            Ok(LooperCommand::Start {
                events: new_events,
                length: new_length,
            }) => {
                release_held(producer, &mut held);
                events = new_events;
                events.sort_by_key(|e| e.offset);
                length = new_length;
                playing = true;
                epoch = Instant::now();
                cycle = 0;
                next_index = 0;
            }
            Ok(LooperCommand::Overdub(mut extra)) => {
                // Insert without disturbing the current playback position:
                // events at or before the playhead this cycle wait for the
                // next pass.
                events.append(&mut extra);
                events.sort_by_key(|e| e.offset);
                if playing {
                    let position = Instant::now().saturating_duration_since(epoch);
                    let in_cycle_ns = position.as_nanos() % length.as_nanos().max(1);
                    next_index = events
                        .iter()
                        .position(|e| e.offset.as_nanos() > in_cycle_ns)
                        .unwrap_or(events.len());
                }
            }
            Ok(LooperCommand::Restart) => {
                if !events.is_empty() {
                    release_held(producer, &mut held);
                    playing = true;
                    epoch = Instant::now();
                    cycle = 0;
                    next_index = 0;
                }
            }
            Ok(LooperCommand::Stop) => {
                playing = false;
                release_held(producer, &mut held);
            }
            Ok(LooperCommand::Clear) => {
                playing = false;
                release_held(producer, &mut held);
                events.clear();
                length = Duration::ZERO;
            }
            Ok(LooperCommand::Ring(new_producer)) => {
                held.clear();
                *producer = new_producer;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                release_held(producer, &mut held);
                return;
            }
        }

        if !playing || events.is_empty() {
            continue;
        }
        let now = Instant::now();
        // Fire everything due.
        loop {
            if next_index >= events.len() {
                let wrap_at = epoch + length * (cycle + 1) as u32;
                if now >= wrap_at {
                    cycle += 1;
                    next_index = 0;
                } else {
                    break;
                }
            }
            let due = epoch + length * cycle as u32 + events[next_index].offset;
            if now >= due {
                let ev = events[next_index];
                push(producer, &ev, &mut held);
                next_index += 1;
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note_on() -> ([u8; 3], u8) {
        ([0x90, 60, 100], 3)
    }

    fn note_off() -> ([u8; 3], u8) {
        ([0x80, 60, 0], 3)
    }

    #[test]
    fn record_flow_produces_a_loop() {
        let mut logic = LooperLogic::new();
        let t0 = Instant::now();

        // Arm; nothing recorded until a note arrives.
        let out = logic.press(LooperButton::Record, t0);
        assert!(out.command.is_none());
        assert!(logic.captures_notes());

        let (on, len) = note_on();
        assert!(
            logic
                .note(on, len, t0 + Duration::from_millis(500))
                .is_none()
        );
        let (off, len) = note_off();
        assert!(
            logic
                .note(off, len, t0 + Duration::from_millis(900))
                .is_none()
        );

        // Close after 1s of playing (timing measured from the first note).
        let out = logic.press(LooperButton::Record, t0 + Duration::from_millis(1500));
        let Some(LooperCommand::Start { events, length }) = out.command else {
            panic!("expected Start, got {:?}", out.command);
        };
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].offset, Duration::ZERO);
        assert_eq!(events[1].offset, Duration::from_millis(400));
        assert_eq!(length, Duration::from_millis(1000));
    }

    #[test]
    fn too_short_takes_keep_recording() {
        let mut logic = LooperLogic::new();
        let t0 = Instant::now();
        logic.press(LooperButton::Record, t0);
        let (on, len) = note_on();
        logic.note(on, len, t0);
        let out = logic.press(LooperButton::Record, t0 + Duration::from_millis(50));
        assert!(out.command.is_none());
        assert!(logic.captures_notes());
    }

    #[test]
    fn overdub_toggles_and_wraps_offsets() {
        let mut logic = LooperLogic::new();
        let t0 = Instant::now();
        logic.press(LooperButton::Record, t0);
        let (on, len) = note_on();
        logic.note(on, len, t0);
        logic.press(LooperButton::Record, t0 + Duration::from_secs(1));

        // Playing, overdub off: notes are not captured.
        assert!(!logic.captures_notes());
        assert!(
            logic
                .note(on, len, t0 + Duration::from_millis(1200))
                .is_none()
        );

        // Overdub on: captured with a wrapped offset.
        logic.press(LooperButton::Record, t0 + Duration::from_millis(1300));
        let cmd = logic.note(on, len, t0 + Duration::from_millis(2500));
        let Some(LooperCommand::Overdub(events)) = cmd else {
            panic!("expected Overdub, got {cmd:?}");
        };
        // Epoch was loop close (t0+1s); 2.5s - 1s = 1.5s; mod 1s = 0.5s.
        assert_eq!(events[0].offset, Duration::from_millis(500));
    }

    #[test]
    fn stop_stops_then_clears() {
        let mut logic = LooperLogic::new();
        let t0 = Instant::now();
        logic.press(LooperButton::Record, t0);
        let (on, len) = note_on();
        logic.note(on, len, t0);
        logic.press(LooperButton::Loop, t0 + Duration::from_secs(1));

        let out = logic.press(LooperButton::Stop, t0 + Duration::from_secs(2));
        assert!(matches!(out.command, Some(LooperCommand::Stop)));
        // Play resumes from the kept loop.
        let out = logic.press(LooperButton::Play, t0 + Duration::from_secs(3));
        assert!(matches!(out.command, Some(LooperCommand::Restart)));
        logic.press(LooperButton::Stop, t0 + Duration::from_secs(4));
        let out = logic.press(LooperButton::Stop, t0 + Duration::from_secs(5));
        assert!(matches!(out.command, Some(LooperCommand::Clear)));
        assert_eq!(logic.status(), "loop: empty");
    }

    #[test]
    fn stop_discards_an_active_take() {
        let mut logic = LooperLogic::new();
        let t0 = Instant::now();
        logic.press(LooperButton::Record, t0);
        let (on, len) = note_on();
        logic.note(on, len, t0);
        let out = logic.press(LooperButton::Stop, t0 + Duration::from_secs(1));
        assert!(matches!(out.command, Some(LooperCommand::Clear)));
        assert!(!logic.captures_notes());
    }
}
