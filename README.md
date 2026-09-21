# Zoog

A native Linux sound browser, macro controller, and MIDI phrase looper for
the Arturia MiniLab 3, written in Rust with an iced GUI and hosting CLAP
instruments (Surge XT and Odin2 out of the box).

- Browse a tagged, searchable preset library from the GUI or entirely from
  the hardware (main encoder scrolls, Shift+turn changes category, click
  loads, with display feedback).
- The 8 encoders and 4 faders drive per-engine macro mappings with
  value-scaling soft takeover and live macro names on the device display.
- The Shift-row transport pads are a phrase looper; in Loops mode the 8
  pads become independent loop slots, each keeping the preset it was
  recorded with (per-slot engine instances, mixed live).
- Hotplug-safe: controller and audio stream recover automatically.

**Not affiliated with or endorsed by Arturia.** Device communication is
based on community-documented MIDI and SysEx behavior; see
docs/minilab3-control-map.md and docs/minilab3-sysex.md.

## Building

```
cargo build --release
```

Run `scripts/check-env.sh` to verify the toolchain, system packages, and a
CLAP Surge XT installation. `scripts/build-deb.sh` produces the Pop!_OS /
Debian package (requires cargo-deb).

Binaries: `zoog` (the app) and `labctl` (debug CLI: monitor, captures,
SysEx, plugins, params, presets, play).

## Workspace

| Crate | Purpose |
|---|---|
| `lab-midi` | Device discovery, event decode, SysEx encode, `Device` trait + mock |
| `lab-engine` | CLAP hosting, audio I/O, real-time queues, multi-instance mixing |
| `lab-library` | Preset index, tags, favorites, search |
| `lab-core` | App core: state machine, macro mapping, looper, glue |
| `lab-gui` | iced application (`zoog` binary) |
| `labctl` | Debug CLI |

## License

GPL-3.0-only. See [LICENSE](LICENSE).
