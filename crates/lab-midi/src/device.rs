//! The [`Device`] trait: raw MIDI in and out for one controller, with a
//! midir-backed implementation and a capture-replaying mock so everything
//! runs without hardware.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};

use crate::capture::CaptureLine;
use crate::event::TimedMessage;
use crate::ports::{MidiError, name_matches};

/// Raw MIDI transport for one controller. Decoding and mapping happen above
/// this layer so mock and hardware devices go through identical code paths.
pub trait Device {
    /// Non-blocking poll for the next incoming message.
    fn try_recv(&mut self) -> Option<TimedMessage>;
    /// Blocking receive with a timeout. `None` on timeout.
    fn recv_timeout(&mut self, timeout: Duration) -> Option<TimedMessage>;
    /// Send raw MIDI bytes to the device (display and pad SysEx).
    fn send(&mut self, bytes: &[u8]) -> Result<(), MidiError>;
}

/// A hardware device connected through midir's ALSA backend.
///
/// Incoming messages are forwarded from the midir callback thread into an
/// mpsc channel; the callback does nothing else.
pub struct MidirDevice {
    input_port_name: String,
    input_port_id: String,
    // Held to keep the connection alive.
    _input: MidiInputConnection<Sender<TimedMessage>>,
    output: Option<MidiOutputConnection>,
    rx: Receiver<TimedMessage>,
}

impl MidirDevice {
    /// Open the first input port whose name matches `matcher`
    /// (case-insensitive substring), plus the matching output port when one
    /// exists. Fails if no input port matches; output is optional so
    /// monitoring works even without a writable port.
    pub fn open(client_name: &str, matcher: &str) -> Result<Self, MidiError> {
        let mut input = MidiInput::new(client_name).map_err(|e| MidiError::Init(e.to_string()))?;
        // Pass SysEx through; the MiniLab talks SysEx for feedback and init.
        input.ignore(Ignore::None);

        let in_port = input
            .ports()
            .into_iter()
            .find(|p| {
                input
                    .port_name(p)
                    .map(|n| name_matches(&n, matcher))
                    .unwrap_or(false)
            })
            .ok_or_else(|| MidiError::NoMatchingPort {
                matcher: matcher.to_string(),
            })?;
        let input_port_name = input
            .port_name(&in_port)
            .map_err(|e| MidiError::Connect(e.to_string()))?;
        let input_port_id = in_port.id();

        let (tx, rx) = channel();
        let connection = input
            .connect(
                &in_port,
                client_name,
                move |timestamp_us, bytes, tx| {
                    let _ = tx.send(TimedMessage {
                        timestamp_us,
                        bytes: bytes.to_vec(),
                    });
                },
                tx,
            )
            .map_err(|e| MidiError::Connect(e.to_string()))?;

        let output = Self::open_output(client_name, matcher)?;

        Ok(MidirDevice {
            input_port_name,
            input_port_id,
            _input: connection,
            output,
            rx,
        })
    }

    fn open_output(
        client_name: &str,
        matcher: &str,
    ) -> Result<Option<MidiOutputConnection>, MidiError> {
        let output = MidiOutput::new(client_name).map_err(|e| MidiError::Init(e.to_string()))?;
        let port = output.ports().into_iter().find(|p| {
            output
                .port_name(p)
                .map(|n| name_matches(&n, matcher))
                .unwrap_or(false)
        });
        match port {
            Some(port) => output
                .connect(&port, client_name)
                .map(Some)
                .map_err(|e| MidiError::Connect(e.to_string())),
            None => Ok(None),
        }
    }

    /// The full name of the connected input port.
    pub fn input_port_name(&self) -> &str {
        &self.input_port_name
    }

    /// The opaque id of the connected input port (changes on re-enumeration).
    pub fn input_port_id(&self) -> &str {
        &self.input_port_id
    }

    /// Whether an output port was found (needed for device feedback).
    pub fn has_output(&self) -> bool {
        self.output.is_some()
    }
}

impl Device for MidirDevice {
    fn try_recv(&mut self) -> Option<TimedMessage> {
        self.rx.try_recv().ok()
    }

    fn recv_timeout(&mut self, timeout: Duration) -> Option<TimedMessage> {
        self.rx.recv_timeout(timeout).ok()
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), MidiError> {
        match &mut self.output {
            Some(output) => output
                .send(bytes)
                .map_err(|e| MidiError::Send(e.to_string())),
            None => Err(MidiError::Disconnected),
        }
    }
}

/// A mock device that replays a capture. Sends are recorded for assertions.
#[derive(Debug, Default)]
pub struct MockDevice {
    queue: VecDeque<TimedMessage>,
    pub sent: Vec<Vec<u8>>,
}

impl MockDevice {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a mock from capture lines (comments are skipped).
    pub fn from_capture(lines: &[CaptureLine]) -> Self {
        let queue = lines
            .iter()
            .filter_map(|line| match line {
                CaptureLine::Message(msg) => Some(msg.clone()),
                CaptureLine::Comment(_) => None,
            })
            .collect();
        MockDevice {
            queue,
            sent: Vec::new(),
        }
    }

    pub fn push(&mut self, msg: TimedMessage) {
        self.queue.push_back(msg);
    }
}

impl Device for MockDevice {
    fn try_recv(&mut self) -> Option<TimedMessage> {
        self.queue.pop_front()
    }

    fn recv_timeout(&mut self, _timeout: Duration) -> Option<TimedMessage> {
        // Replay is instant; a drained queue behaves like a silent device.
        self.queue.pop_front()
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), MidiError> {
        self.sent.push(bytes.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::read_capture;
    use crate::event::{CcTarget, ControlMap, DeviceEvent, MidiMessage};

    const CAPTURE: &str = "\
# keyboard press and release
1000 90 3C 64
2000 80 3C 00
# synthetic encoder cc
3000 B0 0A 21
";

    fn drain(device: &mut impl Device, map: &ControlMap) -> Vec<DeviceEvent> {
        let mut events = Vec::new();
        while let Some(msg) = device.try_recv() {
            events.push(map.map(MidiMessage::decode(&msg.bytes)));
        }
        events
    }

    #[test]
    fn replaying_a_capture_produces_identical_events() {
        let lines = read_capture(CAPTURE.as_bytes()).unwrap();
        let map = ControlMap::new()
            .keyboard_channel(0)
            .cc(0, 10, CcTarget::Encoder(0));

        let first = drain(&mut MockDevice::from_capture(&lines), &map);
        let second = drain(&mut MockDevice::from_capture(&lines), &map);

        assert_eq!(
            first,
            vec![
                DeviceEvent::NoteOn {
                    note: 0x3C,
                    velocity: 100
                },
                DeviceEvent::NoteOff { note: 0x3C },
                DeviceEvent::Encoder {
                    index: 0,
                    value: 0x21
                },
            ]
        );
        assert_eq!(first, second);
    }

    #[test]
    fn mock_records_sends() {
        let mut mock = MockDevice::new();
        mock.send(&[0xF0, 0x00, 0xF7]).unwrap();
        assert_eq!(mock.sent, vec![vec![0xF0, 0x00, 0xF7]]);
    }
}
