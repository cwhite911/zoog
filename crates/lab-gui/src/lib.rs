//! iced GUI. Depends only on lab-core; runs fully against the mock device
//! and stub engine (PLAN.md Phase 6).

mod macro_panel;
mod theme;

use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, text, text_input,
};
use iced::{Element, Fill, Length, Subscription, Task, keyboard};

use lab_core::app::{
    CoreCommand, CoreConfig, CoreEvent, CoreHandle, EngineConfig, PresetInfo, start,
};
use lab_core::mapping::Control;

use macro_panel::{ControlView, MacroPanel};

/// Boot parameters, hashed to identify the core subscription.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct Boot {
    pub plugin_match: String,
    pub mapping_path: Option<String>,
    pub arturia_mode: bool,
    pub sample_rate: u32,
    pub buffer_frames: u32,
    pub mock_device: bool,
    pub stub_engine: bool,
}

impl Default for Boot {
    fn default() -> Self {
        Boot {
            plugin_match: "Surge XT".to_string(),
            mapping_path: None,
            arturia_mode: false,
            sample_rate: 48_000,
            buffer_frames: 256,
            mock_device: false,
            stub_engine: false,
        }
    }
}

impl Boot {
    fn to_config(&self) -> CoreConfig {
        CoreConfig {
            plugin_match: self.plugin_match.clone(),
            mapping_path: self.mapping_path.clone().map(Into::into),
            arturia_mode: self.arturia_mode,
            engine: EngineConfig {
                sample_rate: self.sample_rate,
                buffer_frames: self.buffer_frames,
            },
            library_path: None,
            mock_device: self.mock_device,
            stub_engine: self.stub_engine,
            restore_session: true,
        }
    }
}

pub fn run(boot: Boot) -> iced::Result {
    let (width, height) = load_window_size().unwrap_or((1100.0, 720.0));
    let icon =
        iced::window::icon::from_file_data(include_bytes!("../assets/benchlab-256.png"), None).ok();
    iced::application(move || App::new(boot.clone()), App::update, App::view)
        .title(app_title)
        .subscription(App::subscription)
        .theme(app_theme)
        .window(iced::window::Settings {
            size: iced::Size::new(width, height),
            icon,
            ..iced::window::Settings::default()
        })
        .run()
}

fn window_state_path() -> Option<std::path::PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .map(|base| base.join("benchlab").join("window"))
}

fn load_window_size() -> Option<(f32, f32)> {
    let text = std::fs::read_to_string(window_state_path()?).ok()?;
    let mut parts = text.split_whitespace().map(str::parse::<f32>);
    match (parts.next(), parts.next()) {
        (Some(Ok(w)), Some(Ok(h))) if w >= 400.0 && h >= 300.0 => Some((w, h)),
        _ => None,
    }
}

fn save_window_size(size: iced::Size) {
    if let Some(path) = window_state_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, format!("{} {}", size.width, size.height));
    }
}

fn project_controls(controls: &[lab_core::app::ControlInfo]) -> Vec<ControlView> {
    controls
        .iter()
        .map(|c| ControlView {
            control: c.control,
            label: c.label.clone(),
            normalized: c.normalized,
            active: c.active,
        })
        .collect()
}

fn app_title(_app: &App) -> String {
    "benchlab".to_string()
}

fn app_theme(_app: &App) -> iced::Theme {
    iced::Theme::Dark
}

#[derive(Debug, Clone)]
pub enum Message {
    CoreStarted(CoreHandle),
    Core(CoreEvent),
    CoreStopped,
    SearchChanged(String),
    CategorySelected(String),
    FavoritesOnly(bool),
    PresetPressed(i64),
    ToggleFavorite(i64),
    ControlDragged(Control, f64),
    Key(keyboard::Event),
    WindowResized(iced::Size),
    OpenSettings(bool),
    Rescan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Main,
    Settings,
}

#[derive(Debug, Clone, Default)]
struct Stats {
    overruns: u64,
    max_callback_ms: f64,
    min_frames: u64,
    max_frames: u64,
    stream_errors: u64,
    dsp_load: f64,
    output_peak: f32,
}

struct App {
    boot: Boot,
    handle: Option<CoreHandle>,
    plugin_title: String,
    engine_running: bool,
    device_connected: bool,
    presets: Vec<PresetInfo>,
    categories: Vec<String>,
    search: String,
    category: String,
    favorites_only: bool,
    selected: Option<i64>,
    loaded: Option<i64>,
    loaded_name: Option<String>,
    audio: Option<lab_core::app::AudioInfo>,
    control_views: Vec<ControlView>,
    pads: [bool; 8],
    stats: Stats,
    last_error: Option<String>,
    page: Page,
}

const ALL_CATEGORIES: &str = "All";
/// Scrollable id for the preset list (hardware browsing scroll-follow).
const PRESET_LIST_ID: &str = "preset-list";
/// Estimated preset row height, for scroll-follow positioning.
const ROW_HEIGHT: f32 = 33.0;
/// Rows rendered at once; refine the search to see the rest.
const MAX_VISIBLE_ROWS: usize = 200;

impl App {
    fn new(boot: Boot) -> (Self, Task<Message>) {
        (
            App {
                boot,
                handle: None,
                plugin_title: "starting...".to_string(),
                engine_running: false,
                device_connected: false,
                presets: Vec::new(),
                categories: Vec::new(),
                search: String::new(),
                category: ALL_CATEGORIES.to_string(),
                favorites_only: false,
                selected: None,
                loaded: None,
                loaded_name: None,
                audio: None,
                control_views: Vec::new(),
                pads: [false; 8],
                stats: Stats::default(),
                last_error: None,
                page: Page::Main,
            },
            Task::none(),
        )
    }

    fn filtered(&self) -> Vec<&PresetInfo> {
        let query = self.search.to_lowercase();
        self.presets
            .iter()
            .filter(|p| {
                (query.is_empty() || p.name.to_lowercase().contains(&query))
                    && (self.category == ALL_CATEGORIES
                        || p.category.as_deref() == Some(self.category.as_str()))
                    && (!self.favorites_only || p.favorite)
            })
            .collect()
    }

    fn set_control_view(&mut self, control: Control, normalized: f64) {
        if let Some(view) = self.control_views.iter_mut().find(|c| c.control == control) {
            view.normalized = normalized;
        }
    }

    fn send(&self, command: CoreCommand) {
        if let Some(handle) = &self.handle {
            handle.send(command);
        }
    }

    fn move_selection(&mut self, delta: i64) {
        let filtered = self.filtered();
        if filtered.is_empty() {
            return;
        }
        let current = self
            .selected
            .and_then(|id| filtered.iter().position(|p| p.id == id))
            .map(|i| i as i64)
            .unwrap_or(-1);
        let next = (current + delta).clamp(0, filtered.len() as i64 - 1);
        self.selected = Some(filtered[next as usize].id);
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::CoreStarted(handle) => self.handle = Some(handle),
            Message::CoreStopped => {
                self.last_error = Some("core stopped".to_string());
                self.engine_running = false;
            }
            Message::Core(event) => return self.on_core_event(event),
            Message::SearchChanged(search) => self.search = search,
            Message::CategorySelected(category) => self.category = category,
            Message::FavoritesOnly(on) => self.favorites_only = on,
            Message::PresetPressed(id) => {
                if self.selected == Some(id) {
                    self.send(CoreCommand::LoadPreset(id));
                } else {
                    self.selected = Some(id);
                }
            }
            Message::ToggleFavorite(id) => {
                if let Some(preset) = self.presets.iter_mut().find(|p| p.id == id) {
                    preset.favorite = !preset.favorite;
                    let favorite = preset.favorite;
                    self.send(CoreCommand::SetFavorite { id, favorite });
                }
            }
            Message::ControlDragged(control, normalized) => {
                self.set_control_view(control, normalized);
                self.send(CoreCommand::SetControl {
                    control,
                    normalized,
                });
            }
            Message::Key(event) => {
                if let keyboard::Event::KeyPressed { key, modifiers, .. } = event {
                    use keyboard::key::{Key, Named};
                    match key.as_ref() {
                        Key::Named(Named::ArrowDown) => self.move_selection(1),
                        Key::Named(Named::ArrowUp) => self.move_selection(-1),
                        Key::Named(Named::Enter) => {
                            if let Some(id) = self.selected {
                                self.send(CoreCommand::LoadPreset(id));
                            }
                        }
                        Key::Character("f") if modifiers.control() => {
                            if let Some(id) = self.selected {
                                return self.update(Message::ToggleFavorite(id));
                            }
                        }
                        _ => {}
                    }
                }
            }
            Message::WindowResized(size) => save_window_size(size),
            Message::OpenSettings(open) => {
                self.page = if open { Page::Settings } else { Page::Main };
            }
            Message::Rescan => self.send(CoreCommand::RescanLibrary),
        }
        Task::none()
    }

    fn scroll_to_selected(&self) -> Task<Message> {
        let Some(id) = self.selected else {
            return Task::none();
        };
        let Some(index) = self.filtered().iter().position(|p| p.id == id) else {
            return Task::none();
        };
        // Only rows within the render cap are reachable.
        let index = index.min(MAX_VISIBLE_ROWS);
        let y = (index as f32 * ROW_HEIGHT - 150.0).max(0.0);
        iced::widget::operation::scroll_to(
            iced::widget::Id::new(PRESET_LIST_ID),
            iced::widget::operation::AbsoluteOffset { x: 0.0, y },
        )
    }

    fn on_core_event(&mut self, event: CoreEvent) -> Task<Message> {
        match event {
            CoreEvent::Ready {
                plugin_title,
                engine_running,
                device_connected,
                presets,
                categories,
                controls,
                audio,
            } => {
                self.audio = audio;
                self.plugin_title = plugin_title;
                self.engine_running = engine_running;
                self.device_connected = device_connected;
                self.presets = presets;
                self.categories = categories;
                self.control_views = project_controls(&controls);
            }
            CoreEvent::PresetLoaded { id, name } => {
                self.loaded = Some(id);
                self.selected = Some(id);
                self.loaded_name = Some(name);
            }
            CoreEvent::PresetLoadFailed { name } => {
                self.last_error = Some(format!("load failed: {name}"));
            }
            CoreEvent::ControlChanged {
                control,
                normalized,
            } => {
                self.set_control_view(control, normalized);
            }
            CoreEvent::Pad { index, down } => {
                if let Some(pad) = self.pads.get_mut(index as usize) {
                    *pad = down;
                }
            }
            CoreEvent::BrowserSelected { id } => {
                self.selected = Some(id);
                return self.scroll_to_selected();
            }
            CoreEvent::DeviceConnected(connected) => self.device_connected = connected,
            CoreEvent::ControlsRebound { controls } => {
                self.control_views = project_controls(&controls);
            }
            CoreEvent::LibraryRescanned {
                presets,
                categories,
            } => {
                self.presets = presets;
                self.categories = categories;
            }
            CoreEvent::Stats {
                callbacks: _,
                overruns,
                max_callback_ms,
                min_frames,
                max_frames,
                stream_errors,
                dsp_load,
                output_peak,
            } => {
                self.stats = Stats {
                    overruns,
                    max_callback_ms,
                    min_frames,
                    max_frames,
                    stream_errors,
                    dsp_load,
                    output_peak,
                };
            }
            CoreEvent::Error(e) => self.last_error = Some(e),
        }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            Subscription::run_with(self.boot.clone(), core_stream),
            keyboard::listen().map(Message::Key),
            iced::window::resize_events().map(|(_, size)| Message::WindowResized(size)),
        ])
    }

    fn view(&self) -> Element<'_, Message> {
        match self.page {
            Page::Settings => self.view_settings(),
            Page::Main => self.view_main(),
        }
    }

    fn view_main(&self) -> Element<'_, Message> {
        let filtered = self.filtered();
        let total = filtered.len();

        // Left column: filters.
        let mut categories = vec![ALL_CATEGORIES.to_string()];
        categories.extend(self.categories.iter().cloned());
        let filters = column![
            text("benchlab").size(22),
            text(self.plugin_title.clone())
                .size(14)
                .color(theme::TEXT_DIM),
            text_input("search (type, arrows, Enter)", &self.search)
                .on_input(Message::SearchChanged)
                .padding(8),
            pick_list(
                categories,
                Some(self.category.clone()),
                Message::CategorySelected
            )
            .width(Fill),
            checkbox(self.favorites_only)
                .label("favorites only")
                .on_toggle(Message::FavoritesOnly),
            button("Rescan library").on_press(Message::Rescan),
            button("Settings").on_press(Message::OpenSettings(true)),
        ]
        .spacing(10)
        .width(Length::Fixed(190.0));

        // Center: preset list.
        let mut list = column![].spacing(2);
        for preset in filtered.into_iter().take(MAX_VISIBLE_ROWS) {
            let selected = self.selected == Some(preset.id);
            let loaded = self.loaded == Some(preset.id);
            let marker = if loaded {
                "> "
            } else if preset.favorite {
                "* "
            } else {
                "  "
            };
            let label = row![
                text(format!("{marker}{}", preset.name)).width(Fill),
                text(preset.author.clone().unwrap_or_default())
                    .size(12)
                    .color(theme::TEXT_DIM),
            ]
            .spacing(8);
            let row_button = button(label)
                .width(Fill)
                .style(if selected {
                    button::primary
                } else {
                    button::text
                })
                .on_press(Message::PresetPressed(preset.id));
            let fav = button(text(if preset.favorite { "*" } else { "+" }).size(12))
                .style(button::text)
                .on_press(Message::ToggleFavorite(preset.id));
            list = list.push(row![row_button, fav].spacing(2));
        }
        if total > MAX_VISIBLE_ROWS {
            list = list.push(
                text(format!(
                    "... {} more; refine the search",
                    total - MAX_VISIBLE_ROWS
                ))
                .size(12)
                .color(theme::TEXT_DIM),
            );
        }
        let presets_panel = column![
            text(format!("{total} presets"))
                .size(12)
                .color(theme::TEXT_DIM),
            scrollable(list).id(PRESET_LIST_ID).height(Fill).width(Fill),
        ]
        .spacing(6)
        .width(Fill);

        // Right: macro view.
        let macro_view = iced::widget::canvas(MacroPanel {
            controls: &self.control_views,
            pads: &self.pads,
        })
        .width(Fill)
        .height(Fill);
        let now_playing = self
            .loaded_name
            .clone()
            .unwrap_or_else(|| "no preset loaded".to_string());
        let macro_panel = column![
            text(now_playing).size(24),
            text(self.plugin_title.clone())
                .size(13)
                .color(theme::TEXT_DIM),
            macro_view,
            text("drag knobs and faders; hardware moves mirror here")
                .size(11)
                .color(theme::TEXT_DIM),
        ]
        .spacing(8)
        .width(Length::FillPortion(3));

        let body = row![filters, presets_panel, macro_panel]
            .spacing(16)
            .height(Fill);

        // Status bar.
        let audio = match (&self.audio, self.engine_running) {
            (Some(a), _) => format!(
                "{} {} Hz {}",
                a.device_name.as_deref().unwrap_or("audio"),
                a.sample_rate,
                a.sample_format
            ),
            (None, true) => "audio".to_string(),
            (None, false) => "no audio (stub)".to_string(),
        };
        let meter = {
            let peak = self.stats.output_peak;
            let db = if peak > 0.0 {
                format!("{:>5.1} dB", 20.0 * peak.log10())
            } else {
                "  -inf".to_string()
            };
            let bars = (peak.clamp(0.0, 1.0) * 8.0).round() as usize;
            format!("out [{}{}] {db}", "#".repeat(bars), "-".repeat(8 - bars))
        };
        let status = format!(
            "MIDI: {}   {}   {meter}   dsp {:>4.1}%   frames {}..{}   overruns {}   max {:.2} ms   stream errors {}{}",
            if self.device_connected {
                "connected"
            } else {
                "absent"
            },
            audio,
            self.stats.dsp_load * 100.0,
            self.stats.min_frames,
            self.stats.max_frames,
            self.stats.overruns,
            self.stats.max_callback_ms,
            self.stats.stream_errors,
            self.last_error
                .as_deref()
                .map(|e| format!("   [{e}]"))
                .unwrap_or_default(),
        );
        let status_bar = container(text(status).size(12).color(
            if self.stats.overruns > 0 || self.last_error.is_some() {
                theme::WARN
            } else {
                theme::TEXT_DIM
            },
        ))
        .padding(6);

        container(column![body, status_bar].spacing(8))
            .padding(12)
            .into()
    }

    fn view_settings(&self) -> Element<'_, Message> {
        let boot = &self.boot;
        let entry = |name: &str, value: String| {
            row![
                text(name.to_string()).width(Length::Fixed(220.0)),
                text(value).color(theme::TEXT_DIM)
            ]
            .spacing(10)
        };
        container(
            column![
                text("Settings").size(22),
                entry("Plugin", boot.plugin_match.clone()),
                entry(
                    "Mapping file",
                    boot.mapping_path
                        .clone()
                        .unwrap_or_else(|| "mappings/surge-xt.toml".to_string())
                ),
                entry(
                    "Sample rate (requested)",
                    format!("{} Hz", boot.sample_rate)
                ),
                entry(
                    "Buffer (requested)",
                    format!("{} frames", boot.buffer_frames)
                ),
                entry(
                    "Control map",
                    if boot.arturia_mode {
                        "Arturia mode".to_string()
                    } else {
                        "DAW mode".to_string()
                    }
                ),
                entry(
                    "MIDI port match",
                    "minilab3 midi (see labctl ports)".to_string()
                ),
                entry(
                    "Plugin search paths",
                    "CLAP standard paths + $CLAP_PATH".to_string()
                ),
                text("These are set with benchlab's command-line flags and apply at startup.")
                    .size(12)
                    .color(theme::TEXT_DIM),
                row![
                    button("Rescan library").on_press(Message::Rescan),
                    button("Back").on_press(Message::OpenSettings(false)),
                ]
                .spacing(10),
            ]
            .spacing(12),
        )
        .padding(20)
        .into()
    }
}

/// The core runs inside this subscription stream; its first item hands the
/// command sender to the application.
fn core_stream(boot: &Boot) -> iced::futures::channel::mpsc::UnboundedReceiver<Message> {
    let (tx, rx) = iced::futures::channel::mpsc::unbounded();
    let config = boot.to_config();
    std::thread::Builder::new()
        .name("benchlab-core-bridge".to_string())
        .spawn(move || {
            let (handle, events) = start(config);
            if tx.unbounded_send(Message::CoreStarted(handle)).is_err() {
                return;
            }
            for event in events {
                if tx.unbounded_send(Message::Core(event)).is_err() {
                    return;
                }
            }
            let _ = tx.unbounded_send(Message::CoreStopped);
        })
        .expect("spawning the core bridge thread cannot fail");
    rx
}
