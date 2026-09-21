# MiniLab 3 hardware capture script

Goal: capture every control of the MiniLab 3 in each mode so
`docs/minilab3-control-map.md` can be generated from observed data. Run one
capture per mode (Arturia, DAW, User 1). Hold Shift and tap a pad to switch
modes on the device (check the quick start guide if that binding differs).

For each mode, run:

```bash
cargo run -p labctl -- monitor --capture docs/captures/<mode>.cap
```

Then touch the controls in this order. Before each step, type the marker text
shown and press Enter so the capture file is self-describing.

| Marker to type | Then do |
|---|---|
| `mode <arturia/daw/user1>` | nothing, just labels the file |
| `key C4` | press and release the middle C key once |
| `key velocity` | press the same key softly, then hard |
| `pitch strip` | slide the pitch strip up, release, slide down, release |
| `mod strip` | slide the mod strip bottom to top to bottom |
| `encoder 1` | turn encoder 1 slowly right one full turn, then left one full turn |
| `encoder 2` .. `encoder 8` | one small right turn each |
| `fader 1` | move fader 1 bottom to top to bottom |
| `fader 2` .. `fader 4` | small move each |
| `pad 1` | tap pad 1, then press and hold with increasing pressure, release |
| `pad 2` .. `pad 8` | one tap each |
| `main encoder turn` | turn the main (browse) encoder right 3 clicks, left 3 clicks |
| `main encoder click` | click the main encoder, release |
| `shift` | press Shift alone, release |
| `shift main encoder` | hold Shift and turn the main encoder right 3 clicks |
| `shift pads` | hold Shift and look at the pads; tap nothing; type any pad labels you see as extra markers |
| `end` | stop with Ctrl-C |

Notes to record manually (as markers or in the chat):

- Firmware version if the device or the MIDI Control Center shows one.
- The exact port names printed by `labctl ports` while the device is plugged
  in.
- Which mode the device was in when the display reacted to anything.
