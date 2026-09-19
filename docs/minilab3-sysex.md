# MiniLab 3 SysEx messages

Community reverse-engineered; nothing here is vendor-documented. Every byte
layout below is transcribed from:

- Primary: https://gist.github.com/Janiczek/04a87c2534b9d1435a1d8159c742d260
  including its comments (notably florimondmanca's pad ID tables and the
  cdiaz/Janiczek exchange on mode limitations). Fetched 2026-09-18.
- Second source for the vendor header: `soyersoyer/sysex-controls`
  (`src/sc-midi.c`), which uses the same `F0 00 20 6B 7F 42` prefix for its
  MiniLab 3 configuration messages (a different message family from the
  feedback messages below).

Firmware version on the test device: not recorded yet (TODO).
Messages verified on hardware are marked; everything else is
transcribed-but-unverified.

Implementation: `crates/lab-midi/src/sysex.rs`, golden-byte unit tests in
the same file. Per PLAN.md Phase 2, no SysEx may be sent that is not
documented in this file.

## Header

All messages: `F0 00 20 6B 7F 42 <payload> F7`
(`00 20 6B` = Arturia manufacturer ID, `7F 42` device addressing as used by
both sources.)

## Known limitation

Display text works in **DAW mode** only; the gist comments report no known
way to write the display in Arturia mode. Pad colors have per-mode prefix
variants (below). benchlab designs around DAW mode.

## Init / handshake

```
F0 00 20 6B 7F 42 02 02 40 6A 21 F7
```

Gist: "Seems to only be needed for showing text on the display." Send once
after connecting, before display text.

## Pad and button color

```
F0 00 20 6B 7F 42 02 MM 16 ID RR GG BB F7
```

- `MM` mode prefix byte: `02` = DAW mode, `01` = Arturia mode, `00` = User
  mode (per florimondmanca's comment; his working example was
  `F0 00 20 6B 7F 42 02 01 16 04 00 7F 00 F7`).
- `RR GG BB`: 7-bit components, `00`..`7F` each.
- `ID`:

| ID | Target |
|---|---|
| 0x00..0x03 | Shift, Oct-, Hold, Oct+ buttons |
| 0x04..0x0B | pads 1..8, temporary (bank A) |
| 0x14..0x1B | pads 9..16, temporary (bank B) |
| 0x34..0x3B | pads 1..8, persistent (bank A) |
| 0x44..0x4B | pads 9..16, persistent (bank B) |

Comments: persistent colors survive bank/program changes but reset on power
cycle. User-mode colors are temporary, lost on bank/mode change and when the
pad is pressed; the idle grey is approximately `19 19 19`.

## Display text, two lines, left-aligned (the Phase 2 message)

```
F0 00 20 6B 7F 42 04 02 60 01 <S1...> 00 02 <S2...> F7
```

- `S1`, `S2`: ASCII bytes for line 1 and line 2. Line 1 renders in a smaller
  font, left-aligned.
- No documented maximum length; the display clips. Data bytes must be 7-bit
  (MIDI requirement), so text is sanitized to printable ASCII before
  encoding.

## Display text variants (documented in the gist, not implemented yet)

Centered with pictograms:

```
F0 00 20 6B 7F 42 04 02 60 1F 07 01 P1 P2 01 00 01 <S1> 00 02 <S2> 00 F7
```

Pictogram values: 0x00 none, 0x01 heart, 0x02 play, 0x03 record,
0x04 armed, 0x05 shift.

Control visualization ("info display", intended for Phase 4 knob/fader
feedback):

```
F0 00 20 6B 7F 42 04 02 60 1F CC AH VV 00 00 01 <S1> 00 02 <S2> F7
```

- `CC` control type: 0x03 knob, 0x04 fader, 0x05 pad.
- `AH` autohide: 0x00 persistent, 0x02 autohide after seconds.
- `VV` value 0x00..0x7F.

Scrolling list (intended for Phase 5 preset browsing):

```
F0 00 20 6B 7F 42 04 02 60 1F CC AH PO 00 LE 00 00 01 <S1> 00 02 <S2> 00 F7
```

- `PO` position (current index), `LE` list length, position must be less
  than length.

## Device-originated notifications (observed on hardware 2026-09-18)

Seen in `docs/captures/arturia.cap` (see `minilab3-control-map.md`):

| When | Bytes |
|---|---|
| At connect | `F0 00 20 6B 7F 42 02 00 40 63 01 F7` |
| Shift+Pad2 (pad bank toggle) | `F0 00 20 6B 7F 42 02 00 40 63 00 F7` |
| Shift+Pad3 (program switch) | `F0 00 20 6B 7F 42 02 00 40 62 02 F7` |

## Hardware verification status

| Message | Verified on hardware |
|---|---|
| Init | sent before display text, which worked (not tested in isolation) |
| Pad color (DAW prefix, temporary IDs) | **yes**, 2026-09-18 via `labctl pad`; pad ID ordering confirmed left-to-right |
| Display text two-line | **yes**, 2026-09-18 via `labctl display` |
| Pictogram / info / scrolling variants | no |

Hardware notes (2026-09-18, DAW mode, via the `Minilab3 MIDI` ALSA port):

- Pad colors set with the temporary bank A IDs **survive the pad being
  tapped** in DAW mode. The gist's "lost when pressing the pad" caveat was
  about User mode.

Update this table as `labctl display` / `labctl pad` checklists come back.
