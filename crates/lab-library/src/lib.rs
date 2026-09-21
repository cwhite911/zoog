// SPDX-License-Identifier: GPL-3.0-only
// SPDX-FileCopyrightText: 2026 Corey T. White

//! Preset index: SQLite-backed library with categories, favorites, search,
//! and rescans. Deliberately independent of the CLAP stack; callers convert
//! discovery results into [`ImportPreset`].

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

/// A preset as handed to the library by a scanner.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportPreset {
    pub name: String,
    pub path: Option<PathBuf>,
    pub load_key: Option<String>,
    /// Primary browsing category. Scanners typically derive it from the
    /// preset's parent directory or first feature; the library does not
    /// invent one.
    pub category: Option<String>,
    /// Raw feature/tag strings as reported by discovery.
    pub features: Vec<String>,
    pub creators: Vec<String>,
    pub description: Option<String>,
    pub is_factory: bool,
    /// File modification time (seconds), for incremental rescans.
    pub mtime: Option<i64>,
}

/// A stored preset row.
#[derive(Debug, Clone, PartialEq)]
pub struct PresetRow {
    pub id: i64,
    pub engine: String,
    pub name: String,
    pub path: Option<PathBuf>,
    pub load_key: Option<String>,
    pub category: Option<String>,
    pub features: Vec<String>,
    pub creators: Vec<String>,
    pub description: Option<String>,
    pub is_factory: bool,
    pub favorite: bool,
    pub last_used: Option<i64>,
}

/// Search filters. Empty/None fields match everything.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub engine: Option<String>,
    /// Case-insensitive substring of the preset name.
    pub query: Option<String>,
    pub category: Option<String>,
    pub favorites_only: bool,
}

pub type Result<T> = std::result::Result<T, rusqlite::Error>;

pub struct Library {
    conn: Connection,
}

impl Library {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    /// The default on-disk location under XDG data
    /// (`~/.local/share/zoog/library.sqlite3`).
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "zoog")
            .map(|dirs| dirs.data_dir().join("library.sqlite3"))
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS presets (
                id INTEGER PRIMARY KEY,
                engine TEXT NOT NULL,
                name TEXT NOT NULL,
                path TEXT,
                load_key TEXT,
                category TEXT,
                features TEXT NOT NULL DEFAULT '',
                creators TEXT NOT NULL DEFAULT '',
                description TEXT,
                is_factory INTEGER NOT NULL DEFAULT 0,
                favorite INTEGER NOT NULL DEFAULT 0,
                last_used INTEGER,
                mtime INTEGER
            );
            CREATE INDEX IF NOT EXISTS presets_engine_name
                ON presets (engine, name);
            CREATE INDEX IF NOT EXISTS presets_engine_category
                ON presets (engine, category);",
        )?;
        Ok(Library { conn })
    }

    /// Full rescan for one engine: replaces all of its rows while
    /// preserving favorite flags and last-used stamps of presets that still
    /// exist (matched on path + load key + name).
    pub fn rescan(&mut self, engine: &str, presets: &[ImportPreset]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        {
            // Preserve user state across the wipe.
            let mut preserved: Vec<(String, bool, Option<i64>)> = Vec::new();
            let mut stmt = tx.prepare(
                "SELECT COALESCE(path,'') || '\u{1}' || COALESCE(load_key,'') || '\u{1}' || name,
                        favorite, last_used
                 FROM presets WHERE engine = ?1 AND (favorite != 0 OR last_used IS NOT NULL)",
            )?;
            let rows = stmt.query_map(params![engine], |row| {
                Ok((row.get(0)?, row.get::<_, i64>(1)? != 0, row.get(2)?))
            })?;
            for row in rows {
                preserved.push(row?);
            }
            drop(stmt);

            tx.execute("DELETE FROM presets WHERE engine = ?1", params![engine])?;

            let mut insert = tx.prepare(
                "INSERT INTO presets
                   (engine, name, path, load_key, category, features, creators,
                    description, is_factory, favorite, last_used, mtime)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?;
            for preset in presets {
                let key = format!(
                    "{}\u{1}{}\u{1}{}",
                    preset.path.as_deref().map(path_str).unwrap_or_default(),
                    preset.load_key.as_deref().unwrap_or_default(),
                    preset.name
                );
                let (favorite, last_used) = preserved
                    .iter()
                    .find(|(k, ..)| *k == key)
                    .map(|(_, fav, used)| (*fav, *used))
                    .unwrap_or((false, None));
                insert.execute(params![
                    engine,
                    preset.name,
                    preset.path.as_deref().map(path_str),
                    preset.load_key,
                    preset.category,
                    preset.features.join("\u{1}"),
                    preset.creators.join("\u{1}"),
                    preset.description,
                    preset.is_factory as i64,
                    favorite as i64,
                    last_used,
                    preset.mtime,
                ])?;
            }
        }
        tx.commit()?;
        Ok(presets.len())
    }

    /// Whether a rescan is needed: true when the stored (path, mtime) set
    /// differs from what the scanner would index now. This is the cheap
    /// incremental path; a full rescan follows only when it returns true.
    pub fn needs_rescan(&self, engine: &str, current: &[(PathBuf, i64)]) -> Result<bool> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, mtime FROM presets WHERE engine = ?1 AND path IS NOT NULL")?;
        let mut stored: Vec<(String, Option<i64>)> = Vec::new();
        for row in stmt.query_map(params![engine], |row| Ok((row.get(0)?, row.get(1)?)))? {
            stored.push(row?);
        }
        if stored.is_empty() && !current.is_empty() {
            return Ok(true);
        }
        let mut stored_set: Vec<(String, i64)> = stored
            .into_iter()
            .map(|(p, m)| (p, m.unwrap_or(-1)))
            .collect();
        stored_set.sort();
        stored_set.dedup();
        let mut current_set: Vec<(String, i64)> =
            current.iter().map(|(p, m)| (path_str(p), *m)).collect();
        current_set.sort();
        current_set.dedup();
        Ok(stored_set != current_set)
    }

    pub fn search(&self, filter: &Filter) -> Result<Vec<PresetRow>> {
        let mut sql = String::from(
            "SELECT id, engine, name, path, load_key, category, features,
                    creators, description, is_factory, favorite, last_used
             FROM presets WHERE 1=1",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(engine) = &filter.engine {
            sql.push_str(" AND engine = ?");
            args.push(Box::new(engine.clone()));
        }
        if let Some(query) = &filter.query {
            sql.push_str(" AND name LIKE ? ESCAPE '\\'");
            args.push(Box::new(format!("%{}%", escape_like(query))));
        }
        if let Some(category) = &filter.category {
            sql.push_str(" AND category = ?");
            args.push(Box::new(category.clone()));
        }
        if filter.favorites_only {
            sql.push_str(" AND favorite != 0");
        }
        sql.push_str(" ORDER BY category, name COLLATE NOCASE");

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
            row_to_preset,
        )?;
        rows.collect()
    }

    pub fn categories(&self, engine: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT category FROM presets
             WHERE engine = ?1 AND category IS NOT NULL
             ORDER BY category COLLATE NOCASE",
        )?;
        let rows = stmt.query_map(params![engine], |row| row.get(0))?;
        rows.collect()
    }

    pub fn set_favorite(&self, id: i64, favorite: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE presets SET favorite = ?2 WHERE id = ?1",
            params![id, favorite as i64],
        )?;
        Ok(())
    }

    /// Stamps a preset as used now (unix seconds provided by the caller).
    pub fn touch_last_used(&self, id: i64, unix_seconds: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE presets SET last_used = ?2 WHERE id = ?1",
            params![id, unix_seconds],
        )?;
        Ok(())
    }

    pub fn get(&self, id: i64) -> Result<Option<PresetRow>> {
        self.conn
            .query_row(
                "SELECT id, engine, name, path, load_key, category, features,
                        creators, description, is_factory, favorite, last_used
                 FROM presets WHERE id = ?1",
                params![id],
                row_to_preset,
            )
            .optional()
    }

    pub fn count(&self, engine: &str) -> Result<i64> {
        self.conn.query_row(
            "SELECT COUNT(*) FROM presets WHERE engine = ?1",
            params![engine],
            |row| row.get(0),
        )
    }
}

fn path_str(path: &Path) -> String {
    path.display().to_string()
}

fn escape_like(query: &str) -> String {
    query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn split_list(joined: String) -> Vec<String> {
    if joined.is_empty() {
        Vec::new()
    } else {
        joined.split('\u{1}').map(str::to_string).collect()
    }
}

fn row_to_preset(row: &rusqlite::Row) -> rusqlite::Result<PresetRow> {
    Ok(PresetRow {
        id: row.get(0)?,
        engine: row.get(1)?,
        name: row.get(2)?,
        path: row.get::<_, Option<String>>(3)?.map(PathBuf::from),
        load_key: row.get(4)?,
        category: row.get(5)?,
        features: split_list(row.get(6)?),
        creators: split_list(row.get(7)?),
        description: row.get(8)?,
        is_factory: row.get::<_, i64>(9)? != 0,
        favorite: row.get::<_, i64>(10)? != 0,
        last_used: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(name: &str, category: &str, path: &str) -> ImportPreset {
        ImportPreset {
            name: name.to_string(),
            category: Some(category.to_string()),
            path: Some(PathBuf::from(path)),
            mtime: Some(100),
            is_factory: true,
            ..Default::default()
        }
    }

    fn sample_library() -> Library {
        let mut lib = Library::open_in_memory().unwrap();
        lib.rescan(
            "org.example",
            &[
                preset("Warm Pad", "Pads", "/p/Pads/Warm Pad.fxp"),
                preset("Big Bass", "Basses", "/p/Basses/Big Bass.fxp"),
                preset("Soft Bass", "Basses", "/p/Basses/Soft Bass.fxp"),
            ],
        )
        .unwrap();
        lib
    }

    #[test]
    fn search_by_name_and_category() {
        let lib = sample_library();
        let all = lib.search(&Filter::default()).unwrap();
        assert_eq!(all.len(), 3);

        let bass = lib
            .search(&Filter {
                query: Some("bass".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(bass.len(), 2);

        let pads = lib
            .search(&Filter {
                category: Some("Pads".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(pads.len(), 1);
        assert_eq!(pads[0].name, "Warm Pad");

        assert_eq!(lib.categories("org.example").unwrap(), ["Basses", "Pads"]);
    }

    #[test]
    fn like_wildcards_are_escaped() {
        let lib = sample_library();
        let none = lib
            .search(&Filter {
                query: Some("%".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn favorites_survive_rescan() {
        let mut lib = sample_library();
        let row = &lib
            .search(&Filter {
                query: Some("Warm".to_string()),
                ..Default::default()
            })
            .unwrap()[0];
        lib.set_favorite(row.id, true).unwrap();
        lib.touch_last_used(row.id, 1_700_000_000).unwrap();

        // Rescan with the same content plus one new preset.
        lib.rescan(
            "org.example",
            &[
                preset("Warm Pad", "Pads", "/p/Pads/Warm Pad.fxp"),
                preset("Big Bass", "Basses", "/p/Basses/Big Bass.fxp"),
                preset("New Lead", "Leads", "/p/Leads/New Lead.fxp"),
            ],
        )
        .unwrap();

        let favorites = lib
            .search(&Filter {
                favorites_only: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(favorites.len(), 1);
        assert_eq!(favorites[0].name, "Warm Pad");
        assert_eq!(favorites[0].last_used, Some(1_700_000_000));
        // The removed preset is gone.
        assert_eq!(lib.count("org.example").unwrap(), 3);
    }

    #[test]
    fn needs_rescan_detects_change() {
        let lib = sample_library();
        let unchanged = vec![
            (PathBuf::from("/p/Pads/Warm Pad.fxp"), 100),
            (PathBuf::from("/p/Basses/Big Bass.fxp"), 100),
            (PathBuf::from("/p/Basses/Soft Bass.fxp"), 100),
        ];
        assert!(!lib.needs_rescan("org.example", &unchanged).unwrap());

        let mut touched = unchanged.clone();
        touched[0].1 = 200;
        assert!(lib.needs_rescan("org.example", &touched).unwrap());

        let mut removed = unchanged;
        removed.pop();
        assert!(lib.needs_rescan("org.example", &removed).unwrap());
    }

    #[test]
    fn engines_are_isolated() {
        let mut lib = sample_library();
        lib.rescan("org.other", &[preset("Elsewhere", "X", "/q/x.fxp")])
            .unwrap();
        assert_eq!(lib.count("org.example").unwrap(), 3);
        assert_eq!(lib.count("org.other").unwrap(), 1);
        lib.rescan("org.other", &[]).unwrap();
        assert_eq!(lib.count("org.example").unwrap(), 3);
    }
}
