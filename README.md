# benchlab

A native Linux sound browser and host for the Arturia MiniLab 3, written in
Rust with an iced GUI. Browse a tagged preset library from the GUI or from the
hardware, load a sound into a CLAP plugin (Surge XT first), play it with low
latency, and control macros from the encoders and faders with display and pad
feedback.

Working name. See [PLAN.md](PLAN.md) for the full plan and phase breakdown.

**This project is not affiliated with or endorsed by Arturia.** It contains no
Arturia sound engines, presets, artwork, or proprietary data. Device
communication is based on community-documented MIDI and SysEx behavior.

## Status

Phase 0: scaffold. Nothing works yet.

## Building

```
cargo build
```

Run `scripts/check-env.sh` to verify the toolchain, system packages, and a
Surge XT CLAP installation.

## Workspace

| Crate | Purpose |
|---|---|
| `lab-midi` | Device discovery, event decode, SysEx encode, `Device` trait + mock |
| `lab-engine` | CLAP hosting, audio I/O, real-time queues |
| `lab-library` | Preset index, tags, favorites, search |
| `lab-core` | App state machine, macro mapping, glue |
| `lab-gui` | iced application |
| `labctl` | Debug CLI |

## License

Pending confirmation: MIT OR Apache-2.0 proposed. See [LICENSE](LICENSE).
