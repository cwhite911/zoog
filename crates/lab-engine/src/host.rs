//! The zoog CLAP host implementation: handler types, extension
//! declarations, and the plugin-registered timer bookkeeping.
//!
//! Follows the clack cpal host example's structure, without the GUI
//! extension (zoog never opens plugin editors).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamRescanFlags,
};
use clack_extensions::timer::{HostTimer, HostTimerImpl, PluginTimer, TimerId};
use clack_host::prelude::*;

/// Messages sent from plugin callbacks to the host thread's run loop.
pub enum HostThreadMessage {
    /// The plugin asked for its `on_main_thread` callback to run.
    RunOnMainThread,
}

pub struct BenchHost;

impl HostHandlers for BenchHost {
    type Shared<'a> = BenchHostShared;
    type MainThread<'a> = BenchHostMainThread;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder
            .register::<HostLog>()
            .register::<HostTimer>()
            .register::<HostParams>();
    }
}

pub struct BenchHostShared {
    sender: Sender<HostThreadMessage>,
}

impl BenchHostShared {
    pub fn new(sender: Sender<HostThreadMessage>) -> Self {
        Self { sender }
    }
}

impl<'a> SharedHandler<'a> for BenchHostShared {
    fn initializing(&self, _instance: InitializingPluginHandle<'a>) {}

    fn request_restart(&self) {
        // Restarting is not supported.
    }

    fn request_process(&self) {
        // We never pause processing; the audio stream is always running.
    }

    fn request_callback(&self) {
        let _ = self.sender.send(HostThreadMessage::RunOnMainThread);
    }
}

impl HostLogImpl for BenchHostShared {
    fn log(&self, severity: LogSeverity, message: &str) {
        // Not real-time safe, matching the example's caveat; plugin logs are
        // rare and this never runs on the audio thread in practice.
        if severity > LogSeverity::Debug {
            eprintln!("[plugin {severity}] {message}");
        }
    }
}

impl HostParamsImplShared for BenchHostShared {
    fn request_flush(&self) {
        // Processing never stops, so events always flush through process().
    }
}

/// Main-thread host data. Unlike the clack example we do not keep the
/// plugin handle here; the run loop reaches the plugin through
/// `PluginInstance::plugin_handle()` instead, which keeps this type
/// lifetime-free.
pub struct BenchHostMainThread {
    timer_support: Cell<Option<PluginTimer>>,
    /// Rc so the run loop can clone it out of `access_handler` and tick it
    /// alongside `plugin_handle()` without a double borrow of the instance
    /// (same pattern as the clack example).
    pub timers: Rc<Timers>,
}

impl BenchHostMainThread {
    pub fn new() -> Self {
        Self {
            timer_support: Cell::new(None),
            timers: Rc::new(Timers::new()),
        }
    }

    pub fn timer_support(&self) -> Option<PluginTimer> {
        self.timer_support.get()
    }
}

impl Default for BenchHostMainThread {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> MainThreadHandler<'a> for BenchHostMainThread {
    fn initialized(&self, instance: InitializedPluginHandle<'a>) {
        self.timer_support.set(instance.get_extension());
    }
}

impl HostParamsImplMainThread for BenchHostMainThread {
    fn rescan(&self, _flags: ParamRescanFlags) {
        // Host-side parameter values are re-read on demand instead of
        // tracked incrementally, so a rescan needs no action here.
    }

    fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {}
}

impl HostTimerImpl for BenchHostMainThread {
    fn register_timer(&self, period_ms: u32) -> Result<TimerId, HostError> {
        Ok(self
            .timers
            .register_new(Duration::from_millis(period_ms as u64)))
    }

    fn unregister_timer(&self, timer_id: TimerId) -> Result<(), HostError> {
        if self.timers.unregister(timer_id) {
            Ok(())
        } else {
            Err(HostError::Message("Unknown timer ID"))
        }
    }
}

/// Plugin-registered timers, ticked from the host thread's run loop.
/// Adapted from the clack example's Timers.
pub struct Timers {
    timers: RefCell<HashMap<TimerId, Timer>>,
    latest_id: Cell<u32>,
}

impl Timers {
    fn new() -> Self {
        Self {
            timers: RefCell::new(HashMap::new()),
            latest_id: Cell::new(0),
        }
    }

    fn register_new(&self, interval: Duration) -> TimerId {
        // The CLAP spec recommends 30ms as the fastest a host should run
        // timers; clamp anything faster.
        const MIN_INTERVAL: Duration = Duration::from_millis(10);
        let interval = interval.max(MIN_INTERVAL);

        let id = TimerId(self.latest_id.get() + 1);
        self.latest_id.set(id.0);
        self.timers
            .borrow_mut()
            .insert(id, Timer::new(id, interval));
        id
    }

    fn unregister(&self, id: TimerId) -> bool {
        self.timers.borrow_mut().remove(&id).is_some()
    }

    /// Ticks all timers, calling the plugin for each one due.
    pub fn tick(&self, timer_ext: &PluginTimer, plugin: &PluginMainThreadHandle) {
        let due: Vec<TimerId> = {
            let mut timers = self.timers.borrow_mut();
            let now = Instant::now();
            timers
                .values_mut()
                .filter_map(|t| t.tick(now).then_some(t.id))
                .collect()
        };
        for id in due {
            timer_ext.on_timer(plugin, id);
        }
    }
}

struct Timer {
    id: TimerId,
    interval: Duration,
    last_triggered_at: Option<Instant>,
}

impl Timer {
    fn new(id: TimerId, interval: Duration) -> Self {
        Self {
            id,
            interval,
            last_triggered_at: None,
        }
    }

    fn tick(&mut self, now: Instant) -> bool {
        let triggered = match self.last_triggered_at {
            Some(last) => now.duration_since(last) > self.interval,
            None => true,
        };
        if triggered {
            self.last_triggered_at = Some(now);
        }
        triggered
    }
}

/// Host identity reported to plugins.
pub fn host_info() -> HostInfo {
    HostInfo::new(
        "zoog",
        "zoog",
        "https://github.com/cwhite911/zoog",
        env!("CARGO_PKG_VERSION"),
    )
    .expect("host info strings contain no interior NUL")
}
