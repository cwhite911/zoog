//! Binds `mappings/surge-xt.toml` against the real Surge XT plugin,
//! verifying every recorded param id still resolves. Skips when Surge XT
//! is not installed.

use std::ffi::CString;
use std::path::Path;
use std::sync::mpsc::channel;

use lab_core::mapping::{Control, MacroControls, MappingFile};
use lab_engine::clack_host::prelude::PluginInstance;
use lab_engine::discovery::find_plugin;
use lab_engine::host::{BenchHost, BenchHostMainThread, BenchHostShared, host_info};

#[test]
fn surge_mapping_binds_against_real_plugin() {
    let mapping_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mappings/surge-xt.toml");
    let file = MappingFile::load(&mapping_path).expect("mapping file must parse");

    let plugin = match find_plugin(&file.plugin_id) {
        Ok(plugin) => plugin,
        Err(e) => {
            eprintln!("skipping: {e}");
            return;
        }
    };

    let (host_tx, _host_rx) = channel();
    let plugin_id = CString::new(plugin.id.as_str()).unwrap();
    let mut instance = PluginInstance::<BenchHost>::new(
        |_| BenchHostShared::new(host_tx),
        |_| BenchHostMainThread::new(),
        &plugin.entry,
        &plugin_id,
        &host_info(),
    )
    .expect("plugin instantiation failed");

    let params = lab_engine::params::list_params(&mut instance);
    let controls =
        MacroControls::bind(&file, &plugin.id, &params).expect("all mapping entries must resolve");

    let bound: Vec<Control> = controls.bindings().map(|b| b.control).collect();
    for i in 0..8 {
        assert!(bound.contains(&Control::Encoder(i)), "encoder {i} unbound");
    }
    for i in 0..4 {
        assert!(bound.contains(&Control::Fader(i)), "fader {i} unbound");
    }
}
