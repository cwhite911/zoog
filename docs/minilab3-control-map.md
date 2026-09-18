# MiniLab 3 control map

Generated from hardware capture, not vendor documentation.

- Sources: `docs/captures/arturia.cap` and `docs/captures/daw.cap`, recorded
  2026-09-18 with `labctl monitor --capture` following
  `docs/capture-script.md`.
- Firmware version: not recorded (TODO: read from MIDI Control Center or a
  device inquiry and note it here).
- Port name: matched the default case-insensitive `"minilab"` filter
  (exact ALSA name TODO: record output of `labctl ports`).
- User mode capture: not yet recorded.

All channels are 0-based (MIDI status nibble), so "ch0" is MIDI channel 1 and
"ch9" is MIDI channel 10. Implemented in `ControlMap::minilab3_arturia()`
and `ControlMap::minilab3_daw()` (`crates/lab-midi/src/event.rs`), verified
by `crates/lab-midi/tests/capture_replay.rs` replaying both captures.

Identical in both modes: keyboard, strips, pad note/velocity/pressure
behavior, absolute encoders and faders, relative main encoder, 127/0
button CCs. Only the CC numbers differ (tables below).

## Keyboard and strips (channel 0)

| Control | Message | Values observed |
|---|---|---|
| Keys | NoteOn 0x90 / NoteOff 0x80, ch0 | velocity varies with strike (55..127 observed); NoteOff velocity always 0 |
| Pitch strip | PitchBend ch0 | full 14-bit 0..16383, returns to center 8192 on release |
| Mod strip | CC1 ch0 | absolute 0..127 |

## Encoders (channel 0, absolute 0..127 in both modes)

| Encoder | Arturia CC | DAW CC |
|---|---|---|
| 1 | 74 | 86 |
| 2 | 71 | 87 |
| 3 | 76 | 89 |
| 4 | 77 | 90 |
| 5 | 93 | 110 |
| 6 | 18 | 111 |
| 7 | 19 | 116 |
| 8 | 16 | 117 |

Absolute values in both modes: encoder 1 swept 0..127 one step at a time in
both captures. Soft takeover will be needed in Phase 4 (values jump to the
knob's stored position, and each preset change can leave stale positions).

## Faders (channel 0, absolute 0..127)

| Fader | Arturia CC | DAW CC |
|---|---|---|
| 1 | 82 | 14 |
| 2 | 83 | 15 |
| 3 | 85 | 30 |
| 4 | 17 | 31 |

## Pads (channel 9, same in both modes)

| Pad | Bank A note | Bank B note |
|---|---|---|
| 1..8 | 36..43 | 44..51 |

- Hit: NoteOn 0x99 with velocity, release: NoteOff 0x89 velocity 0.
- Pressure: **polyphonic aftertouch** (0xA9, per pad note), 0..127. Not
  channel pressure.
- The active bank is device state toggled with Shift+Pad2 and persists
  across sessions and mode switches. Inferred from the captures: the DAW
  capture used 36..43 after the bank had been toggled in the earlier Arturia
  session (which used 44..51, announced as `40 63 01` at connect). Both maps
  accept both ranges.

## Main encoder and Shift (channel 0)

| Control | Arturia CC | DAW CC | Behavior observed |
|---|---|---|---|
| Main encoder turn | 114 | 28 | relative, offset-from-64. In Arturia mode each detent sends a pair ~5 us apart, value 64 (delta 0) then the delta; in DAW mode only the delta is sent. Observed 65 (+1) turning right and 62 (-2) turning left in both modes; treat magnitude as speed-dependent and rely on sign. Consumers ignore delta-0 messages. |
| Main encoder turn with Shift held | 112 | 29 | same relative encoding, distinct CC |
| Main encoder click | 115 | 118 | 127 press, 0 release |
| Shift | 9 | 27 | 127 press, 0 release |

## DAW-mode transport (Shift-held pads 4..8, channel 0)

Observed in the tail of `arturia.cap` after the device switched into the DAW
program via Shift+Pad3 (the Shift CC there, 27, matches the DAW capture):

| Shift+pad | Hardware label | CC |
|---|---|---|
| 4 | Loop | 105 |
| 5 | Stop | 106 |
| 6 | Play | 107 |
| 7 | Record | 108 |
| 8 | Tap | 109 |

127 press / 0 release (Tap once sent 68 as its press value).

## Shift + pads (mode row, Arturia mode)

The pads' Shift row is labeled Arp, Pad, Prog, plus transport functions.
Observed in the Arturia capture:

| Action | Result |
|---|---|
| Shift+Pad1 (Arp) | no MIDI output (internal arpeggiator toggle) |
| Shift+Pad2 (Pad) | device sends SysEx `F0 00 20 6B 7F 42 02 00 40 63 00 F7` (pad bank change notification; `63 01` variant was sent at connect time) |
| Shift+Pad3 (Prog) | device sends SysEx `F0 00 20 6B 7F 42 02 00 40 62 02 F7` and **switches program**. Everything after this point in the capture is NOT Arturia mode. |

The messages after that program switch are the DAW-mode transport CCs,
tabulated above; the DAW capture independently confirmed Shift = CC27
in that mode.

## Octave buttons

The `octive up` marker recorded no messages; the capture ended at that
marker, so octave button output (likely none, an internal transpose) is
unconfirmed.

## Device-originated SysEx seen

| When | Bytes |
|---|---|
| At connect | `F0 00 20 6B 7F 42 02 00 40 63 01 F7` |
| Shift+Pad2 | `F0 00 20 6B 7F 42 02 00 40 63 00 F7` |
| Shift+Pad3 | `F0 00 20 6B 7F 42 02 00 40 62 02 F7` |

All share the Arturia header `F0 00 20 6B 7F 42` documented in the community
gist referenced by PLAN.md section 8. The `40 63 xx` / `40 62 xx` payloads
look like state notifications (pad bank, program); treat as observations, not
a protocol spec.

## Still to capture

- User mode.
- Exact ALSA port names from `labctl ports`.
- Firmware version.
- Whether the display reacted to anything during the DAW session (Phase 2
  will drive it with SysEx).
