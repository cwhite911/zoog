// SPDX-License-Identifier: GPL-3.0-only
// SPDX-FileCopyrightText: 2026 Corey T. White

//! Binds every mapping file in `mappings/` against its real plugin,
//! verifying the recorded param ids still resolve. Each mapping is skipped
//! (with a message) when its plugin is not installed.

use std::ffi::CString;
use std::path::Path;
use std::sync::mpsc::channel;

use lab_core::mapping::{Control, MacroControls, MappingFile};
use lab_engine::clack_host::prelude::PluginInstance;
use lab_engine::discovery::find_plugin;
use lab_engine::host::{BenchHost, BenchHostMainThread, BenchHostShared, host_info};

#[test]
fn all_mappings_bind_against_their_plugins() {
    let mappings_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mappings");
    let mut checked = 0;
    for entry in std::fs::read_dir(&mappings_dir).expect("mappings dir must exist") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let file = MappingFile::load(&path)
            .unwrap_or_else(|e| panic!("{} must parse: {e}", path.display()));

        let plugin = match find_plugin(&file.plugin_id) {
            Ok(plugin) => plugin,
            Err(e) => {
                eprintln!("skipping {}: {e}", path.display());
                continue;
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
        let controls = MacroControls::bind(&file, &plugin.id, &params)
            .unwrap_or_else(|e| panic!("{}: all entries must resolve: {e}", path.display()));

        let bound: Vec<Control> = controls.bindings().map(|b| b.control).collect();
        for i in 0..8 {
            assert!(
                bound.contains(&Control::Encoder(i)),
                "{}: encoder {i} unbound",
                path.display()
            );
        }
        for i in 0..4 {
            assert!(
                bound.contains(&Control::Fader(i)),
                "{}: fader {i} unbound",
                path.display()
            );
        }
        println!("bound {} against {}", path.display(), plugin.id);
        checked += 1;
    }
    // Parsing is verified above for every mapping regardless; binding
    // needs the plugin, so a machine without any installed engine (CI)
    // legitimately checks nothing here.
    if checked == 0 {
        eprintln!("no mapping's plugin is installed; binding not verified");
    }
}
