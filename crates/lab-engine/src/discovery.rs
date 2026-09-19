//! CLAP plugin discovery: scan the standard search paths (including
//! `$CLAP_PATH`, handled by clack-finder) and load plugin entries.

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use clack_finder::ClapFinder;
use clack_host::prelude::*;

/// A plugin found on disk, with its loaded entry.
pub struct FoundPlugin {
    pub id: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub path: PathBuf,
    pub entry: PluginEntry,
}

impl fmt::Display for FoundPlugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.name, &self.version) {
            (Some(name), Some(version)) => write!(f, "{name} ({}) v{version}", self.id),
            (Some(name), None) => write!(f, "{name} ({})", self.id),
            _ => write!(f, "{}", self.id),
        }
    }
}

/// Scans the standard CLAP paths and returns every plugin descriptor found.
/// Files that fail to load are skipped silently (they may not be CLAPs).
pub fn scan_all() -> Vec<FoundPlugin> {
    let paths = clack_finder::standard_clap_paths();
    let mut found = Vec::new();
    for file in ClapFinder::new(paths) {
        found.extend(list_plugins_in_file(file.bundle_path()));
    }
    found
}

/// Lists the plugins in one CLAP file. Returns an empty list when the file
/// cannot be loaded or has no plugin factory.
pub fn list_plugins_in_file(path: &Path) -> Vec<FoundPlugin> {
    // SAFETY: loading a plugin means running arbitrary library init code;
    // this is inherent to hosting. Same approach as the clack example.
    let Ok(entry) = (unsafe { PluginEntry::load(path) }) else {
        return Vec::new();
    };
    let Some(factory) = entry.get_plugin_factory() else {
        return Vec::new();
    };

    factory
        .plugin_descriptors()
        .filter_map(|descriptor| {
            Some(FoundPlugin {
                id: descriptor.id()?.to_str().ok()?.to_string(),
                name: descriptor.name().map(|n| n.to_string_lossy().to_string()),
                version: descriptor
                    .version()
                    .map(|v| v.to_string_lossy().to_string()),
                path: path.to_path_buf(),
                entry: entry.clone(),
            })
        })
        .collect()
}

/// Finds a single plugin by case-insensitive substring match on its id or
/// name. Exactly one match is required.
pub fn find_plugin(matcher: &str) -> Result<FoundPlugin, DiscoveryError> {
    let matcher_lower = matcher.to_lowercase();
    let mut matches: Vec<FoundPlugin> = scan_all()
        .into_iter()
        .filter(|p| {
            p.id.to_lowercase().contains(&matcher_lower)
                || p.name
                    .as_deref()
                    .is_some_and(|n| n.to_lowercase().contains(&matcher_lower))
        })
        .collect();

    match matches.len() {
        0 => Err(DiscoveryError::NoMatch(matcher.to_string())),
        1 => Ok(matches.pop().unwrap()),
        _ => Err(DiscoveryError::Ambiguous(
            matcher.to_string(),
            matches.iter().map(|p| p.id.clone()).collect(),
        )),
    }
}

#[derive(Debug)]
pub enum DiscoveryError {
    NoMatch(String),
    Ambiguous(String, Vec<String>),
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiscoveryError::NoMatch(m) => {
                write!(f, "no CLAP plugin matching {m:?} in the search paths")
            }
            DiscoveryError::Ambiguous(m, ids) => {
                write!(f, "multiple CLAP plugins match {m:?}: {}", ids.join(", "))
            }
        }
    }
}

impl Error for DiscoveryError {}
