//! The benchlab application core: owns the plugin host thread, the device
//! control thread, the audio stream, and the preset library, and exposes
//! them through command/event channels.
//!
//! Thread model (PLAN.md 5.1): the CLAP "main thread" is the dedicated host
//! thread spawned here, never the OS main thread; the GUI (or labctl) talks
//! to it exclusively through [`CoreHandle`] commands and [`CoreEvent`]s.
//! Everything runs without hardware (mock device) and without the plugin
//! (stub engine) so the GUI is always usable.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

pub use lab_engine::audio::AudioInfo;
pub use lab_engine::audio::EngineConfig;
use lab_engine::audio::{AudioStats, activate_to_stream};
use lab_engine::discovery::find_plugin;
use lab_engine::events::{ParamChange, RtMidi};
use lab_engine::host::HostThreadMessage;
use lab_engine::params::ParamDescription;
use lab_library::{Filter, Library, PresetRow};
use lab_midi::device::{Device, MidirDevice, MockDevice};
use lab_midi::event::{ControlMap, DeviceEvent, MidiMessage};
use lab_midi::ports::DEFAULT_PORT_MATCH;
use lab_midi::rate::Coalescer;
use lab_midi::sysex::{ColorTarget, display_text, init, pad_color};

use crate::browser::{BrowseItem, Browser};
use crate::looper::{
    LooperButton, LooperCommand, LooperLogic, LooperUiState, PlayerMsg, SLOTS, pad_action,
};
use crate::mapping::{Control, MacroControls, MappingFile};

pub const CLIENT_NAME: &str = "benchlab";

#[derive(Debug, Clone)]
pub struct CoreConfig {
    pub plugin_match: String,
    pub mapping_path: Option<PathBuf>,
    pub arturia_mode: bool,
    pub engine: EngineConfig,
    /// `None` uses the XDG default library location.
    pub library_path: Option<PathBuf>,
    /// Use a silent mock device instead of real hardware.
    pub mock_device: bool,
    /// Skip plugin and audio entirely (stub engine).
    pub stub_engine: bool,
    /// Reload the last-used preset at startup.
    pub restore_session: bool,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            plugin_match: "Surge XT".to_string(),
            mapping_path: None,
            arturia_mode: false,
            engine: EngineConfig::default(),
            library_path: None,
            mock_device: false,
            stub_engine: false,
            restore_session: true,
        }
    }
}

/// Path of the session state file (`~/.config/benchlab/session.toml`).
fn session_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "benchlab")
        .map(|dirs| dirs.config_dir().join("session.toml"))
}

/// Persisted session state. Kept tiny and rewritten on every preset load so
/// a crash loses at most the current selection (PLAN.md Phase 7 panic
/// safety: save often, restart cleanly).
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct SessionState {
    last_preset: Option<i64>,
}

impl SessionState {
    fn load() -> Self {
        session_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        if let Some(path) = session_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(text) = toml::to_string(self) {
                let _ = std::fs::write(path, text);
            }
        }
    }
}

/// What the eight pads do: play notes into the engine, or launch loops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadMode {
    Notes,
    Loops,
}

/// Commands into the core (GUI or CLI side).
#[derive(Debug, Clone)]
pub enum CoreCommand {
    LoadPreset(i64),
    /// A GUI control move: set a bound control to a normalized 0..=1 value.
    SetControl {
        control: Control,
        normalized: f64,
    },
    SetFavorite {
        id: i64,
        favorite: bool,
    },
    RescanLibrary,
    /// A looper transport button pressed in the GUI.
    Looper(LooperButton),
    /// A pad tapped in the GUI (slot trigger in Loops mode).
    LooperPad(u8),
    /// Switch what the pads do.
    SetPadMode(PadMode),
    Shutdown,
}

/// A preset as exposed to the frontend.
#[derive(Debug, Clone)]
pub struct PresetInfo {
    pub id: i64,
    pub name: String,
    pub category: Option<String>,
    pub author: Option<String>,
    pub favorite: bool,
}

impl From<&PresetRow> for PresetInfo {
    fn from(row: &PresetRow) -> Self {
        PresetInfo {
            id: row.id,
            name: row.name.clone(),
            category: row.category.clone(),
            author: row.creators.first().cloned(),
            favorite: row.favorite,
        }
    }
}

/// A discoverable engine (CLAP instrument) for the engine selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineInfo {
    pub id: String,
    pub name: String,
}

/// A bound control as exposed to the frontend (for the macro view).
#[derive(Debug, Clone)]
pub struct ControlInfo {
    pub control: Control,
    pub label: String,
    pub normalized: f64,
    /// False when the mapped parameter looks unassigned in the current
    /// patch (see the mapping file's `label-from-param`).
    pub active: bool,
}

/// Events out of the core.
#[derive(Debug, Clone)]
pub enum CoreEvent {
    /// First event: everything the frontend needs to draw.
    Ready {
        plugin_title: String,
        engine_running: bool,
        device_connected: bool,
        presets: Vec<PresetInfo>,
        categories: Vec<String>,
        controls: Vec<ControlInfo>,
        /// Negotiated audio stream, when the engine is running.
        audio: Option<AudioInfo>,
        /// All CLAP plugins found in the search paths.
        engines: Vec<EngineInfo>,
    },
    PresetLoaded {
        id: i64,
        name: String,
    },
    PresetLoadFailed {
        name: String,
    },
    /// A control moved (hardware or GUI); for on-screen animation.
    ControlChanged {
        control: Control,
        normalized: f64,
    },
    /// Pad pressed or released (for on-screen animation).
    Pad {
        index: u8,
        down: bool,
    },
    /// Hardware browser cursor moved to this preset id.
    BrowserSelected {
        id: i64,
    },
    /// Control labels/values/activity refreshed (after a preset load).
    ControlsRebound {
        controls: Vec<ControlInfo>,
    },
    /// The hardware controller connected or disconnected (hotplug).
    DeviceConnected(bool),
    /// Looper state changed: pad mode, focused slot, all slot states, and
    /// a status line for the focused slot.
    Looper {
        pad_mode: PadMode,
        focused: usize,
        slots: [LooperUiState; SLOTS],
        status: String,
    },
    /// Library rescan finished; fresh preset list attached.
    LibraryRescanned {
        presets: Vec<PresetInfo>,
        categories: Vec<String>,
    },
    Stats {
        callbacks: u64,
        overruns: u64,
        max_callback_ms: f64,
        min_frames: u64,
        max_frames: u64,
        stream_errors: u64,
        /// Fraction of the frame-time budget spent in process() since the
        /// previous stats event (0.0..~1.0).
        dsp_load: f64,
        /// Peak absolute output sample since the previous stats event.
        output_peak: f32,
    },
    Error(String),
}

/// Cloneable command sender into a running core.
#[derive(Debug, Clone)]
pub struct CoreHandle {
    commands: Sender<CoreCommand>,
}

impl CoreHandle {
    pub fn send(&self, command: CoreCommand) {
        let _ = self.commands.send(command);
    }
}

/// Starts the core. Returns immediately; all work happens on spawned
/// threads. Fatal startup errors arrive as [`CoreEvent::Error`] followed by
/// the event channel closing.
pub fn start(config: CoreConfig) -> (CoreHandle, Receiver<CoreEvent>) {
    let (event_tx, event_rx) = channel();
    let (command_tx, command_rx) = channel();
    thread::Builder::new()
        .name("benchlab-host".to_string())
        .spawn(move || {
            if let Err(e) = host_thread(config, command_rx, &event_tx) {
                let _ = event_tx.send(CoreEvent::Error(e.to_string()));
            }
        })
        .expect("spawning the host thread cannot fail");
    (
        CoreHandle {
            commands: command_tx,
        },
        event_rx,
    )
}

/// Messages from the control thread to the host thread.
enum FromControl {
    LoadPreset {
        id: i64,
        name: String,
        path: Option<PathBuf>,
        load_key: Option<String>,
    },
    /// A loop slot closed a take: give it its own engine instance loaded
    /// with the preset active right now.
    SlotReady { slot: usize },
    /// A loop slot was cleared: retire its engine instance.
    SlotCleared { slot: usize },
}

/// Messages from the host thread to the control thread.
enum ToControl {
    PresetLoaded {
        name: String,
        params: Vec<ParamDescription>,
    },
    PresetLoadFailed {
        name: String,
    },
    /// Set a control's underlying value directly (GUI move): update state
    /// and show feedback, without takeover.
    GuiControl {
        control: Control,
        normalized: f64,
    },
    /// Fresh ring-buffer producers after an audio stream rebuild.
    Rings {
        midi: rtrb::Producer<RtMidi>,
        params: rtrb::Producer<ParamChange>,
    },
    /// A looper transport button pressed in the GUI.
    Looper(LooperButton),
    /// A pad tapped in the GUI.
    LooperPad(u8),
    /// Switch what the pads do.
    PadMode(PadMode),
}

type AnyError = Box<dyn std::error::Error>;

fn host_thread(
    config: CoreConfig,
    commands: Receiver<CoreCommand>,
    events: &Sender<CoreEvent>,
) -> Result<(), AnyError> {
    // Plugin and engine (optional in stub mode).
    let plugin = if config.stub_engine {
        None
    } else {
        match find_plugin(&config.plugin_match) {
            Ok(plugin) => Some(plugin),
            Err(e) => {
                let _ = events.send(CoreEvent::Error(format!("{e}; running with stub engine")));
                None
            }
        }
    };

    let mut instance = None;
    let mut host_rx = None;
    let mut params = Vec::new();
    let plugin_title = match &plugin {
        Some(found) => {
            let (inst, rx) = make_instance(found)?;
            instance = Some(inst);
            host_rx = Some(rx);
            found.name.clone().unwrap_or_else(|| found.id.clone())
        }
        None => "benchlab (no engine)".to_string(),
    };
    if let Some(instance) = instance.as_mut() {
        params = lab_engine::params::list_params(instance);
    }

    // Macro mapping.
    let plugin_id = plugin.as_ref().map(|p| p.id.clone()).unwrap_or_default();
    let (macro_controls, control_infos) = match &plugin {
        Some(_) => bind_mapping(&config, &plugin_id, &params, events),
        None => (None, Vec::new()),
    };

    // Library.
    let mut library = open_library(&config)?;
    if let (Some(found), Ok(0)) = (&plugin, library.count(&plugin_id)) {
        match scan_library(&mut library, found) {
            Ok(n) => {
                let _ = n;
            }
            Err(e) => {
                let _ = events.send(CoreEvent::Error(format!("preset scan failed: {e}")));
            }
        }
    }
    let rows = library.search(&Filter {
        engine: (!plugin_id.is_empty()).then(|| plugin_id.clone()),
        ..Default::default()
    })?;
    let presets: Vec<PresetInfo> = rows.iter().map(PresetInfo::from).collect();
    let categories = if plugin_id.is_empty() {
        Vec::new()
    } else {
        library.categories(&plugin_id)?
    };
    let browse_items: Vec<BrowseItem> = rows
        .iter()
        .map(|row| BrowseItem {
            id: row.id,
            name: row.name.clone(),
            category: row.category.clone(),
            path: row.path.clone(),
            load_key: row.load_key.clone(),
        })
        .collect();

    // Device: the control thread always runs; with real hardware it owns
    // reconnect polling (hotplug and stale-session recovery).
    let device = if config.mock_device {
        DeviceSlot::Mock(MockDevice::new())
    } else {
        match MidirDevice::open(CLIENT_NAME, DEFAULT_PORT_MATCH) {
            Ok(device) => DeviceSlot::Hardware(Some(device)),
            Err(e) => {
                let _ = events.send(CoreEvent::Error(format!(
                    "no controller ({e}); waiting for hotplug"
                )));
                DeviceSlot::Hardware(None)
            }
        }
    };
    let device_connected = device.is_connected();

    // Rings and channels.
    let (midi_producer, midi_consumer) = rtrb::RingBuffer::<RtMidi>::new(1024);
    let (param_producer, param_consumer) = rtrb::RingBuffer::<ParamChange>::new(256);
    let (looper_producer, looper_consumer) = rtrb::RingBuffer::<RtMidi>::new(512);
    let looper_tx = crate::looper::spawn(looper_producer);
    let (from_control_tx, from_control_rx) = channel::<FromControl>();
    let (to_control_tx, to_control_rx) = channel::<ToControl>();

    // Control thread.
    {
        let ctx = ControlThread {
            device,
            map: if config.arturia_mode {
                ControlMap::minilab3_arturia()
            } else {
                ControlMap::minilab3_daw()
            },
            controls: macro_controls,
            browser: Browser::new(browse_items),
            midi_producer,
            param_producer,
            to_host: from_control_tx,
            from_host: to_control_rx,
            events: events.clone(),
            title: plugin_title.clone(),
            loops: (0..SLOTS).map(|_| LooperLogic::new()).collect(),
            focused: 0,
            pad_mode: PadMode::Notes,
            looper_tx: looper_tx.clone(),
        };
        thread::Builder::new()
            .name("benchlab-control".to_string())
            .spawn(move || ctx.run())?;
    }

    // Audio.
    let mut stats: Option<std::sync::Arc<AudioStats>> = None;
    let mut audio_info = None;
    let mut _stream = None;
    let mut audio_retry_at: Option<Instant> = None;
    let mut slot_channels: Option<lab_engine::audio::SlotChannels> = None;
    let mut active_cfg: Option<(f64, u32)> = None;
    let mut current_preset: Option<(Option<PathBuf>, Option<String>)> = None;
    let mut slot_instances: Vec<
        Option<(
            lab_engine::clack_host::prelude::PluginInstance<lab_engine::host::BenchHost>,
            Receiver<HostThreadMessage>,
        )>,
    > = (0..SLOTS).map(|_| None).collect();
    if let Some(instance) = instance.as_mut() {
        match activate_to_stream(
            instance,
            midi_consumer,
            Some(looper_consumer),
            Some(param_consumer),
            config.engine,
        ) {
            Ok((stream, audio_stats, info, channels)) => {
                active_cfg = Some((info.sample_rate as f64, info.buffer_frames.max(1024)));
                _stream = Some(stream);
                stats = Some(audio_stats);
                audio_info = Some(info);
                slot_channels = Some(channels);
            }
            Err(e) => {
                let _ = events.send(CoreEvent::Error(format!("audio failed: {e}")));
                audio_retry_at = Some(Instant::now() + Duration::from_secs(5));
            }
        }
    }

    let engines = lab_engine::discovery::scan_all()
        .into_iter()
        .map(|found| EngineInfo {
            name: found.name.clone().unwrap_or_else(|| found.id.clone()),
            id: found.id,
        })
        .collect();
    let _ = events.send(CoreEvent::Ready {
        plugin_title: plugin_title.clone(),
        engine_running: stats.is_some(),
        device_connected,
        presets,
        categories,
        controls: control_infos,
        audio: audio_info,
        engines,
    });

    // Session restore: reload the last-used preset.
    if config.restore_session
        && let Some(id) = SessionState::load().last_preset
        && let Ok(Some(row)) = library.get(id)
    {
        current_preset = Some((row.path.clone(), row.load_key.clone()));
        do_load_preset(
            instance.as_mut(),
            &library,
            events,
            &to_control_tx,
            row.id,
            row.name,
            row.path,
            row.load_key,
        );
    }

    // Main service loop.
    let timers = instance
        .as_mut()
        .and_then(|i| i.access_handler(|h| h.timer_support().map(|ext| (h.timers.clone(), ext))));
    let mut last_stats = Instant::now();
    let mut last_busy_budget = (0u64, 0u64);
    let mut last_callbacks = 0u64;
    let mut stalled_for = 0u32;
    let mut rebuild_failures = 0u32;
    loop {
        if let Some(rx) = &host_rx
            && let Ok(HostThreadMessage::RunOnMainThread) =
                rx.recv_timeout(Duration::from_millis(20))
            && let Some(instance) = instance.as_mut()
        {
            instance.call_on_main_thread_callback();
        }
        if host_rx.is_none() {
            thread::sleep(Duration::from_millis(20));
        }
        if let (Some((timers, timer_ext)), Some(instance)) = (&timers, instance.as_mut()) {
            timers.tick(timer_ext, &instance.plugin_handle());
        }

        // Requests from the control thread.
        while let Ok(msg) = from_control_rx.try_recv() {
            match msg {
                FromControl::LoadPreset {
                    id,
                    name,
                    path,
                    load_key,
                } => {
                    current_preset = Some((path.clone(), load_key.clone()));
                    do_load_preset(
                        instance.as_mut(),
                        &library,
                        events,
                        &to_control_tx,
                        id,
                        name,
                        path,
                        load_key,
                    );
                }
                FromControl::SlotReady { slot } => {
                    if let (Some(found), Some(channels), Some(cfg)) =
                        (&plugin, slot_channels.as_mut(), active_cfg)
                    {
                        match build_slot_engine(found, &current_preset, cfg, slot) {
                            Ok((inst, rx, engine, producer)) => {
                                if channels.install.push(engine).is_ok() {
                                    let _ = looper_tx.send(PlayerMsg::SlotRing(slot, producer));
                                    slot_instances[slot] = Some((inst, rx));
                                } else {
                                    let _ = events.send(CoreEvent::Error(
                                        "slot engine install queue full".to_string(),
                                    ));
                                }
                            }
                            Err(e) => {
                                let _ = events.send(CoreEvent::Error(format!(
                                    "loop slot engine failed: {e}; slot plays the live preset"
                                )));
                            }
                        }
                    }
                }
                FromControl::SlotCleared { slot } => {
                    if let Some(channels) = slot_channels.as_mut() {
                        let _ = channels.remove.push(slot);
                    }
                    let _ = looper_tx.send(PlayerMsg::SlotRingClear(slot));
                }
            }
        }

        // Retired slot processors come back for main-thread deactivation.
        if let Some(channels) = slot_channels.as_mut() {
            while let Ok((slot, stopped)) = channels.retired.pop() {
                if let Some((mut inst, _rx)) = slot_instances.get_mut(slot).and_then(Option::take) {
                    inst.deactivate(stopped);
                }
            }
        }

        // Slot instances get their main-thread callbacks too.
        for entry in slot_instances.iter_mut().flatten() {
            while let Ok(HostThreadMessage::RunOnMainThread) = entry.1.try_recv() {
                entry.0.call_on_main_thread_callback();
            }
        }

        // Frontend commands.
        loop {
            match commands.try_recv() {
                Ok(CoreCommand::Shutdown) => return Ok(()),
                Ok(CoreCommand::LoadPreset(id)) => {
                    if let Ok(Some(row)) = library.get(id) {
                        current_preset = Some((row.path.clone(), row.load_key.clone()));
                        do_load_preset(
                            instance.as_mut(),
                            &library,
                            events,
                            &to_control_tx,
                            row.id,
                            row.name,
                            row.path,
                            row.load_key,
                        );
                    }
                }
                Ok(CoreCommand::SetControl {
                    control,
                    normalized,
                }) => {
                    let _ = to_control_tx.send(ToControl::GuiControl {
                        control,
                        normalized,
                    });
                }
                Ok(CoreCommand::SetFavorite { id, favorite }) => {
                    let _ = library.set_favorite(id, favorite);
                }
                Ok(CoreCommand::Looper(button)) => {
                    let _ = to_control_tx.send(ToControl::Looper(button));
                }
                Ok(CoreCommand::LooperPad(pad)) => {
                    let _ = to_control_tx.send(ToControl::LooperPad(pad));
                }
                Ok(CoreCommand::SetPadMode(mode)) => {
                    let _ = to_control_tx.send(ToControl::PadMode(mode));
                }
                Ok(CoreCommand::RescanLibrary) => {
                    if let Some(found) = &plugin {
                        match scan_library(&mut library, found) {
                            Ok(_) => {
                                if let Ok(rows) = library.search(&Filter {
                                    engine: Some(plugin_id.clone()),
                                    ..Default::default()
                                }) {
                                    let _ = events.send(CoreEvent::LibraryRescanned {
                                        presets: rows.iter().map(PresetInfo::from).collect(),
                                        categories: library
                                            .categories(&plugin_id)
                                            .unwrap_or_default(),
                                    });
                                }
                            }
                            Err(e) => {
                                let _ =
                                    events.send(CoreEvent::Error(format!("rescan failed: {e}")));
                            }
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }

        if let Some(stats) = &stats
            && last_stats.elapsed() >= Duration::from_secs(1)
        {
            use std::sync::atomic::Ordering::Relaxed;
            last_stats = Instant::now();
            let busy = stats.busy_ns.load(Relaxed);
            let budget = stats.budget_ns.load(Relaxed);
            let (last_busy, last_budget) = last_busy_budget;
            last_busy_budget = (busy, budget);
            let budget_delta = budget.saturating_sub(last_budget);
            let dsp_load = if budget_delta > 0 {
                busy.saturating_sub(last_busy) as f64 / budget_delta as f64
            } else {
                0.0
            };
            // Stall watchdog: a stream whose callback counter stops
            // advancing (or never starts) has died underneath us (seen
            // once on PipeWire, e.g. on output-device loss).
            let callbacks_now = stats.callbacks.load(Relaxed);
            if callbacks_now == last_callbacks {
                stalled_for += 1;
            } else {
                stalled_for = 0;
                rebuild_failures = 0;
            }
            last_callbacks = callbacks_now;
            let output_peak = f32::from_bits(
                stats
                    .peak_bits
                    .swap(0, std::sync::atomic::Ordering::Relaxed),
            );
            let _ = events.send(CoreEvent::Stats {
                callbacks: callbacks_now,
                overruns: stats.overruns.load(Relaxed),
                max_callback_ms: stats.max_callback_ns.load(Relaxed) as f64 / 1e6,
                min_frames: stats.min_frames.load(Relaxed),
                max_frames: stats.max_frames.load(Relaxed),
                stream_errors: stats.stream_errors.load(Relaxed),
                dsp_load,
                output_peak,
            });
        }

        // Rebuild a stalled stream: drop it (which drops the audio
        // processor), deactivate the plugin, and activate onto fresh ring
        // buffers whose producers are handed to the control thread.
        let retry_due = stats.is_none() && audio_retry_at.is_some_and(|t| Instant::now() >= t);
        if (stalled_for >= 3 || retry_due) && rebuild_failures < 5 {
            audio_retry_at = None;
            stalled_for = 0;
            last_callbacks = 0;
            last_busy_budget = (0, 0);
            let _ = events.send(CoreEvent::Error(
                "audio stream stalled; rebuilding".to_string(),
            ));
            _stream = None;
            stats = None;
            if let Some(instance) = instance.as_mut() {
                if instance.is_active()
                    && let Err(e) = instance.try_deactivate()
                {
                    let _ = events.send(CoreEvent::Error(format!(
                        "plugin deactivation failed: {e}; restart benchlab"
                    )));
                    continue;
                }
                let (midi_producer, midi_consumer) = rtrb::RingBuffer::<RtMidi>::new(1024);
                let (param_producer, param_consumer) = rtrb::RingBuffer::<ParamChange>::new(256);
                let (looper_producer, looper_consumer) = rtrb::RingBuffer::<RtMidi>::new(512);
                let _ = looper_tx.send(PlayerMsg::Ring(looper_producer));
                // Slot engines died with the old stream: deactivate their
                // instances; loops fall back to the live preset until
                // re-recorded.
                for slot in 0..SLOTS {
                    if let Some((mut inst, _rx)) =
                        slot_instances.get_mut(slot).and_then(Option::take)
                    {
                        let _ = inst.try_deactivate();
                        let _ = looper_tx.send(PlayerMsg::SlotRingClear(slot));
                    }
                }
                slot_channels = None;
                match activate_to_stream(
                    instance,
                    midi_consumer,
                    Some(looper_consumer),
                    Some(param_consumer),
                    config.engine,
                ) {
                    Ok((stream, audio_stats, _info, channels)) => {
                        _stream = Some(stream);
                        stats = Some(audio_stats);
                        slot_channels = Some(channels);
                        let _ = to_control_tx.send(ToControl::Rings {
                            midi: midi_producer,
                            params: param_producer,
                        });
                        let _ = events.send(CoreEvent::Error("audio stream rebuilt".to_string()));
                    }
                    Err(e) => {
                        rebuild_failures += 1;
                        if rebuild_failures < 5 {
                            audio_retry_at = Some(Instant::now() + Duration::from_secs(5));
                        }
                        let _ = events.send(CoreEvent::Error(format!(
                            "audio stream rebuild failed: {e}{}",
                            if rebuild_failures >= 5 {
                                "; giving up, restart benchlab"
                            } else {
                                "; retrying in 5 s"
                            }
                        )));
                    }
                }
            }
        }
    }
}

fn make_instance(
    plugin: &lab_engine::discovery::FoundPlugin,
) -> Result<
    (
        lab_engine::clack_host::prelude::PluginInstance<lab_engine::host::BenchHost>,
        Receiver<HostThreadMessage>,
    ),
    AnyError,
> {
    use lab_engine::host::{BenchHost, BenchHostMainThread, BenchHostShared, host_info};
    let (host_tx, host_rx) = channel();
    let plugin_id = std::ffi::CString::new(plugin.id.as_str())?;
    let instance = lab_engine::clack_host::prelude::PluginInstance::<BenchHost>::new(
        |_| BenchHostShared::new(host_tx),
        |_| BenchHostMainThread::new(),
        &plugin.entry,
        &plugin_id,
        &host_info(),
    )?;
    Ok((instance, host_rx))
}

/// Builds a dedicated engine instance for a loop slot, loaded with the
/// preset that was active when the loop was recorded.
#[allow(clippy::type_complexity)]
fn build_slot_engine(
    found: &lab_engine::discovery::FoundPlugin,
    preset: &Option<(Option<PathBuf>, Option<String>)>,
    cfg: (f64, u32),
    slot: usize,
) -> Result<
    (
        lab_engine::clack_host::prelude::PluginInstance<lab_engine::host::BenchHost>,
        Receiver<HostThreadMessage>,
        lab_engine::audio::SlotEngine,
        rtrb::Producer<RtMidi>,
    ),
    AnyError,
> {
    use lab_engine::audio::{PluginBuffers, SlotEngine, query_port_layout};
    use lab_engine::clack_host::prelude::PluginAudioConfiguration;
    use lab_engine::events::{EventCollector, NotePortConfig, find_main_note_port};

    let (mut instance, host_rx) = make_instance(found)?;
    if let Some((path, load_key)) = preset {
        // Best effort: a failed preset load leaves the slot on the
        // engine's default sound rather than failing the slot.
        let _ =
            lab_engine::presets::load_preset(&mut instance, path.as_deref(), load_key.as_deref());
    }
    let layout_in = query_port_layout(&mut instance, true);
    let layout_out = query_port_layout(&mut instance, false);
    let note_port = find_main_note_port(&mut instance).unwrap_or(NotePortConfig {
        port_index: 0,
        prefers_midi: true,
    });
    let processor = instance
        .activate(
            |_, _| (),
            PluginAudioConfiguration {
                sample_rate: cfg.0,
                min_frames_count: 1,
                max_frames_count: cfg.1,
            },
        )?
        .start_processing()?;
    let (producer, consumer) = rtrb::RingBuffer::<RtMidi>::new(256);
    let engine = SlotEngine {
        slot,
        processor,
        buffers: PluginBuffers::new(layout_in, layout_out, cfg.1 as usize),
        events: EventCollector::new(cfg.0 as u64, note_port),
        midi: consumer,
    };
    Ok((instance, host_rx, engine, producer))
}

fn bind_mapping(
    config: &CoreConfig,
    plugin_id: &str,
    params: &[ParamDescription],
    events: &Sender<CoreEvent>,
) -> (Option<MacroControls>, Vec<ControlInfo>) {
    // Explicit path wins; otherwise scan the mapping directories
    // (working directory for development, user config, system install)
    // for a file whose plugin-id matches the loaded engine.
    let path = config
        .mapping_path
        .clone()
        .or_else(|| find_mapping_for(plugin_id));
    let Some(path) = path else {
        let _ = events.send(CoreEvent::Error(format!(
            "no mapping file for {plugin_id}; encoders and faders inactive"
        )));
        return (None, Vec::new());
    };
    if !path.exists() {
        let _ = events.send(CoreEvent::Error(format!(
            "no mapping file at {}; encoders and faders inactive",
            path.display()
        )));
        return (None, Vec::new());
    }
    let file = match MappingFile::load(&path) {
        Ok(file) if file.plugin_id == plugin_id => file,
        Ok(file) => {
            let _ = events.send(CoreEvent::Error(format!(
                "mapping {} is for {}; controls inactive",
                path.display(),
                file.plugin_id
            )));
            return (None, Vec::new());
        }
        Err(e) => {
            let _ = events.send(CoreEvent::Error(format!("mapping load failed: {e}")));
            return (None, Vec::new());
        }
    };
    match MacroControls::bind(&file, plugin_id, params) {
        Ok(controls) => {
            let infos = control_infos(&controls);
            (Some(controls), infos)
        }
        Err(e) => {
            let _ = events.send(CoreEvent::Error(format!("mapping bind failed: {e}")));
            (None, Vec::new())
        }
    }
}

/// Projects the current bindings into frontend control infos.
fn control_infos(controls: &MacroControls) -> Vec<ControlInfo> {
    controls
        .bindings()
        .map(|binding| ControlInfo {
            control: binding.control,
            label: binding.label.clone(),
            normalized: controls.normalized_value(binding),
            active: binding.active,
        })
        .collect()
}

/// Finds the mapping file whose `plugin-id` matches, searching the
/// development, user, and system mapping directories in that order.
fn find_mapping_for(plugin_id: &str) -> Option<PathBuf> {
    let mut dirs = vec![PathBuf::from("mappings")];
    if let Some(project) = directories::ProjectDirs::from("", "", "benchlab") {
        dirs.push(project.config_dir().join("mappings"));
    }
    dirs.push(PathBuf::from("/usr/share/benchlab/mappings"));

    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "toml"))
            .collect();
        paths.sort();
        for path in paths {
            if let Ok(file) = MappingFile::load(&path)
                && file.plugin_id == plugin_id
            {
                return Some(path);
            }
        }
    }
    None
}

fn open_library(config: &CoreConfig) -> Result<Library, AnyError> {
    let path = match &config.library_path {
        Some(path) => path.clone(),
        None => Library::default_path().ok_or("cannot determine XDG data directory")?,
    };
    Ok(Library::open(&path)?)
}

fn scan_library(
    library: &mut Library,
    plugin: &lab_engine::discovery::FoundPlugin,
) -> Result<usize, AnyError> {
    let discovered =
        lab_engine::presets::discover_presets(&plugin.entry, &lab_engine::host::host_info())?;
    let imports: Vec<lab_library::ImportPreset> = discovered
        .into_iter()
        .filter(|p| p.plugin_ids.is_empty() || p.plugin_ids.contains(&plugin.id))
        .map(|preset| {
            let category = preset
                .path
                .as_deref()
                .and_then(std::path::Path::parent)
                .and_then(std::path::Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
                .or_else(|| preset.features.first().cloned());
            let mtime = preset.path.as_deref().and_then(|p| {
                std::fs::metadata(p)
                    .ok()?
                    .modified()
                    .ok()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_secs() as i64)
            });
            lab_library::ImportPreset {
                name: preset.name,
                path: preset.path,
                load_key: preset.load_key,
                category,
                features: preset.features,
                creators: preset.creators,
                description: preset.description,
                is_factory: preset.is_factory,
                mtime,
            }
        })
        .collect();
    Ok(library.rescan(&plugin.id, &imports)?)
}

#[allow(clippy::too_many_arguments)]
fn do_load_preset(
    instance: Option<
        &mut lab_engine::clack_host::prelude::PluginInstance<lab_engine::host::BenchHost>,
    >,
    library: &Library,
    events: &Sender<CoreEvent>,
    to_control: &Sender<ToControl>,
    id: i64,
    name: String,
    path: Option<PathBuf>,
    load_key: Option<String>,
) {
    let Some(instance) = instance else {
        let _ = events.send(CoreEvent::PresetLoadFailed { name });
        return;
    };
    match lab_engine::presets::load_preset(instance, path.as_deref(), load_key.as_deref()) {
        Ok(()) => {
            let params = lab_engine::params::list_params(instance);
            let now_s = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let _ = library.touch_last_used(id, now_s);
            SessionState {
                last_preset: Some(id),
            }
            .save();
            let _ = to_control.send(ToControl::PresetLoaded {
                name: name.clone(),
                params,
            });
            let _ = events.send(CoreEvent::PresetLoaded { id, name });
        }
        Err(e) => {
            let _ = events.send(CoreEvent::Error(format!(
                "preset load failed for {name:?}: {e}"
            )));
            let _ = to_control.send(ToControl::PresetLoadFailed { name: name.clone() });
            let _ = events.send(CoreEvent::PresetLoadFailed { name });
        }
    }
}

/// The control thread's device: a fixed mock, or hardware that may be
/// absent and is reconnected by polling.
enum DeviceSlot {
    Mock(MockDevice),
    Hardware(Option<MidirDevice>),
}

impl DeviceSlot {
    fn is_connected(&self) -> bool {
        match self {
            DeviceSlot::Mock(_) => true,
            DeviceSlot::Hardware(device) => device.is_some(),
        }
    }

    fn device_mut(&mut self) -> Option<&mut (dyn Device + Send)> {
        match self {
            DeviceSlot::Mock(mock) => Some(mock),
            DeviceSlot::Hardware(Some(device)) => Some(device),
            DeviceSlot::Hardware(None) => None,
        }
    }
}

/// State owned by the device control thread.
struct ControlThread {
    device: DeviceSlot,
    map: ControlMap,
    controls: Option<MacroControls>,
    browser: Browser,
    midi_producer: rtrb::Producer<RtMidi>,
    param_producer: rtrb::Producer<ParamChange>,
    to_host: Sender<FromControl>,
    from_host: Receiver<ToControl>,
    events: Sender<CoreEvent>,
    title: String,
    loops: Vec<LooperLogic>,
    focused: usize,
    pad_mode: PadMode,
    looper_tx: Sender<PlayerMsg>,
}

impl ControlThread {
    fn slot_states(&self) -> [LooperUiState; SLOTS] {
        std::array::from_fn(|i| self.loops[i].ui_state())
    }

    fn slot_status(&self) -> String {
        format!(
            "loop {} {}",
            self.focused + 1,
            self.loops[self.focused]
                .status()
                .trim_start_matches("loop: ")
        )
    }

    fn emit_looper(&mut self, status: String) {
        let _ = self.events.send(CoreEvent::Looper {
            pad_mode: self.pad_mode,
            focused: self.focused,
            slots: self.slot_states(),
            status: status.clone(),
        });
        let line2 = status.trim_start_matches("loop ").to_string();
        self.show("Loop", &line2);
        self.paint_pads();
    }

    /// At most one slot may capture notes; cancel arming/overdub elsewhere.
    fn ensure_exclusive_capture(&mut self, except: usize, now: Instant) {
        for index in 0..SLOTS {
            if index == except || !self.loops[index].captures_notes() {
                continue;
            }
            match self.loops[index].ui_state() {
                // Cancel an armed slot; switch overdub off.
                LooperUiState::Armed | LooperUiState::Overdub => {
                    let output = self.loops[index].press(LooperButton::Record, now);
                    if let Some(command) = output.command {
                        let _ = self.looper_tx.send(PlayerMsg::Slot(index, command));
                    }
                }
                // A slot mid-recording keeps recording; new arms are
                // refused in press_slot instead.
                _ => {}
            }
        }
    }

    /// Drives the focused slot's state machine from a transport button.
    fn handle_looper(&mut self, button: LooperButton, now: Instant) {
        self.press_slot(self.focused, button, now);
    }

    fn press_slot(&mut self, slot: usize, button: LooperButton, now: Instant) {
        self.focused = slot;
        // Refuse arming a second recorder; close the first take first.
        if button == LooperButton::Record
            && matches!(
                self.loops[slot].ui_state(),
                LooperUiState::Empty | LooperUiState::Stopped
            )
            && let Some(busy) = (0..SLOTS)
                .find(|&i| i != slot && self.loops[i].ui_state() == LooperUiState::Recording)
        {
            self.emit_looper(format!("loop {} still recording", busy + 1));
            return;
        }
        let output = self.loops[slot].press(button, now);
        if matches!(
            self.loops[slot].ui_state(),
            LooperUiState::Armed | LooperUiState::Overdub
        ) {
            self.ensure_exclusive_capture(slot, now);
        }
        if let Some(command) = output.command {
            let started = matches!(command, LooperCommand::Start { .. });
            let cleared = matches!(command, LooperCommand::Clear);
            let _ = self.looper_tx.send(PlayerMsg::Slot(slot, command));
            if started {
                let _ = self.to_host.send(FromControl::SlotReady { slot });
            }
            if cleared {
                let _ = self.to_host.send(FromControl::SlotCleared { slot });
            }
        }
        if output.status.is_some() {
            self.emit_looper(self.slot_status());
        }
    }

    /// A pad tap in Loops mode: one button per slot, action depending on
    /// the slot's state.
    fn tap_slot(&mut self, slot: usize, now: Instant) {
        if slot >= SLOTS {
            return;
        }
        let action = pad_action(self.loops[slot].ui_state());
        self.press_slot(slot, action, now);
    }

    fn set_pad_mode(&mut self, mode: PadMode) {
        if self.pad_mode != mode {
            self.pad_mode = mode;
            self.emit_looper(match mode {
                PadMode::Loops => "pads: loop slots".to_string(),
                PadMode::Notes => "pads: notes".to_string(),
            });
        }
    }

    /// Pad backlights: per-slot loop state in Loops mode, a single wash of
    /// the focused slot's state in Notes mode.
    fn paint_pads(&mut self) {
        let color = |state: LooperUiState| match state {
            LooperUiState::Empty => (6, 28, 44),
            LooperUiState::Armed => (70, 12, 12),
            LooperUiState::Recording => (110, 6, 6),
            LooperUiState::Overdub => (110, 50, 6),
            LooperUiState::Playing => (10, 70, 22),
            LooperUiState::Stopped => (55, 40, 8),
        };
        let states = self.slot_states();
        let mode = self.pad_mode;
        let focused = self.focused;
        if let Some(device) = self.device.device_mut() {
            for pad in 0..SLOTS {
                let (r, g, b) = match mode {
                    PadMode::Loops => color(states[pad]),
                    PadMode::Notes => color(states[focused]),
                };
                let _ = device.send(&pad_color(ColorTarget::PadTemporary(pad as u8), r, g, b));
            }
        }
    }

    fn show(&mut self, line1: &str, line2: &str) {
        if let Some(device) = self.device.device_mut() {
            let _ = device.send(&display_text(line1, line2));
        }
    }

    /// Sends the init handshake, benchlab's pad wash, and the current
    /// title to a (re)connected device.
    fn greet_device(&mut self) {
        if let Some(device) = self.device.device_mut() {
            let _ = device.send(&init());
        }
        // Pad wash marks "benchlab connected" and doubles as the looper
        // state light (temporary colors survive taps in DAW mode).
        self.paint_pads();
        let title = self.title.clone();
        self.show(&title, "");
    }

    /// Hotplug maintenance for hardware devices. Detects disappearance AND
    /// silent re-enumeration (same name, new port id: the stale-session
    /// failure observed on hardware), reconnects when the port is back.
    /// Returns true if the connection state changed.
    fn maintain_hardware(&mut self) -> bool {
        let DeviceSlot::Hardware(slot) = &mut self.device else {
            return false;
        };
        let current_id =
            lab_midi::ports::find_input_port_id(CLIENT_NAME, DEFAULT_PORT_MATCH).unwrap_or(None);
        match (slot.as_ref(), current_id) {
            // Connected and the port id still matches: healthy.
            (Some(device), Some(id)) if device.input_port_id() == id => false,
            // Gone, or re-enumerated under a new id: drop and maybe reopen.
            (Some(_), current) => {
                *slot = None;
                if current.is_some()
                    && let Ok(device) = MidirDevice::open(CLIENT_NAME, DEFAULT_PORT_MATCH)
                {
                    *slot = Some(device);
                    let _ = self.events.send(CoreEvent::DeviceConnected(true));
                    self.greet_device();
                } else {
                    let _ = self.events.send(CoreEvent::DeviceConnected(false));
                }
                true
            }
            // Absent and still absent.
            (None, None) => false,
            // Absent but a port appeared: connect.
            (None, Some(_)) => match MidirDevice::open(CLIENT_NAME, DEFAULT_PORT_MATCH) {
                Ok(device) => {
                    *slot = Some(device);
                    let _ = self.events.send(CoreEvent::DeviceConnected(true));
                    self.greet_device();
                    true
                }
                Err(_) => false,
            },
        }
    }

    fn run(mut self) {
        let mut coalescer: Coalescer<(String, String)> = Coalescer::new(Duration::from_millis(33));
        let mut revert_at: Option<Instant> = None;
        let mut last_hotplug_check = Instant::now();
        let mut last_looper_status = self.slot_status();
        let mut last_looper_states = self.slot_states();
        let mut last_looper_refresh = Instant::now();

        self.greet_device();

        loop {
            let now = Instant::now();
            // The presence probe opens a fresh ALSA client, so throttle it.
            if now.duration_since(last_hotplug_check) >= Duration::from_secs(2) {
                last_hotplug_check = now;
                self.maintain_hardware();
            }
            let received = match self.device.device_mut() {
                Some(device) => device.recv_timeout(Duration::from_millis(50)),
                None => {
                    thread::sleep(Duration::from_millis(200));
                    None
                }
            };
            if let Some(msg) = received {
                match self.map.map(MidiMessage::decode(&msg.bytes)) {
                    event @ (DeviceEvent::NoteOn { .. }
                    | DeviceEvent::NoteOff { .. }
                    | DeviceEvent::PitchBend { .. }
                    | DeviceEvent::ModStrip { .. }
                    | DeviceEvent::PadDown { .. }
                    | DeviceEvent::PadUp { .. }
                    | DeviceEvent::PadPressure { .. }) => {
                        let is_pad = matches!(
                            event,
                            DeviceEvent::PadDown { .. }
                                | DeviceEvent::PadUp { .. }
                                | DeviceEvent::PadPressure { .. }
                        );
                        // In Loops mode the pads are slot triggers, not
                        // notes: consume them here.
                        if is_pad && self.pad_mode == PadMode::Loops {
                            if let DeviceEvent::PadDown { index, .. } = event {
                                self.tap_slot(index as usize, now);
                            }
                        } else if let Some(rt) = RtMidi::from_bytes(msg.timestamp_us, &msg.bytes) {
                            // Looper capture: note events only.
                            if matches!(
                                event,
                                DeviceEvent::NoteOn { .. }
                                    | DeviceEvent::NoteOff { .. }
                                    | DeviceEvent::PadDown { .. }
                                    | DeviceEvent::PadUp { .. }
                            ) && let Some(capturing) =
                                (0..SLOTS).find(|&i| self.loops[i].captures_notes())
                                && let Some(command) =
                                    self.loops[capturing].note(rt.bytes, rt.len, now)
                            {
                                let _ = self.looper_tx.send(PlayerMsg::Slot(capturing, command));
                            }
                            let _ = self.midi_producer.push(rt);
                        }
                        match event {
                            DeviceEvent::PadDown { index, .. } => {
                                let _ = self.events.send(CoreEvent::Pad { index, down: true });
                            }
                            DeviceEvent::PadUp { index } => {
                                let _ = self.events.send(CoreEvent::Pad { index, down: false });
                            }
                            _ => {}
                        }
                    }
                    event @ (DeviceEvent::Encoder { .. } | DeviceEvent::Fader { .. }) => {
                        if let Some(update) = self.controls.as_mut().and_then(|c| c.handle(&event))
                        {
                            let _ = self.param_producer.push(update.change);
                            let _ = self.events.send(CoreEvent::ControlChanged {
                                control: update.control,
                                normalized: update.normalized,
                            });
                            if let Some((l1, l2)) =
                                coalescer.offer((update.label, update.display_value), now)
                            {
                                self.show(&l1, &l2);
                            }
                            revert_at = Some(now + Duration::from_millis(1200));
                        }
                    }
                    DeviceEvent::MainEncoderTurn { delta } => {
                        if let Some(item) = self.browser.scroll(delta) {
                            let id = item.id;
                            let name = item.name.clone();
                            let (pos, len) = self.browser.position();
                            let line2 = format!("{} {pos}/{len}", self.browser.category_name());
                            let _ = self.events.send(CoreEvent::BrowserSelected { id });
                            if let Some((l1, l2)) = coalescer.offer((name, line2), now) {
                                self.show(&l1, &l2);
                            }
                            revert_at = Some(now + Duration::from_secs(3));
                        }
                    }
                    DeviceEvent::MainEncoderShiftTurn { delta } => {
                        if delta != 0 {
                            let category = self.browser.cycle_category(delta).to_string();
                            let name = self
                                .browser
                                .current()
                                .map(|i| i.name.clone())
                                .unwrap_or_default();
                            if let Some(item) = self.browser.current() {
                                let _ =
                                    self.events.send(CoreEvent::BrowserSelected { id: item.id });
                            }
                            if let Some((l1, l2)) =
                                coalescer.offer((format!("[{category}]"), name), now)
                            {
                                self.show(&l1, &l2);
                            }
                            revert_at = Some(now + Duration::from_secs(3));
                        }
                    }
                    DeviceEvent::MainEncoderClick { pressed: true } => {
                        if let Some(item) = self.browser.current() {
                            let name = item.name.clone();
                            let _ = self.to_host.send(FromControl::LoadPreset {
                                id: item.id,
                                name: name.clone(),
                                path: item.path.clone(),
                                load_key: item.load_key.clone(),
                            });
                            self.show("Loading...", &name);
                            revert_at = None;
                        }
                    }
                    DeviceEvent::Transport {
                        control,
                        pressed: true,
                    } => {
                        use lab_midi::event::TransportControl as T;
                        let button = match control {
                            T::Record => Some(LooperButton::Record),
                            T::Loop => Some(LooperButton::Loop),
                            T::Play => Some(LooperButton::Play),
                            T::Stop => Some(LooperButton::Stop),
                            T::Tap => {
                                // Shift+Tap flips the pads between notes
                                // and loop slots.
                                self.set_pad_mode(match self.pad_mode {
                                    PadMode::Notes => PadMode::Loops,
                                    PadMode::Loops => PadMode::Notes,
                                });
                                None
                            }
                        };
                        if let Some(button) = button {
                            self.handle_looper(button, now);
                        }
                    }
                    _ => {}
                }
            }

            let now = Instant::now();
            loop {
                let msg = match self.from_host.try_recv() {
                    Ok(msg) => msg,
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    // Host thread is gone: shut down and release the port.
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                };
                match msg {
                    ToControl::PresetLoaded { name, params } => {
                        if let Some(c) = self.controls.as_mut() {
                            c.reset_values(&params);
                            let _ = self.events.send(CoreEvent::ControlsRebound {
                                controls: control_infos(c),
                            });
                        }
                        self.title = name.clone();
                        self.show(&name, "");
                        revert_at = None;
                    }
                    ToControl::PresetLoadFailed { name } => {
                        self.show("Load failed", &name);
                        revert_at = Some(now + Duration::from_secs(2));
                    }
                    ToControl::Rings { midi, params } => {
                        self.midi_producer = midi;
                        self.param_producer = params;
                    }
                    ToControl::Looper(button) => {
                        self.handle_looper(button, now);
                    }
                    ToControl::LooperPad(pad) => {
                        self.tap_slot(pad as usize, now);
                    }
                    ToControl::PadMode(mode) => {
                        self.set_pad_mode(mode);
                    }
                    ToControl::GuiControl {
                        control,
                        normalized,
                    } => {
                        if let Some(update) = self
                            .controls
                            .as_mut()
                            .and_then(|c| c.set_normalized(control, normalized))
                        {
                            let _ = self.param_producer.push(update.change);
                            let _ = self.events.send(CoreEvent::ControlChanged {
                                control: update.control,
                                normalized: update.normalized,
                            });
                            if let Some((l1, l2)) =
                                coalescer.offer((update.label, update.display_value), now)
                            {
                                self.show(&l1, &l2);
                            }
                            revert_at = Some(now + Duration::from_millis(1200));
                        }
                    }
                }
            }
            if let Some((l1, l2)) = coalescer.poll(now) {
                self.show(&l1, &l2);
            }
            if revert_at.is_some_and(|t| now >= t) {
                revert_at = None;
                let title = self.title.clone();
                self.show(&title, "");
            }
            // Keep frontends in sync with looper time and note-driven
            // transitions (armed -> recording happens without a button).
            if now.duration_since(last_looper_refresh) >= Duration::from_millis(500) {
                last_looper_refresh = now;
                let status = self.slot_status();
                let states = self.slot_states();
                if status != last_looper_status || states != last_looper_states {
                    last_looper_status = status.clone();
                    last_looper_states = states;
                    self.emit_looper(status);
                }
            }
        }
    }
}
