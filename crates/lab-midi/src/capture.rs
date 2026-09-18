//! Plain-text capture format for MIDI sessions.
//!
//! One message per line: `<timestamp_us> <hex bytes>`, e.g. `123456 90 3C 64`.
//! Lines starting with `#` are comments (used as section markers during
//! scripted captures). Blank lines are ignored. Captures recorded by
//! `labctl monitor` are replayed through
//! [`MockDevice`](crate::device::MockDevice) in tests.

use std::io::{self, BufRead, Write};

use crate::event::TimedMessage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureLine {
    Message(TimedMessage),
    Comment(String),
}

pub fn write_line<W: Write>(w: &mut W, line: &CaptureLine) -> io::Result<()> {
    match line {
        CaptureLine::Comment(text) => writeln!(w, "# {text}"),
        CaptureLine::Message(msg) => {
            write!(w, "{}", msg.timestamp_us)?;
            for byte in &msg.bytes {
                write!(w, " {byte:02X}")?;
            }
            writeln!(w)
        }
    }
}

pub fn write_capture<W: Write>(w: &mut W, lines: &[CaptureLine]) -> io::Result<()> {
    for line in lines {
        write_line(w, line)?;
    }
    Ok(())
}

pub fn read_capture<R: BufRead>(r: R) -> io::Result<Vec<CaptureLine>> {
    let mut lines = Vec::new();
    for (lineno, line) in r.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(comment) = trimmed.strip_prefix('#') {
            lines.push(CaptureLine::Comment(comment.trim().to_string()));
            continue;
        }
        let mut fields = trimmed.split_whitespace();
        let timestamp_us = fields
            .next()
            .and_then(|f| f.parse::<u64>().ok())
            .ok_or_else(|| bad_line(lineno, trimmed))?;
        let bytes = fields
            .map(|f| u8::from_str_radix(f, 16))
            .collect::<Result<Vec<u8>, _>>()
            .map_err(|_| bad_line(lineno, trimmed))?;
        if bytes.is_empty() {
            return Err(bad_line(lineno, trimmed));
        }
        lines.push(CaptureLine::Message(TimedMessage {
            timestamp_us,
            bytes,
        }));
    }
    Ok(lines)
}

fn bad_line(lineno: usize, content: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("capture line {}: cannot parse {content:?}", lineno + 1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<CaptureLine> {
        vec![
            CaptureLine::Comment("keyboard C4".to_string()),
            CaptureLine::Message(TimedMessage {
                timestamp_us: 1000,
                bytes: vec![0x90, 0x3C, 0x64],
            }),
            CaptureLine::Message(TimedMessage {
                timestamp_us: 250_000,
                bytes: vec![0x80, 0x3C, 0x00],
            }),
            CaptureLine::Comment("sysex".to_string()),
            CaptureLine::Message(TimedMessage {
                timestamp_us: 300_000,
                bytes: vec![0xF0, 0x00, 0x20, 0x6B, 0xF7],
            }),
        ]
    }

    #[test]
    fn round_trip() {
        let lines = sample();
        let mut buf = Vec::new();
        write_capture(&mut buf, &lines).unwrap();
        let parsed = read_capture(buf.as_slice()).unwrap();
        assert_eq!(parsed, lines);
    }

    #[test]
    fn ignores_blank_lines_and_trims_comments() {
        let text = "\n#  marker  \n42 90 3C 7F\n\n";
        let parsed = read_capture(text.as_bytes()).unwrap();
        assert_eq!(
            parsed,
            vec![
                CaptureLine::Comment("marker".to_string()),
                CaptureLine::Message(TimedMessage {
                    timestamp_us: 42,
                    bytes: vec![0x90, 0x3C, 0x7F],
                }),
            ]
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(read_capture("not a line".as_bytes()).is_err());
        assert!(read_capture("123".as_bytes()).is_err());
        assert!(read_capture("123 ZZ".as_bytes()).is_err());
    }
}
