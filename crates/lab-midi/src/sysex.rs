//! SysEx encoders for MiniLab 3 feedback (display text, pad colors, init).
//!
//! Every byte layout here is transcribed from community documentation; see
//! `docs/minilab3-sysex.md` for sources, verification status, and the rule
//! that nothing undocumented may be sent. Display text only works in DAW
//! mode; benchlab designs around that.

/// `F0` + Arturia vendor/device prefix shared by every message.
const HEADER: [u8; 6] = [0xF0, 0x00, 0x20, 0x6B, 0x7F, 0x42];
const EOX: u8 = 0xF7;

/// Init / handshake. The community gist notes it is only needed before
/// display text; send once after connecting.
pub fn init() -> Vec<u8> {
    let mut msg = HEADER.to_vec();
    msg.extend_from_slice(&[0x02, 0x02, 0x40, 0x6A, 0x21, EOX]);
    msg
}

/// Color target IDs for the pad/button color message
/// (`docs/minilab3-sysex.md`). Temporary pad colors revert when the device
/// redraws the pad; persistent ones survive bank/program changes but not a
/// power cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorTarget {
    ShiftButton,
    OctaveMinusButton,
    HoldButton,
    OctavePlusButton,
    /// Pad 0..=7 on bank A, temporary color.
    PadTemporary(u8),
    /// Pad 0..=7 on bank B, temporary color.
    PadBTemporary(u8),
    /// Pad 0..=7 on bank A, persistent color.
    PadPersistent(u8),
    /// Pad 0..=7 on bank B, persistent color.
    PadBPersistent(u8),
}

impl ColorTarget {
    fn id(self) -> u8 {
        match self {
            ColorTarget::ShiftButton => 0x00,
            ColorTarget::OctaveMinusButton => 0x01,
            ColorTarget::HoldButton => 0x02,
            ColorTarget::OctavePlusButton => 0x03,
            ColorTarget::PadTemporary(pad) => 0x04 + (pad & 0x07),
            ColorTarget::PadBTemporary(pad) => 0x14 + (pad & 0x07),
            ColorTarget::PadPersistent(pad) => 0x34 + (pad & 0x07),
            ColorTarget::PadBPersistent(pad) => 0x44 + (pad & 0x07),
        }
    }
}

/// Pad/button color in DAW mode (mode prefix byte `02`). Components are
/// 7-bit; higher bits are clamped off.
pub fn pad_color(target: ColorTarget, r: u8, g: u8, b: u8) -> Vec<u8> {
    let mut msg = HEADER.to_vec();
    msg.extend_from_slice(&[
        0x02,
        0x02,
        0x16,
        target.id(),
        r & 0x7F,
        g & 0x7F,
        b & 0x7F,
        EOX,
    ]);
    msg
}

/// Two-line left-aligned display text (DAW mode only). Text is sanitized to
/// printable ASCII (0x20..=0x7E); anything else becomes a space. No length
/// limit is documented; the display clips.
pub fn display_text(line1: &str, line2: &str) -> Vec<u8> {
    let mut msg = HEADER.to_vec();
    msg.extend_from_slice(&[0x04, 0x02, 0x60, 0x01]);
    push_ascii(&mut msg, line1);
    msg.extend_from_slice(&[0x00, 0x02]);
    push_ascii(&mut msg, line2);
    msg.push(EOX);
    msg
}

fn push_ascii(msg: &mut Vec<u8>, text: &str) {
    for ch in text.chars() {
        msg.push(if ('\x20'..='\x7E').contains(&ch) {
            ch as u8
        } else {
            b' '
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Golden bytes hand-assembled from the layouts in
    // docs/minilab3-sysex.md.

    #[test]
    fn init_golden() {
        assert_eq!(
            init(),
            [
                0xF0, 0x00, 0x20, 0x6B, 0x7F, 0x42, 0x02, 0x02, 0x40, 0x6A, 0x21, 0xF7
            ]
        );
    }

    #[test]
    fn pad_color_golden() {
        // DAW-mode analog of the community example
        // F0 00 20 6B 7F 42 02 01 16 04 00 7F 00 F7 (which used the
        // Arturia-mode prefix 01).
        assert_eq!(
            pad_color(ColorTarget::PadTemporary(0), 0x00, 0x7F, 0x00),
            [
                0xF0, 0x00, 0x20, 0x6B, 0x7F, 0x42, 0x02, 0x02, 0x16, 0x04, 0x00, 0x7F, 0x00, 0xF7
            ]
        );
    }

    #[test]
    fn pad_color_clamps_to_7bit() {
        let msg = pad_color(ColorTarget::PadTemporary(1), 0xFF, 0x80, 0x7F);
        assert_eq!(&msg[9..13], &[0x05, 0x7F, 0x00, 0x7F]);
    }

    #[test]
    fn color_target_ids() {
        assert_eq!(ColorTarget::ShiftButton.id(), 0x00);
        assert_eq!(ColorTarget::OctavePlusButton.id(), 0x03);
        assert_eq!(ColorTarget::PadTemporary(7).id(), 0x0B);
        assert_eq!(ColorTarget::PadBTemporary(0).id(), 0x14);
        assert_eq!(ColorTarget::PadPersistent(7).id(), 0x3B);
        assert_eq!(ColorTarget::PadBPersistent(3).id(), 0x47);
    }

    #[test]
    fn display_text_golden() {
        // F0 00 20 6B 7F 42 04 02 60 01 'H' 'I' 00 02 'Y' 'O' F7
        assert_eq!(
            display_text("HI", "YO"),
            [
                0xF0, 0x00, 0x20, 0x6B, 0x7F, 0x42, 0x04, 0x02, 0x60, 0x01, b'H', b'I', 0x00, 0x02,
                b'Y', b'O', 0xF7
            ]
        );
    }

    #[test]
    fn display_text_sanitizes_non_ascii() {
        let msg = display_text("A\u{e9}", "\tB");
        // 'é' and tab become spaces; every data byte stays 7-bit.
        assert_eq!(&msg[10..12], b"A ");
        assert_eq!(&msg[14..16], b" B");
        assert!(msg[1..msg.len() - 1].iter().all(|&b| b < 0x80));
    }
}
