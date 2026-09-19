//! `labctl presets`: scan, list, search, load, favorite.

use std::error::Error;
use std::path::Path;
use std::process::ExitCode;

use lab_engine::discovery::{FoundPlugin, find_plugin};
use lab_engine::host::host_info;
use lab_engine::presets::{DiscoveredPreset, discover_presets, load_preset};
use lab_library::{Filter, ImportPreset, Library, PresetRow};

const DEFAULT_PLUGIN: &str = "Surge XT";

const USAGE: &str = "\
labctl presets subcommands:
    scan [--plugin MATCH]         discover and index the plugin's presets
    list [--category C] [--favorites]
    search QUERY
    categories
    load <ID|NAME>                load into a fresh instance (headless check)
    favorite <ID> [on|off]
";

pub fn cmd_presets(args: &[String]) -> ExitCode {
    let result = match args.first().map(String::as_str) {
        Some("scan") => cmd_scan(&args[1..]),
        Some("list") => cmd_list(&args[1..]),
        Some("search") => cmd_search(&args[1..]),
        Some("categories") => cmd_categories(),
        Some("load") => cmd_load(&args[1..]),
        Some("favorite") => cmd_favorite(&args[1..]),
        _ => {
            print!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("labctl presets: {e}");
            ExitCode::FAILURE
        }
    }
}

pub fn open_library() -> Result<Library, Box<dyn Error>> {
    let path = Library::default_path().ok_or("cannot determine XDG data directory")?;
    Ok(Library::open(&path)?)
}

/// Converts a discovery result to a library row. The browsing category is
/// the preset file's parent directory name (Surge's factory tree is
/// organized that way); falls back to the first reported feature.
fn to_import(preset: DiscoveredPreset) -> ImportPreset {
    let category = preset
        .path
        .as_deref()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .or_else(|| preset.features.first().cloned());
    let mtime = preset.path.as_deref().and_then(file_mtime);
    ImportPreset {
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
}

fn file_mtime(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(
        modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as i64,
    )
}

/// Discovers and indexes the plugin's presets. Returns the number indexed.
pub fn scan_into_library(
    library: &mut Library,
    plugin: &FoundPlugin,
) -> Result<usize, Box<dyn Error>> {
    let discovered = discover_presets(&plugin.entry, &host_info())?;
    // Keep only presets usable by this plugin (providers may serve several).
    let imports: Vec<ImportPreset> = discovered
        .into_iter()
        .filter(|p| p.plugin_ids.is_empty() || p.plugin_ids.contains(&plugin.id))
        .map(to_import)
        .collect();
    Ok(library.rescan(&plugin.id, &imports)?)
}

/// Ensures the library has an up-to-date index for `plugin`, rescanning
/// when empty or stale (the incremental check compares path+mtime sets).
pub fn ensure_indexed(library: &mut Library, plugin: &FoundPlugin) -> Result<(), Box<dyn Error>> {
    let count = library.count(&plugin.id)?;
    if count == 0 {
        println!("library empty for {}; scanning presets...", plugin.id);
        let n = scan_into_library(library, plugin)?;
        println!("indexed {n} presets");
    }
    Ok(())
}

fn cmd_scan(args: &[String]) -> Result<(), Box<dyn Error>> {
    let plugin_match = match args {
        [] => DEFAULT_PLUGIN.to_string(),
        [flag, value] if flag == "--plugin" => value.clone(),
        _ => return Err("usage: labctl presets scan [--plugin MATCH]".into()),
    };
    let plugin = find_plugin(&plugin_match)?;
    let mut library = open_library()?;
    println!("scanning presets for {plugin}...");
    let count = scan_into_library(&mut library, &plugin)?;
    println!("indexed {count} presets for {}", plugin.id);
    let categories = library.categories(&plugin.id)?;
    println!("{} categories: {}", categories.len(), categories.join(", "));
    Ok(())
}

fn print_rows(rows: &[PresetRow]) {
    for row in rows {
        println!(
            "  {:>6}  {:<32} {:<16} {}{}",
            row.id,
            row.name,
            row.category.as_deref().unwrap_or("-"),
            if row.favorite { "* " } else { "" },
            row.path
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        );
    }
    println!("{} presets", rows.len());
}

fn cmd_list(args: &[String]) -> Result<(), Box<dyn Error>> {
    let mut filter = Filter::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--category" => {
                filter.category = Some(it.next().ok_or("--category needs a value")?.clone());
            }
            "--favorites" => filter.favorites_only = true,
            other => return Err(format!("unknown argument {other:?}").into()),
        }
    }
    print_rows(&open_library()?.search(&filter)?);
    Ok(())
}

fn cmd_search(args: &[String]) -> Result<(), Box<dyn Error>> {
    let [query] = args else {
        return Err("usage: labctl presets search QUERY".into());
    };
    let filter = Filter {
        query: Some(query.clone()),
        ..Default::default()
    };
    print_rows(&open_library()?.search(&filter)?);
    Ok(())
}

fn cmd_categories() -> Result<(), Box<dyn Error>> {
    let library = open_library()?;
    let plugin = find_plugin(DEFAULT_PLUGIN)?;
    for category in library.categories(&plugin.id)? {
        println!("{category}");
    }
    Ok(())
}

fn find_preset(library: &Library, key: &str) -> Result<PresetRow, Box<dyn Error>> {
    if let Ok(id) = key.parse::<i64>()
        && let Some(row) = library.get(id)?
    {
        return Ok(row);
    }
    let rows = library.search(&Filter {
        query: Some(key.to_string()),
        ..Default::default()
    })?;
    match rows.len() {
        0 => Err(format!("no preset matches {key:?}").into()),
        1 => Ok(rows.into_iter().next().unwrap()),
        n => Err(format!("{n} presets match {key:?}; use the ID").into()),
    }
}

fn cmd_load(args: &[String]) -> Result<(), Box<dyn Error>> {
    let [key] = args else {
        return Err("usage: labctl presets load <ID|NAME>".into());
    };
    let library = open_library()?;
    let row = find_preset(&library, key)?;
    let plugin = find_plugin(&row.engine)?;
    let (mut instance, _host_rx) = crate::hostutil::make_instance(&plugin)?;
    load_preset(&mut instance, row.path.as_deref(), row.load_key.as_deref())?;
    println!("loaded {:?} ({}) into {}", row.name, row.id, plugin.id);
    Ok(())
}

fn cmd_favorite(args: &[String]) -> Result<(), Box<dyn Error>> {
    let (key, on) = match args {
        [key] => (key, true),
        [key, state] if state == "on" => (key, true),
        [key, state] if state == "off" => (key, false),
        _ => return Err("usage: labctl presets favorite <ID> [on|off]".into()),
    };
    let library = open_library()?;
    let row = find_preset(&library, key)?;
    library.set_favorite(row.id, on)?;
    println!(
        "{} favorite: {:?} ({})",
        if on { "set" } else { "cleared" },
        row.name,
        row.id
    );
    Ok(())
}
