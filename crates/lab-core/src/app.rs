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
use lab_midi::sysex::{display_text, init};

use crate::browser::{BrowseItem, Browser};
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
        }
    }
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
    Shutdown,
}

/// A preset as exposed to the frontend.
#[derive(Debug, Clone)]
pub struct PresetInfo {
    pub id: i64,
    pub name: String,
    pub category: Option<String>,
    pub favorite: bool,
}

impl From<&PresetRow> for PresetInfo {
    fn from(row: &PresetRow) -> Self {
        PresetInfo {
            id: row.id,
            name: row.name.clone(),
            category: row.category.clone(),
            favorite: row.favorite,
        }
    }
}

/// A bound control as exposed to the frontend (for the macro view).
#[derive(Debug, Clone)]
pub struct ControlInfo {
    pub control: Control,
    pub label: String,
    pub normalized: f64,
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
        sample_rate: u32,
        buffer_frames: u32,
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

    // Device.
    let device: Option<Box<dyn Device + Send>> = if config.mock_device {
        Some(Box::new(MockDevice::new()))
    } else {
        match MidirDevice::open(CLIENT_NAME, DEFAULT_PORT_MATCH) {
            Ok(device) => Some(Box::new(device)),
            Err(e) => {
                let _ = events.send(CoreEvent::Error(format!(
                    "no controller ({e}); running without hardware"
                )));
                None
            }
        }
    };
    let device_connected = device.is_some();

    // Rings and channels.
    let (midi_producer, midi_consumer) = rtrb::RingBuffer::<RtMidi>::new(1024);
    let (param_producer, param_consumer) = rtrb::RingBuffer::<ParamChange>::new(256);
    let (from_control_tx, from_control_rx) = channel::<FromControl>();
    let (to_control_tx, to_control_rx) = channel::<ToControl>();

    // Control thread.
    if let Some(device) = device {
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
        };
        thread::Builder::new()
            .name("benchlab-control".to_string())
            .spawn(move || ctx.run())?;
    }

    // Audio.
    let mut stats: Option<std::sync::Arc<AudioStats>> = None;
    let mut _stream = None;
    if let Some(instance) = instance.as_mut() {
        match activate_to_stream(instance, midi_consumer, Some(param_consumer), config.engine) {
            Ok((stream, audio_stats)) => {
                _stream = Some(stream);
                stats = Some(audio_stats);
            }
            Err(e) => {
                let _ = events.send(CoreEvent::Error(format!("audio failed: {e}")));
            }
        }
    }

    let _ = events.send(CoreEvent::Ready {
        plugin_title: plugin_title.clone(),
        engine_running: stats.is_some(),
        device_connected,
        presets,
        categories,
        controls: control_infos,
        sample_rate: config.engine.sample_rate,
        buffer_frames: config.engine.buffer_frames,
    });

    // Main service loop.
    let timers = instance
        .as_mut()
        .and_then(|i| i.access_handler(|h| h.timer_support().map(|ext| (h.timers.clone(), ext))));
    let mut last_stats = Instant::now();
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

        // Preset loads requested by the control thread.
        while let Ok(msg) = from_control_rx.try_recv() {
            let FromControl::LoadPreset {
                id,
                name,
                path,
                load_key,
            } = msg;
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

        // Frontend commands.
        loop {
            match commands.try_recv() {
                Ok(CoreCommand::Shutdown) => return Ok(()),
                Ok(CoreCommand::LoadPreset(id)) => {
                    if let Ok(Some(row)) = library.get(id) {
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
            let _ = events.send(CoreEvent::Stats {
                callbacks: stats.callbacks.load(Relaxed),
                overruns: stats.overruns.load(Relaxed),
                max_callback_ms: stats.max_callback_ns.load(Relaxed) as f64 / 1e6,
                min_frames: stats.min_frames.load(Relaxed),
                max_frames: stats.max_frames.load(Relaxed),
                stream_errors: stats.stream_errors.load(Relaxed),
            });
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

fn bind_mapping(
    config: &CoreConfig,
    plugin_id: &str,
    params: &[ParamDescription],
    events: &Sender<CoreEvent>,
) -> (Option<MacroControls>, Vec<ControlInfo>) {
    let path = config
        .mapping_path
        .clone()
        .unwrap_or_else(|| PathBuf::from("mappings/surge-xt.toml"));
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
            let infos = controls
                .bindings()
                .map(|binding| ControlInfo {
                    control: binding.control,
                    label: binding.label.clone(),
                    normalized: controls.normalized_value(binding),
                })
                .collect();
            (Some(controls), infos)
        }
        Err(e) => {
            let _ = events.send(CoreEvent::Error(format!("mapping bind failed: {e}")));
            (None, Vec::new())
        }
    }
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

/// State owned by the device control thread.
struct ControlThread {
    device: Box<dyn Device + Send>,
    map: ControlMap,
    controls: Option<MacroControls>,
    browser: Browser,
    midi_producer: rtrb::Producer<RtMidi>,
    param_producer: rtrb::Producer<ParamChange>,
    to_host: Sender<FromControl>,
    from_host: Receiver<ToControl>,
    events: Sender<CoreEvent>,
    title: String,
}

impl ControlThread {
    fn show(&mut self, line1: &str, line2: &str) {
        let _ = self.device.send(&display_text(line1, line2));
    }

    fn run(mut self) {
        let mut coalescer: Coalescer<(String, String)> = Coalescer::new(Duration::from_millis(33));
        let mut revert_at: Option<Instant> = None;

        let _ = self.device.send(&init());
        let title = self.title.clone();
        self.show(&title, "");

        loop {
            let now = Instant::now();
            if let Some(msg) = self.device.recv_timeout(Duration::from_millis(50)) {
                match self.map.map(MidiMessage::decode(&msg.bytes)) {
                    event @ (DeviceEvent::NoteOn { .. }
                    | DeviceEvent::NoteOff { .. }
                    | DeviceEvent::PitchBend { .. }
                    | DeviceEvent::ModStrip { .. }
                    | DeviceEvent::PadDown { .. }
                    | DeviceEvent::PadUp { .. }
                    | DeviceEvent::PadPressure { .. }) => {
                        if let Some(rt) = RtMidi::from_bytes(msg.timestamp_us, &msg.bytes) {
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
                    _ => {}
                }
            }

            let now = Instant::now();
            while let Ok(msg) = self.from_host.try_recv() {
                match msg {
                    ToControl::PresetLoaded { name, params } => {
                        if let Some(c) = self.controls.as_mut() {
                            c.reset_values(&params);
                        }
                        self.title = name.clone();
                        self.show(&name, "");
                        revert_at = None;
                    }
                    ToControl::PresetLoadFailed { name } => {
                        self.show("Load failed", &name);
                        revert_at = Some(now + Duration::from_secs(2));
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
        }
    }
}
