// SPDX-License-Identifier: GPL-3.0-only
// SPDX-FileCopyrightText: 2026 Corey T. White

//! Preset discovery (CLAP preset discovery factory) and preset loading
//! (`clap.preset-load`). Surge XT exposes both, which is why this path
//! is preferred over host-side `clap.state` snapshots.

use std::error::Error;
use std::ffi::{CStr, CString};
use std::fmt;
use std::path::{Path, PathBuf};

use clack_extensions::preset_discovery::prelude::*;
use clack_extensions::preset_discovery::{
    PluginPresetLoad,
    indexer::IndexerImpl,
    metadata_receiver::MetadataReceiverImpl,
    provider::{Provider, ProviderInstanceError},
};
use clack_host::prelude::*;

use crate::host::BenchHost;

/// One preset found by the plugin's discovery provider.
#[derive(Debug, Clone, Default)]
pub struct DiscoveredPreset {
    pub name: String,
    /// Opaque per-container key passed back to `clap.preset-load`.
    pub load_key: Option<String>,
    /// Filesystem path of the container, or `None` for plugin-internal
    /// presets.
    pub path: Option<PathBuf>,
    pub plugin_ids: Vec<String>,
    pub is_factory: bool,
    pub is_user: bool,
    pub creators: Vec<String>,
    pub description: Option<String>,
    /// CLAP preset features (categories/tags as reported by the plugin).
    pub features: Vec<String>,
}

/// Indexer that records what the provider declares.
#[derive(Debug, Default)]
struct CollectingIndexer {
    file_extensions: Vec<String>,
    locations: Vec<(Option<PathBuf>, Flags)>,
}

impl IndexerImpl for CollectingIndexer {
    fn declare_filetype(&mut self, file_type: FileType) -> Result<(), HostError> {
        if let Some(ext) = file_type.file_extension {
            self.file_extensions
                .push(ext.to_string_lossy().to_lowercase());
        }
        Ok(())
    }

    fn declare_location(&mut self, location: LocationInfo) -> Result<(), HostError> {
        let path = location
            .location
            .file_path()
            .map(|p| PathBuf::from(p.to_string_lossy().into_owned()));
        self.locations.push((path, location.flags));
        Ok(())
    }

    fn declare_soundpack(&mut self, _soundpack: Soundpack) -> Result<(), HostError> {
        Ok(())
    }
}

/// Metadata receiver that accumulates presets, attaching the location path
/// that is currently being read.
#[derive(Debug, Default)]
struct CollectingReceiver {
    current_path: Option<PathBuf>,
    location_flags: Flags,
    presets: Vec<DiscoveredPreset>,
    errors: u32,
}

impl CollectingReceiver {
    fn current(&mut self) -> Option<&mut DiscoveredPreset> {
        self.presets.last_mut()
    }
}

impl MetadataReceiverImpl for CollectingReceiver {
    fn on_error(&mut self, _error_code: i32, _error_message: Option<&CStr>) {
        self.errors += 1;
    }

    fn begin_preset(
        &mut self,
        name: Option<&CStr>,
        load_key: Option<&CStr>,
    ) -> Result<(), HostError> {
        let fallback_name = || {
            self.current_path
                .as_deref()
                .and_then(Path::file_stem)
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        self.presets.push(DiscoveredPreset {
            name: name
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(fallback_name),
            load_key: load_key.map(|k| k.to_string_lossy().into_owned()),
            path: self.current_path.clone(),
            is_factory: self.location_flags.contains(Flags::IS_FACTORY_CONTENT),
            is_user: self.location_flags.contains(Flags::IS_USER_CONTENT),
            ..Default::default()
        });
        Ok(())
    }

    fn add_plugin_id(&mut self, plugin_id: UniversalPluginId) {
        if let Some(preset) = self.current()
            && plugin_id.abi.to_bytes() == b"clap"
        {
            preset
                .plugin_ids
                .push(plugin_id.id.to_string_lossy().into_owned());
        }
    }

    fn set_soundpack_id(&mut self, _soundpack_id: &CStr) {}

    fn set_flags(&mut self, flags: Flags) {
        if let Some(preset) = self.current() {
            preset.is_factory = flags.contains(Flags::IS_FACTORY_CONTENT);
            preset.is_user = flags.contains(Flags::IS_USER_CONTENT);
        }
    }

    fn add_creator(&mut self, creator: &CStr) {
        if let Some(preset) = self.current() {
            preset.creators.push(creator.to_string_lossy().into_owned());
        }
    }

    fn set_description(&mut self, description: &CStr) {
        if let Some(preset) = self.current() {
            preset.description = Some(description.to_string_lossy().into_owned());
        }
    }

    fn set_timestamps(&mut self, _creation: Option<Timestamp>, _modification: Option<Timestamp>) {}

    fn add_feature(&mut self, feature: &CStr) {
        if let Some(preset) = self.current() {
            preset.features.push(feature.to_string_lossy().into_owned());
        }
    }

    fn add_extra_info(&mut self, _key: &CStr, _value: &CStr) {}
}

#[derive(Debug)]
pub enum PresetDiscoveryError {
    NoFactory,
    Provider(ProviderInstanceError),
}

impl fmt::Display for PresetDiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PresetDiscoveryError::NoFactory => {
                write!(f, "plugin has no preset discovery factory")
            }
            PresetDiscoveryError::Provider(e) => write!(f, "preset provider failed: {e:?}"),
        }
    }
}

impl Error for PresetDiscoveryError {}

/// Discovers every preset the plugin's providers can enumerate. Directory
/// locations are walked recursively; files are filtered by the declared
/// extensions (or all files when none were declared).
pub fn discover_presets(
    entry: &PluginEntry,
    host_info: &HostInfo,
) -> Result<Vec<DiscoveredPreset>, PresetDiscoveryError> {
    let factory: PresetDiscoveryFactory =
        entry.get_factory().ok_or(PresetDiscoveryError::NoFactory)?;

    let mut receiver = CollectingReceiver::default();

    let provider_ids: Vec<CString> = factory
        .provider_descriptors()
        .filter_map(|d| d.id().map(CString::from))
        .collect();

    for provider_id in &provider_ids {
        let mut provider =
            Provider::instantiate(CollectingIndexer::default(), entry, provider_id, host_info)
                .map_err(PresetDiscoveryError::Provider)?;

        let locations = provider.indexer().locations.clone();
        let extensions = provider.indexer().file_extensions.clone();

        for (path, flags) in locations {
            receiver.location_flags = flags;
            match path {
                None => {
                    receiver.current_path = None;
                    provider.get_metadata(Location::Plugin, &mut receiver);
                }
                Some(root) => {
                    let mut files = Vec::new();
                    collect_files(&root, &extensions, &mut files);
                    for file in files {
                        let Ok(c_path) = CString::new(file.display().to_string()) else {
                            continue;
                        };
                        receiver.current_path = Some(file);
                        provider.get_metadata(Location::File { path: &c_path }, &mut receiver);
                    }
                }
            }
        }
    }

    Ok(receiver.presets)
}

/// Recursively collects files under `root` matching the declared
/// extensions (case-insensitive; empty list matches everything). A `root`
/// that is a plain file is taken as-is.
fn collect_files(root: &Path, extensions: &[String], out: &mut Vec<PathBuf>) {
    if root.is_file() {
        if matches_extension(root, extensions) {
            out.push(root.to_path_buf());
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for entry in entries {
        if entry.is_dir() {
            collect_files(&entry, extensions, out);
        } else if matches_extension(&entry, extensions) {
            out.push(entry);
        }
    }
}

fn matches_extension(path: &Path, extensions: &[String]) -> bool {
    if extensions.is_empty() {
        return true;
    }
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .is_some_and(|e| extensions.contains(&e))
}

/// Loads a preset into a live plugin instance (main-thread operation).
pub fn load_preset(
    instance: &mut PluginInstance<BenchHost>,
    path: Option<&Path>,
    load_key: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let handle = instance.plugin_handle();
    let preset_load = handle
        .get_extension::<PluginPresetLoad>()
        .ok_or("plugin does not support clap.preset-load")?;

    let c_path = path
        .map(|p| CString::new(p.display().to_string()))
        .transpose()?;
    let c_key = load_key.map(CString::new).transpose()?;

    let location = match &c_path {
        Some(path) => Location::File { path },
        None => Location::Plugin,
    };
    preset_load.load_from_location(&handle, location, c_key.as_deref())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_matching() {
        let exts = vec!["fxp".to_string()];
        assert!(matches_extension(Path::new("/x/a.fxp"), &exts));
        assert!(matches_extension(Path::new("/x/a.FXP"), &exts));
        assert!(!matches_extension(Path::new("/x/a.wav"), &exts));
        assert!(matches_extension(Path::new("/x/a.anything"), &[]));
    }
}
