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

/// Scans the standard CLAP paths, returning plugins and per-file load
/// failures (e.g. a binary built against a newer glibc).
pub fn scan_all_with_errors() -> (Vec<FoundPlugin>, Vec<(PathBuf, String)>) {
    let paths = clack_finder::standard_clap_paths();
    let mut found = Vec::new();
    let mut failures = Vec::new();
    for file in ClapFinder::new(paths) {
        let path = file.bundle_path();
        match try_list_plugins_in_file(path) {
            Ok(plugins) => found.extend(plugins),
            Err(e) => failures.push((path.to_path_buf(), e)),
        }
    }
    (found, failures)
}

fn try_list_plugins_in_file(path: &Path) -> Result<Vec<FoundPlugin>, String> {
    // SAFETY: loading a plugin means running arbitrary library init code;
    // this is inherent to hosting. Same approach as the clack example.
    let entry = unsafe { PluginEntry::load(path) }.map_err(|e| e.to_string())?;
    let Some(factory) = entry.get_plugin_factory() else {
        return Err("no plugin factory".to_string());
    };
    Ok(factory
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
        .collect())
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

/// Finds a single plugin by case-insensitive match on its id or name.
/// Substring matches are accepted when unambiguous; with several substring
/// matches, an exact id or name match wins (so "Surge XT" selects Surge XT
/// even though "Surge XT Effects" also contains it).
pub fn find_plugin(matcher: &str) -> Result<FoundPlugin, DiscoveryError> {
    let mut plugins = scan_all();
    let index = select_match(
        matcher,
        &plugins
            .iter()
            .map(|p| (p.id.clone(), p.name.clone()))
            .collect::<Vec<_>>(),
    )?;
    Ok(plugins.swap_remove(index))
}

/// Pure matching logic behind [`find_plugin`], on (id, name) pairs.
fn select_match(
    matcher: &str,
    candidates: &[(String, Option<String>)],
) -> Result<usize, DiscoveryError> {
    let matcher_lower = matcher.to_lowercase();
    let matches: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, (id, name))| {
            id.to_lowercase().contains(&matcher_lower)
                || name
                    .as_deref()
                    .is_some_and(|n| n.to_lowercase().contains(&matcher_lower))
        })
        .map(|(i, _)| i)
        .collect();

    match matches.as_slice() {
        [] => Err(DiscoveryError::NoMatch(matcher.to_string())),
        [single] => Ok(*single),
        several => {
            let exact: Vec<usize> = several
                .iter()
                .copied()
                .filter(|&i| {
                    let (id, name) = &candidates[i];
                    id.to_lowercase() == matcher_lower
                        || name
                            .as_deref()
                            .is_some_and(|n| n.to_lowercase() == matcher_lower)
                })
                .collect();
            if let [single] = exact.as_slice() {
                return Ok(*single);
            }
            Err(DiscoveryError::Ambiguous(
                matcher.to_string(),
                several.iter().map(|&i| candidates[i].0.clone()).collect(),
            ))
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates() -> Vec<(String, Option<String>)> {
        vec![
            (
                "org.surge-synth-team.surge-xt".to_string(),
                Some("Surge XT".to_string()),
            ),
            (
                "org.surge-synth-team.surge-xt-fx".to_string(),
                Some("Surge XT Effects".to_string()),
            ),
        ]
    }

    #[test]
    fn exact_name_wins_over_ambiguous_substring() {
        assert_eq!(select_match("Surge XT", &candidates()).unwrap(), 0);
        assert_eq!(select_match("surge xt effects", &candidates()).unwrap(), 1);
        assert_eq!(
            select_match("org.surge-synth-team.surge-xt", &candidates()).unwrap(),
            0
        );
    }

    #[test]
    fn ambiguous_substring_errors_with_ids() {
        match select_match("surge", &candidates()) {
            Err(DiscoveryError::Ambiguous(_, ids)) => assert_eq!(ids.len(), 2),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn no_match_errors() {
        assert!(matches!(
            select_match("vital", &candidates()),
            Err(DiscoveryError::NoMatch(_))
        ));
    }

    #[test]
    fn single_substring_match_is_accepted() {
        assert_eq!(select_match("effects", &candidates()).unwrap(), 1);
    }
}
