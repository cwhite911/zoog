# PLAN.md: Native Linux sound browser and host for the Arturia MiniLab 3
Working name: `benchlab` (rename freely; avoid Arturia trademarks in the final name).
Author: Corey T. White
Status: plan, nothing built yet.
## 1. Goal
Build a native Linux application, written in Rust with an iced GUI, that gives the MiniLab 3 the same workflow Analog Lab provides on Windows/macOS:
1. Plug in the controller and it is detected automatically.
2. Browse a tagged, searchable preset library from the GUI or from the main encoder on the hardware.
3. Load a preset and play it with low latency.
4. The 8 encoders and 4 faders control meaningful macros for the loaded sound.
5. The controller's display shows the preset name and parameter feedback, and the pads light up.
## 2. Non-goals
- No Arturia sound engines, presets, artwork, or names. Nothing proprietary is copied or converted.
- No web technology. No webview, no Tauri, no Electron. Native iced only.
- No embedding of third-party plugin editor windows. The app shows only its own macro view (this sidesteps X11/Wayland window embedding entirely).
- Not a DAW. No timeline, no recording, no mixer beyond a master volume (layers/splits are a stretch goal).
- No VST3 hosting in the initial scope. CLAP first, LV2 as a later option.
## 3. Target environment
- Pop!_OS (System76 Serval WS), COSMIC/Wayland session, PipeWire audio.
- Must also run under X11 and on other mainstream distros, but Pop!_OS is the reference machine.
- Rust stable, 2024 edition.
- System packages likely needed (verify, do not assume): `libasound2-dev`, `pkg-config`, `libjack-jackd2-dev` (optional JACK backend), plus iced's wgpu/Vulkan runtime deps.
- First sound engine: Surge XT installed as a CLAP plugin. It is open source, ships a CLAP build on Linux, has a large factory library, and has 8 built-in macros that map naturally to the 8 encoders.
## 4. Rules for Claude Code
These matter more than any individual task below.
1. **Do not fabricate.** Never invent SysEx bytes, CC numbers, crate APIs, or plugin behavior. If a value is not in this file, in a cited reference, or observed on hardware, stop and ask, or write a tool that lets the human observe it.
2. **Read before writing.** Before writing host code, read the clack host example end to end. Before writing SysEx code, read the references in section 8.
3. **Verify crate state.** Check current versions and APIs of every dependency at the time of implementation. iced's API changes between releases; pin exact versions and follow the docs for the pinned version only.
4. **Hardware checks go to the human.** You cannot see the display or hear audio. When a step needs hardware confirmation, produce a short numbered checklist (e.g., "1. Run `labctl display 'HELLO'`. 2. Confirm the top line of the display reads HELLO.") and wait for the result.
5. **Everything must run without hardware.** Put the device behind a trait with a mock implementation so tests and the GUI work with no controller attached.
6. **One phase at a time.** Finish a phase, meet its acceptance criteria, commit, then move on. Do not scaffold later phases early.
7. **Record decisions.** Append to `docs/DECISIONS.md` whenever you choose between alternatives (what, why, what was rejected).
8. **Real-time discipline.** No allocation, locking, logging, or I/O in the audio callback. Communication with the audio thread is lock-free only.
## 5. Architecture
### 5.1 Threads
| Thread | Owns | Notes |
|---|---|---|
| GUI (OS main thread) | iced application | Never blocks on audio or plugin calls |
| Plugin host thread | CLAP "main thread" duties: plugin lifecycle, param enumeration, preset load, state save/load | CLAP requires main-thread calls to come from one consistent thread. Because we never open plugin GUIs, this does not need to be the OS main thread. Verify this holds for Surge XT; if it does not, log it in DECISIONS.md and revisit. |
| Audio callback | CLAP `process`, event queues | Real-time safe only |
| MIDI input | midir callback | Decodes and forwards events, does nothing else |
### 5.2 Data flow
- MIDI in -> decode -> (a) note/pitch/mod events to the audio thread via SPSC ring buffer (`rtrb`), (b) control events (encoders, faders, pads, main encoder) to the app core via channel.
- App core -> parameter changes to the audio thread via ring buffer; parameter values back to GUI via atomics or a return ring buffer.
- App core -> device feedback (display text, pad colors) via MIDI out as SysEx.
- GUI <-> app core via messages; in iced, incoming core events arrive through a `Subscription`.
### 5.3 Workspace layout
```
benchlab/
  Cargo.toml                 # workspace
  crates/
    lab-midi/                # device discovery, event decode, SysEx encode, Device trait + mock
    lab-engine/              # CLAP hosting (clack), audio I/O (cpal), RT queues
    lab-library/             # preset index (SQLite via rusqlite), tags, favorites, search
    lab-core/                # app state machine, macro mapping, glue between the above
    lab-gui/                 # iced application, custom Canvas widgets
    labctl/                  # CLI for every phase before the GUI exists; stays as a debug tool
  mappings/                  # per-engine macro mapping files (TOML)
  docs/
    DECISIONS.md
    minilab3-control-map.md  # generated from observed hardware in Phase 1
    minilab3-sysex.md        # verified SysEx messages in Phase 2
```
Keep `lab-gui` strictly dependent on `lab-core` only. If iced ever needs to be swapped for Slint, only that crate changes.
### 5.4 Key dependencies (verify versions at implementation time)
- `midir`: MIDI I/O (ALSA backend).
- `cpal`: audio output. Start with the default ALSA host (routed through PipeWire). Add the `jack` feature later as an option for lower latency via pipewire-jack.
- `clack-host`, `clack-extensions`: CLAP hosting. These are git dependencies from `github.com/prokopyl/clack`, not on crates.io. Pin an exact `rev`.
- `rtrb`: lock-free SPSC ring buffers.
- `iced`: GUI, with the `canvas` feature for custom knobs, faders, and meters.
- `rusqlite` (bundled): preset index.
- `serde` + `toml`: config and mapping files.
- `directories`: XDG config/data/cache paths.
- `tracing`: logging (never from the audio thread).
## 6. Phases
Each phase ends with passing `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, and a commit.
### Phase 0: Scaffold
- Create the workspace and empty crates above, CI workflow (fmt, clippy, test), license placeholder `[MIT OR Apache-2.0, confirm with Corey]`, README with a non-affiliation note (not affiliated with or endorsed by Arturia).
- Write `scripts/check-env.sh` that reports: Rust version, presence of required system packages, whether a CLAP Surge XT is found in `~/.clap`, `/usr/lib/clap`, or `$CLAP_PATH`.
Acceptance: fresh clone builds; env script prints a clear pass/fail table.
### Phase 1: MIDI input and the control map
- `lab-midi`: enumerate ports, find the MiniLab 3 by name match, open in/out, reconnect on hotplug (polling is fine initially).
- `labctl monitor`: print every incoming message, raw hex and decoded.
- Define a typed event enum: `NoteOn`, `NoteOff`, `PitchBend`, `ModStrip`, `Encoder(n, value)`, `Fader(n, value)`, `Pad(n, velocity)`, `PadPressure`, `MainEncoderTurn(delta)`, `MainEncoderClick`, `Shift`, etc.
- Do not hardcode CC numbers from memory. Have the human run `labctl monitor`, touch each control in a scripted order, and generate `docs/minilab3-control-map.md` from the capture. Record which mode the device was in (Arturia, DAW, User) and whether encoders report absolute or relative values in each mode.
- `Device` trait plus `MockDevice` that replays captured sessions.
Acceptance: every physical control produces a correctly typed event; replaying a capture through `MockDevice` produces identical events in tests.
### Phase 2: Device feedback (display and pads)
- Implement SysEx encoders for: mode/init handshake, display text (two lines), pad color. Source the byte layouts from the references in section 8, transcribe them into `docs/minilab3-sysex.md` with attribution, and unit test the encoders against golden byte arrays.
- Known constraint from the community reference: display text messages work in DAW mode and have not been made to work in Arturia mode. Design around DAW mode.
- `labctl display "LINE1" "LINE2"` and `labctl pad <id> <r> <g> <b>` (RGB components are 7-bit, 0x00 to 0x7F).
- Rate-limit display updates (e.g., coalesce to at most [30] per second) so knob sweeps do not flood the device.
Acceptance: human confirms text on the display and pad colors via checklist; golden-byte tests pass; no SysEx is sent that is not documented in `docs/minilab3-sysex.md`.
### Phase 3: Headless CLAP host that makes sound
- `lab-engine`: scan CLAP search paths, load Surge XT, activate, start processing on a cpal output stream. Stereo out, configurable sample rate and buffer size (start at 48 kHz / 256 frames).
- Route note, pitch bend, mod, and pressure events from `lab-midi` into CLAP note/MIDI events through the ring buffer.
- `labctl play`: connects controller to Surge XT with its default patch.
- Offline render test (no audio device, no hardware): instantiate the plugin, push a note-on, process [100] blocks, assert output RMS is above a silence threshold; then note-off and assert the tail decays. Skip gracefully when Surge XT is not installed.
Acceptance: human plays keys and hears sound with no audible dropouts at 256 frames; offline render test passes; a callback-duration counter shows no overruns during a 5 minute session.
### Phase 4: Parameters and macros
- Enumerate CLAP params (`clap.params`), expose id, name, range, and current value.
- Mapping file format in `mappings/<engine>.toml`: each of the 8 encoders and 4 faders maps to a param id with optional range scaling and a short display label. For Surge XT, map encoders 1 to 8 to its 8 macros by default; faders to [amp envelope A/D/S/R, confirm with Corey].
- Handle encoder semantics found in Phase 1 (absolute vs relative) including soft takeover or relative accumulation so values do not jump on preset change.
- On knob move: send param event to audio thread, show `label: value` on the device display briefly, then revert to the preset name.
Acceptance: turning each encoder audibly and visibly changes the mapped parameter; display feedback appears and reverts; mapping unit tests cover scaling and relative accumulation.
### Phase 5: Preset library
- Investigate, in this order, how to enumerate and load Surge XT presets from a host: (1) CLAP preset discovery factory plus `clap.preset-load`; (2) loading patch files directly through `clap.preset-load`; (3) fallback of host-side snapshots via `clap.state` save/load. Record findings in DECISIONS.md before choosing.
- `lab-library`: SQLite index with engine, name, category/tags, author, load key (path or URI), favorite flag, last used. Full rescan and incremental rescan. Use whatever metadata discovery provides; do not invent tags.
- Hardware browsing: main encoder scrolls the current filtered list, click loads, name shown on display. Shift plus encoder changes category (confirm the Shift behavior is observable in Phase 1 first).
- `labctl presets list|search|load`.
Acceptance: the full Surge XT factory library is indexed; search by name and filter by category work from the CLI; browsing and loading from the hardware alone works with display feedback; preset switch completes without audio thread stalls.
### Phase 6: iced GUI
- Layout: left column is filters (engine, category, tags, favorites); center is the searchable preset list; right or bottom is the macro view mirroring the hardware (8 knobs, 4 faders, 8 pads) drawn with `Canvas`; status bar shows MIDI connection, audio device, sample rate, buffer size, DSP load.
- The macro view is bidirectional: hardware moves animate the on-screen controls, and dragging on-screen controls sends params (and updates the device display).
- Keyboard-first: type to search, arrows to move, Enter to load, a key to favorite.
- Settings page: audio device and buffer size, MIDI ports, plugin search paths, rescan library.
- Dark theme first. Use design tokens in one module so theming is centralized.
- The GUI must run fully against `MockDevice` and, where Surge XT is absent, a stub engine.
Acceptance: all Phase 5 functionality reachable from the GUI; GUI stays responsive (no frame hitches) while sweeping knobs and switching presets; runs on both Wayland and X11 sessions.
### Phase 7: Hardening and packaging
- Hotplug for controller and audio device loss without crashing; clear error surfaces in the GUI.
- Persist config and session (last preset, window state) under XDG paths.
- Panic safety: a plugin crash is unrecoverable in-process, so at minimum save state often and restart cleanly. Note out-of-process hosting as a future option in DECISIONS.md.
- Package as `.deb` for Pop!_OS first; evaluate Flatpak afterwards (MIDI, audio, and plugin path sandbox permissions need care).
Acceptance: unplugging and replugging the controller mid-session recovers automatically; a `.deb` installs and runs on a clean Pop!_OS VM.
### Phase 8: Stretch
- Additional engines. Candidates to check for a Linux CLAP build before committing: Odin2, Dexed, Cardinal, Six Sines. Vital is a candidate only if LV2 hosting is added (e.g., via the `livi` crate).
- Unified cross-engine tagging and a per-engine mapping file for each.
- Two-part layers and keyboard splits.
- Pads: switchable between drum notes, favorites recall, and chord triggers.
- JACK backend option for lower latency.
## 7. Testing strategy
- Pure logic (event decode, SysEx encode, mapping math, library queries) is unit tested with no hardware and no plugin.
- Engine tests use offline rendering and skip with a clear message when the plugin is absent.
- Hardware-in-the-loop tests are `#[ignore]` by default and run with `cargo test -- --ignored` by the human.
- Add a debug counter for audio callback overruns and surface it in `labctl` and the GUI status bar.
## 8. References
- MiniLab 3 SysEx (community reverse engineered; display text, pad colors, init message): https://gist.github.com/Janiczek/04a87c2534b9d1435a1d8159c742d260 . Read the comments too; they contain corrections and the DAW-mode limitation.
  - Arturia SysEx header as documented there: `F0 00 20 6B 7F 42 ... F7`.
  - Pad color as documented there: `F0 00 20 6B 7F 42 02 02 16 <ID> <RR> <GG> <BB> F7`, components 0x00 to 0x7F.
  - Take the display-text layouts directly from the gist; do not reconstruct them from memory.
- `soyersoyer/sysex-controls`: Linux (GTK/libadwaita) device configuration tool that lists MiniLab 3 support. Useful as a second source for device settings messages: https://github.com/soyersoyer/sysex-controls
- Clack (CLAP host and plugin wrappers, includes a cpal-based host example): https://github.com/prokopyl/clack
- CLAP specification and extension headers (params, state, preset-load, preset discovery): https://github.com/free-audio/clap
- Surge XT: https://surge-synthesizer.github.io/
## 9. Known risks
| Risk | Mitigation |
|---|---|
| SysEx is undocumented by the vendor and may change with firmware | Record firmware version in `docs/minilab3-sysex.md`; keep all device messages in one module; degrade gracefully to no display feedback |
| Display feedback only works in DAW mode | Design the control map around DAW mode; document how the user switches modes |
| CLAP main-thread requirement vs iced owning the OS main thread | Dedicated plugin host thread (section 5.1); verify with Surge XT early in Phase 3 |
| Host-side preset enumeration for Surge XT may be limited | Three-step investigation in Phase 5 with a state-snapshot fallback |
| iced API churn | Pin exact version; isolate all iced code in `lab-gui` |
| Clack is a git dependency with no crates.io release | Pin `rev`; vendor if it becomes necessary |
| PipeWire latency via the ALSA shim | Make buffer size configurable; JACK backend as a stretch option |
## 10. Open decisions for Corey
- [ ] Project name and license (`[MIT OR Apache-2.0]` proposed).
- [ ] Default fader mapping for Surge XT (proposed: amp envelope ADSR).
- [ ] Pad behavior in v1 (proposed: plain drum/note pads, nothing fancy).
- [ ] Whether the repo lives under a personal account or OpenPlains.
