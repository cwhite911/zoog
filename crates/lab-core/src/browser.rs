// SPDX-License-Identifier: GPL-3.0-only
// SPDX-FileCopyrightText: 2026 Corey T. White

//! Preset browsing state: an ordered list with a cursor and a category
//! filter, driven by the main encoder (turn scrolls, Shift+turn cycles the
//! category, click loads). Shared by the hardware browse path and, later,
//! the GUI.

use std::path::PathBuf;

/// One browsable preset (library row projected for browsing/loading).
#[derive(Debug, Clone, PartialEq)]
pub struct BrowseItem {
    pub id: i64,
    pub name: String,
    pub category: Option<String>,
    pub path: Option<PathBuf>,
    pub load_key: Option<String>,
}

/// Cursor + category filter over a list of presets. The list order is
/// whatever the caller provides (typically category, then name).
pub struct Browser {
    items: Vec<BrowseItem>,
    categories: Vec<String>,
    /// 0 = all categories, otherwise 1-based index into `categories`.
    category_index: usize,
    filtered: Vec<usize>,
    cursor: usize,
}

impl Browser {
    pub fn new(items: Vec<BrowseItem>) -> Self {
        let mut categories: Vec<String> = items.iter().filter_map(|i| i.category.clone()).collect();
        categories.sort();
        categories.dedup();
        let mut browser = Browser {
            items,
            categories,
            category_index: 0,
            filtered: Vec::new(),
            cursor: 0,
        };
        browser.rebuild();
        browser
    }

    pub fn is_empty(&self) -> bool {
        self.filtered.is_empty()
    }

    fn rebuild(&mut self) {
        let category = self.category_name_option();
        self.filtered = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| category.is_none_or(|c| item.category.as_deref() == Some(c)))
            .map(|(i, _)| i)
            .collect();
        self.cursor = 0;
    }

    fn category_name_option(&self) -> Option<&str> {
        if self.category_index == 0 {
            None
        } else {
            self.categories
                .get(self.category_index - 1)
                .map(String::as_str)
        }
    }

    pub fn category_name(&self) -> &str {
        self.category_name_option().unwrap_or("All")
    }

    /// Moves the cursor by the sign of `delta`, wrapping. Returns the newly
    /// selected item. Zero deltas (the relative encoder's idle half of each
    /// detent pair) do nothing.
    pub fn scroll(&mut self, delta: i8) -> Option<&BrowseItem> {
        if self.filtered.is_empty() || delta == 0 {
            return None;
        }
        let len = self.filtered.len();
        self.cursor = match delta.signum() {
            1 => (self.cursor + 1) % len,
            _ => (self.cursor + len - 1) % len,
        };
        self.current()
    }

    /// Cycles the category filter by the sign of `delta` (position 0 is
    /// "All"), resetting the cursor. Returns the new category name.
    pub fn cycle_category(&mut self, delta: i8) -> &str {
        if delta != 0 && !self.categories.is_empty() {
            let count = self.categories.len() + 1;
            self.category_index = match delta.signum() {
                1 => (self.category_index + 1) % count,
                _ => (self.category_index + count - 1) % count,
            };
            self.rebuild();
        }
        self.category_name()
    }

    pub fn current(&self) -> Option<&BrowseItem> {
        self.filtered
            .get(self.cursor)
            .and_then(|&i| self.items.get(i))
    }

    /// (1-based position, filtered length).
    pub fn position(&self) -> (usize, usize) {
        (self.cursor + 1, self.filtered.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: i64, name: &str, category: &str) -> BrowseItem {
        BrowseItem {
            id,
            name: name.to_string(),
            category: Some(category.to_string()),
            path: None,
            load_key: None,
        }
    }

    fn browser() -> Browser {
        Browser::new(vec![
            item(1, "Big Bass", "Basses"),
            item(2, "Soft Bass", "Basses"),
            item(3, "Bright Lead", "Leads"),
            item(4, "Warm Pad", "Pads"),
        ])
    }

    #[test]
    fn scrolls_and_wraps() {
        let mut b = browser();
        assert_eq!(b.current().unwrap().id, 1);
        assert_eq!(b.scroll(1).unwrap().id, 2);
        assert_eq!(b.scroll(2).unwrap().id, 3);
        assert_eq!(b.scroll(1).unwrap().id, 4);
        assert_eq!(b.scroll(1).unwrap().id, 1);
        assert_eq!(b.scroll(-1).unwrap().id, 4);
        assert_eq!(b.position(), (4, 4));
        // Zero delta (the idle half of a detent pair) is a no-op.
        assert!(b.scroll(0).is_none());
        assert_eq!(b.current().unwrap().id, 4);
    }

    #[test]
    fn category_cycling_filters_and_wraps() {
        let mut b = browser();
        assert_eq!(b.category_name(), "All");
        assert_eq!(b.cycle_category(1), "Basses");
        assert_eq!(b.position(), (1, 2));
        assert_eq!(b.current().unwrap().id, 1);
        assert_eq!(b.cycle_category(1), "Leads");
        assert_eq!(b.current().unwrap().id, 3);
        assert_eq!(b.cycle_category(1), "Pads");
        assert_eq!(b.cycle_category(1), "All");
        assert_eq!(b.position(), (1, 4));
        assert_eq!(b.cycle_category(-1), "Pads");
    }

    #[test]
    fn empty_browser_is_inert() {
        let mut b = Browser::new(vec![]);
        assert!(b.is_empty());
        assert!(b.scroll(1).is_none());
        assert_eq!(b.cycle_category(1), "All");
        assert!(b.current().is_none());
    }
}
