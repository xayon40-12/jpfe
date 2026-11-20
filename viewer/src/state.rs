//! What the app remembers between sessions (`state.json` in its storage), and the clock its records are
//! stamped with, so that two devices' copies can be merged record by record (see [`crate::sync`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::book::{SavedBook, Settings};
use crate::reader::{self, BookReaderState};

/// Log scale of each plot by default, in storage order: new words, characters, density, words learned so far.
pub const DEFAULT_LOG_SCALES: [bool; 4] = [true; 4];

/// Everything remembered between sessions, besides the files and analyses.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedState {
    pub settings: Settings,
    /// Log scale of each plot, in the order of [`DEFAULT_LOG_SCALES`] (a list, so states saved with fewer plots load)
    pub log_scales: Vec<bool>,
    pub shown: Option<u64>,
    pub next_id: u64,
    pub books: Vec<SavedBook>,
    /// Show load times and storage errors in the sidebar (this device only, never synced)
    pub debug: bool,
    pub reader_font_size: f32,
    /// Vertical text (縦書き) in the reader
    pub reader_vertical: bool,
    /// One page at a time in the reader, rather than scrolling
    pub reader_paginate: bool,
    /// Every word in the reader on a fill colored by its parts
    pub reader_highlight: bool,
    /// When the synced settings (the word-list settings, the plots' scales and the reader's settings) last
    /// changed, in milliseconds since 1970-01-01 UTC; 0 if they never changed since sync came
    pub settings_modified: u64,
    /// What the reader remembers of each book (where it was left), by book id
    pub reader_books: BTreeMap<u64, BookReaderState>,
    /// The view showing, reopened when the app starts again
    pub view: ViewState,
    /// Books removed, by hash, with when: kept so that another device does not bring them back, unless it
    /// adds the book again later
    pub removed: BTreeMap<String, u64>,
}

/// Which view was showing, so the app reopens it: the reader on the same book (at its saved place), or the
/// statistics. Saved with the settings; two copies of it (from two devices) merge by keeping the one
/// `modified` last.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewState {
    /// The book shown in the reader; `None` for the statistics
    pub reading: Option<u64>,
    /// When the view last changed, in milliseconds since 1970-01-01 UTC
    pub modified: u64,
}

impl Default for SavedState {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            log_scales: DEFAULT_LOG_SCALES.to_vec(),
            shown: None,
            next_id: 0,
            books: Vec::new(),
            debug: false,
            reader_font_size: reader::DEFAULT_FONT_SIZE,
            reader_vertical: reader::DEFAULT_VERTICAL,
            reader_paginate: reader::DEFAULT_PAGINATE,
            reader_highlight: reader::DEFAULT_HIGHLIGHT,
            settings_modified: 0,
            reader_books: BTreeMap::new(),
            view: ViewState::default(),
            removed: BTreeMap::new(),
        }
    }
}

impl SavedState {
    /// Read a saved state; a missing or unreadable one is the default. States saved before sync get the
    /// oldest possible time (1) on what was set then, so that it wins over a new device's defaults but loses
    /// to any change made since.
    pub fn load(bytes: Option<&[u8]>) -> Self {
        let mut state: SavedState = bytes.and_then(|b| serde_json::from_slice(b).ok()).unwrap_or_default();
        if state.settings_modified == 0 && !state.has_default_settings() {
            state.settings_modified = 1;
        }
        for book in &mut state.books {
            book.modified = book.modified.max(1);
        }
        state
    }

    /// Whether the synced settings are all at their defaults.
    pub fn has_default_settings(&self) -> bool {
        let default = SavedState::default();
        (self.settings, &self.log_scales, self.reader_font_size, self.reader_vertical, self.reader_paginate, self.reader_highlight)
            == (default.settings, &default.log_scales, default.reader_font_size, default.reader_vertical, default.reader_paginate, default.reader_highlight)
    }
}

/// Now, in milliseconds since 1970-01-01 UTC.
pub fn now_millis() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// The time to stamp a change of a record last stamped `previous` with: now, or just after `previous` if that
/// is later (another device's clock ahead of this one's). A change made here so always replaces the record it
/// changes, and a timestamp from the future never makes a record unchangeable.
pub fn stamp(previous: u64) -> u64 {
    stamp_at(now_millis(), previous)
}

pub fn stamp_at(now: u64, previous: u64) -> u64 {
    now.max(previous.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_saved_before_the_reader_remembered_places_load() {
        let old = r#"{"settings":{},"shown":0,"next_id":1,"books":[{"id":0,"name":"本","file_name":"本.epub","learned":2}],"reader_font_size":24.0,"reader_vertical":false}"#;
        let state = SavedState::load(Some(old.as_bytes()));
        assert_eq!(state.books.len(), 1);
        assert_eq!(state.reader_font_size, 24.0);
        assert!(state.reader_paginate);
        assert!(!state.reader_highlight);
        assert!(state.reader_books.is_empty());
        // The statistics, as before
        assert_eq!(state.view, ViewState::default());

        // Places saved with the book's progress, before the reader stopped using it
        let with_learned = r#"{"reader_books":{"7":{"chapter":3,"offset":120,"text":"探す文","learned":2,"modified":5}}}"#;
        let state = SavedState::load(Some(with_learned.as_bytes()));
        assert_eq!(state.reader_books.get(&7).map(|p| (p.chapter, p.offset)), Some((3, 120)));

        let mut state = SavedState::default();
        let place = BookReaderState { chapter: 3, offset: 120, text: "探す文".into(), modified: 1_800_000_000_000 };
        state.reader_books.insert(7, place.clone());
        state.view = ViewState { reading: Some(7), modified: 1_800_000_000_001 };
        let json = serde_json::to_string(&state).unwrap();
        let back: SavedState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.reader_books.get(&7), Some(&place));
        assert_eq!(back.view, state.view);
    }

    #[test]
    fn states_saved_before_sync_load_with_the_oldest_times() {
        // Changed settings and a book, without hashes or times
        let old = r#"{"settings":{"min_chapters":5},"next_id":1,"books":[{"id":0,"name":"本","file_name":"本.epub","learned":2}],"reader_books":{"0":{"chapter":1,"offset":0,"text":"","modified":1700000000000}}}"#;
        let state = SavedState::load(Some(old.as_bytes()));
        assert_eq!(state.settings_modified, 1);
        assert_eq!(state.books[0].hash, None);
        assert_eq!(state.books[0].modified, 1);
        assert!(state.removed.is_empty());
        // Settings left at their defaults were never set: a new device's synced settings win
        let state = SavedState::load(Some(br#"{"reader_font_size":20.0}"#));
        assert_eq!(state.settings_modified, 0);
        // Missing or damaged
        assert_eq!(SavedState::load(None), SavedState::default());
        assert_eq!(SavedState::load(Some(b"{not json")), SavedState::default());
        // A saved state keeps its times and hashes
        let mut state = SavedState::default();
        state.books.push(SavedBook { id: 3, hash: Some("ab".into()), modified: 42, ..SavedBook::default() });
        state.removed.insert("cd".into(), 7);
        state.settings_modified = 9;
        let back = SavedState::load(Some(&serde_json::to_vec(&state).unwrap()));
        assert_eq!(back, state);
    }

    #[test]
    fn a_change_always_replaces_the_record_it_changes() {
        assert_eq!(stamp_at(1000, 10), 1000);
        // Another device's clock was ahead: just after its time
        assert_eq!(stamp_at(1000, 5000), 5001);
        assert_eq!(stamp_at(1000, u64::MAX), u64::MAX);
    }
}
