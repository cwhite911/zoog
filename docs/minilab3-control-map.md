# MiniLab 3 control map

Generated from hardware capture, not vendor documentation.

- Source: `docs/captures/arturia.cap`, recorded 2026-09-18 with
  `labctl monitor --capture` following `docs/capture-script.md`.
- Device mode: **Arturia** (power-on default program).
- Firmware version: not recorded (TODO: read from MIDI Control Center or a
  device inquiry and note it here).
- Port name: matched the default case-insensitive `"minilab"` filter
  (exact ALSA name TODO: record output of `labctl ports`).
- DAW and User mode captures: not yet recorded.

All channels are 0-based (MIDI status nibble), so "ch0" is MIDI channel 1 and
"ch9" is MIDI channel 10. Implemented in
`ControlMap::minilab3_arturia()` (`crates/lab-midi/src/event.rs`), verified by
`crates/lab-midi/tests/arturia_capture.rs` replaying the capture.

## Keyboard and strips (channel 0)

| Control | Message | Values observed |
|---|---|---|
| Keys | NoteOn 0x90 / NoteOff 0x80, ch0 | velocity varies with strike (55..127 observed); NoteOff velocity always 0 |
| Pitch strip | PitchBend ch0 | full 14-bit 0..16383, returns to center 8192 on release |
| Mod strip | CC1 ch0 | absolute 0..127 |

## Encoders (channel 0, absolute 0..127 in Arturia mode)

| Encoder | CC |
|---|---|
| 1 | 74 |
| 2 | 71 |
| 3 | 76 |
| 4 | 77 |
| 5 | 93 |
| 6 | 18 |
| 7 | 19 |
| 8 | 16 |

Absolute values: encoder 1 swept 0..127 one step at a time in the capture.
Soft takeover will be needed in Phase 4 (values jump to the knob's stored
position, and each preset change can leave stale positions).

## Faders (channel 0, absolute 0..127)

| Fader | CC |
|---|---|
| 1 | 82 |
| 2 | 83 |
| 3 | 85 |
| 4 | 17 |

## Pads (channel 9)

| Pad | Note |
|---|---|
| 1..8 | 44..51, left to right |

- Hit: NoteOn 0x99 with velocity, release: NoteOff 0x89 velocity 0.
- Pressure: **polyphonic aftertouch** (0xA9, per pad note), 0..127. Not
  channel pressure.

## Main encoder and Shift (channel 0)

| Control | CC | Behavior observed |
|---|---|---|
| Main encoder turn | 114 | relative, offset-from-64: each detent sends a pair ~5 us apart, value 64 (delta 0) then the delta value. Observed 65 (+1) turning right and 62 (-2) turning left; magnitude may scale with turn speed. Consumers ignore delta-0 messages. |
| Main encoder turn with Shift held | 112 | same relative encoding, distinct CC |
| Main encoder click | 115 | 127 press, 0 release |
| Shift | 9 | 127 press, 0 release |

## Shift + pads (mode row)

The pads' Shift row is labeled Arp, Pad, Prog, plus transport functions.
Observed in the capture:

| Action | Result |
|---|---|
| Shift+Pad1 (Arp) | no MIDI output (internal arpeggiator toggle) |
| Shift+Pad2 (Pad) | device sends SysEx `F0 00 20 6B 7F 42 02 00 40 63 00 F7` (pad bank change notification; `63 01` variant was sent at connect time) |
| Shift+Pad3 (Prog) | device sends SysEx `F0 00 20 6B 7F 42 02 00 40 62 02 F7` and **switches program**. Everything after this point in the capture is NOT Arturia mode. |

Observed after the program switch (recorded here for reference, to be
confirmed by a proper capture of that mode):

- Shift moved to CC27 (127/0).
- Shift-held pads 4..8 sent CC 105, 106, 107, 108, 109 (labels on hardware:
  Loop/Repeat, Stop, Play, Record, Tap), mostly 127 press / 0 release.

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

- DAW mode (required: display feedback only works there per PLAN.md).
- User mode.
- Exact ALSA port names from `labctl ports`.
- Firmware version.
- Whether encoders switch to relative values in DAW/User modes.
