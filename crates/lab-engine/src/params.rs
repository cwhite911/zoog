// SPDX-License-Identifier: GPL-3.0-only
// SPDX-FileCopyrightText: 2026 Corey T. White

//! Parameter enumeration via the `clap.params` extension (main-thread side).

use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};
use clack_host::prelude::*;

use crate::host::BenchHost;

/// Owned description of one plugin parameter.
#[derive(Debug, Clone)]
pub struct ParamDescription {
    pub id: u32,
    pub name: String,
    pub module: String,
    pub min_value: f64,
    pub max_value: f64,
    pub default_value: f64,
    /// Current plain value, when the plugin reports one.
    pub value: Option<f64>,
    /// Human-readable rendering of `value`, when the plugin provides one.
    pub value_text: Option<String>,
    pub is_automatable: bool,
    /// The plugin's opaque per-param cookie pointer, carried as usize so it
    /// can cross threads. Must be passed back in param events: some
    /// plugins (Odin2 2.4.1) dereference it without a null check.
    pub cookie: usize,
}

/// Enumerates every parameter the plugin exposes. Returns an empty list if
/// the plugin lacks the params extension.
pub fn list_params(instance: &mut PluginInstance<BenchHost>) -> Vec<ParamDescription> {
    let handle = instance.plugin_handle();
    let Some(params) = handle.get_extension::<PluginParams>() else {
        return Vec::new();
    };

    let mut buffer = ParamInfoBuffer::new();
    let mut text_buffer = [0u8; 128];
    let mut out = Vec::new();

    for index in 0..params.count(&handle) {
        let Some(info) = params.get_info(&handle, index, &mut buffer) else {
            continue;
        };
        let id = info.id;
        let value = params.get_value(&handle, id);
        let value_text = value.and_then(|v| {
            params
                .value_to_text(&handle, id, v, &mut text_buffer)
                .ok()
                .map(|t| String::from_utf8_lossy(t).into_owned())
        });
        out.push(ParamDescription {
            id: id.get(),
            name: String::from_utf8_lossy(info.name).into_owned(),
            module: String::from_utf8_lossy(info.module).into_owned(),
            min_value: info.min_value,
            max_value: info.max_value,
            default_value: info.default_value,
            value,
            value_text,
            is_automatable: info.flags.contains(ParamInfoFlags::IS_AUTOMATABLE),
            cookie: info.cookie.as_raw() as usize,
        });
    }
    out
}
