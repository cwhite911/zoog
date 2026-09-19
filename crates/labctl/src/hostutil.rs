//! Shared plugin-instance plumbing for labctl subcommands.

use std::error::Error;
use std::sync::mpsc::{Receiver, channel};

use lab_engine::clack_host::prelude::PluginInstance;
use lab_engine::discovery::FoundPlugin;
use lab_engine::host::{
    BenchHost, BenchHostMainThread, BenchHostShared, HostThreadMessage, host_info,
};

/// Instantiates a plugin with the benchlab host, returning the instance and
/// the host-thread message receiver.
pub fn make_instance(
    plugin: &FoundPlugin,
) -> Result<(PluginInstance<BenchHost>, Receiver<HostThreadMessage>), Box<dyn Error>> {
    let (host_tx, host_rx) = channel();
    let plugin_id = std::ffi::CString::new(plugin.id.as_str())?;
    let instance = PluginInstance::<BenchHost>::new(
        |_| BenchHostShared::new(host_tx),
        |_| BenchHostMainThread::new(),
        &plugin.entry,
        &plugin_id,
        &host_info(),
    )?;
    Ok((instance, host_rx))
}
