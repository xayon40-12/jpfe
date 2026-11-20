//! Syncing the app between devices through the user's own cloud storage (Dropbox first).
//!
//! What is synced is one small document of records ([`SyncDoc`], `/state.json` in the app's folder) and the
//! books' original files (`/books/<sha256>.<ext>`). Each record carries when it last changed, and two copies
//! merge record by record, keeping the newest: the settings as one record, each book's entry, each book's
//! place in the reader, and the view. Books are named by the SHA-256 of their file, the same on every
//! device; on a device they keep their local id (the key of their files in its storage), and the records are
//! mapped between the two here ([`local_doc`], [`adopt`]).
//!
//! The [`engine`] runs the syncing itself against a [`engine::Provider`], so that another storage (CloudKit)
//! could be added later; [`dropbox`] is the first provider.

pub mod account;
pub mod dropbox;
pub mod engine;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::book::Settings;
use crate::reader::BookReaderState;
use crate::state::{SavedState, ViewState, DEFAULT_LOG_SCALES};

/// Version of the synced document's format; a device refuses to write over a newer one.
pub const FORMAT: u32 = 1;

/// The synced records (`/state.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncDoc {
    pub format: u32,
    /// `None` until some device changes a setting
    pub settings: Option<SettingsRecord>,
    /// Each book's entry, or the record of its removal, by hash
    pub books: BTreeMap<String, BookRecord>,
    /// Where the reader was left in each book, by hash
    pub places: BTreeMap<String, BookReaderState>,
    /// The view showing on the device that last changed it; `None` until one does
    pub view: Option<ViewRecord>,
}

impl Default for SyncDoc {
    fn default() -> Self {
        Self { format: FORMAT, settings: None, books: BTreeMap::new(), places: BTreeMap::new(), view: None }
    }
}

/// The settings synced: the word lists', the plots' scales and the reader's. Not the debug information,
/// which is for one device.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SettingsRecord {
    pub modified: u64,
    pub settings: Settings,
    pub log_scales: Vec<bool>,
    pub reader_font_size: f32,
    pub reader_vertical: bool,
    pub reader_paginate: bool,
    pub reader_highlight: bool,
}

impl Default for SettingsRecord {
    fn default() -> Self {
        Self::of(&SavedState::default())
    }
}

impl SettingsRecord {
    pub fn of(state: &SavedState) -> Self {
        Self {
            modified: state.settings_modified,
            settings: state.settings,
            log_scales: state.log_scales.clone(),
            reader_font_size: state.reader_font_size,
            reader_vertical: state.reader_vertical,
            reader_paginate: state.reader_paginate,
            reader_highlight: state.reader_highlight,
        }
    }

    fn apply(&self, state: &mut SavedState) {
        state.settings_modified = self.modified;
        state.settings = self.settings;
        state.log_scales = self.log_scales.clone();
        state.log_scales.resize(DEFAULT_LOG_SCALES.len().max(state.log_scales.len()), true);
        state.reader_font_size = self.reader_font_size;
        state.reader_vertical = self.reader_vertical;
        state.reader_paginate = self.reader_paginate;
        state.reader_highlight = self.reader_highlight;
    }
}

/// A book's entry, or the record of its removal (`removed`, with when).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BookRecord {
    pub modified: u64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub file_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub learned: Option<usize>,
}

impl BookRecord {
    pub fn removal(modified: u64) -> Self {
        Self { modified, removed: true, ..Self::default() }
    }

    /// Where the book's file is in the synced folder.
    pub fn path(&self, hash: &str) -> String {
        book_path(hash, &self.file_name)
    }
}

/// Where a book's file is in the synced folder: `/books/<hash>.<extension>`.
pub fn book_path(hash: &str, file_name: &str) -> String {
    let extension = file_name.rsplit_once('.').map_or("bin".to_string(), |(_, e)| e.to_lowercase());
    format!("/books/{hash}.{extension}")
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewRecord {
    pub modified: u64,
    /// The book shown in the reader, by hash; `None` for the statistics
    pub reading: Option<String>,
}

/// A record that merges by keeping the newest copy.
pub trait Record: Serialize {
    fn modified(&self) -> u64;
}

impl Record for SettingsRecord {
    fn modified(&self) -> u64 {
        self.modified
    }
}

impl Record for BookRecord {
    fn modified(&self) -> u64 {
        self.modified
    }
}

impl Record for BookReaderState {
    fn modified(&self) -> u64 {
        self.modified
    }
}

impl Record for ViewRecord {
    fn modified(&self) -> u64 {
        self.modified
    }
}

/// Whether `a` replaces `b`: it changed later, or at the same time with contents that sort after (any rule
/// works, as long as every device picks the same copy).
pub fn newer<R: Record>(a: &R, b: &R) -> bool {
    match a.modified().cmp(&b.modified()) {
        std::cmp::Ordering::Equal => serde_json::to_string(a).ok() > serde_json::to_string(b).ok(),
        order => order.is_gt(),
    }
}

/// `a` if it replaces `b` (or there is no `b`).
fn newest<'a, R: Record>(a: Option<&'a R>, b: Option<&'a R>) -> Option<&'a R> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if newer(b, a) { b } else { a }),
        (a, b) => a.or(b),
    }
}

fn merge_maps<R: Record + Clone>(a: &BTreeMap<String, R>, b: &BTreeMap<String, R>) -> BTreeMap<String, R> {
    let mut merged = a.clone();
    for (key, record) in b {
        if a.get(key).is_none_or(|mine| newer(record, mine)) {
            merged.insert(key.clone(), record.clone());
        }
    }
    merged
}

/// Two copies of the records, merged record by record, keeping the newest. The places of removed books are
/// dropped.
pub fn merge(a: &SyncDoc, b: &SyncDoc) -> SyncDoc {
    let books = merge_maps(&a.books, &b.books);
    let mut places = merge_maps(&a.places, &b.places);
    places.retain(|hash, _| books.get(hash).is_some_and(|book| !book.removed));
    SyncDoc {
        format: FORMAT,
        settings: newest(a.settings.as_ref(), b.settings.as_ref()).cloned(),
        books,
        places,
        view: newest(a.view.as_ref(), b.view.as_ref()).cloned(),
    }
}

/// SHA-256 of a book's file, in hex: its name on every device.
pub fn content_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// The hash of each local book id; books not hashed yet (states saved before sync) are left out, and so
/// are not synced yet. A book opened twice is synced once, as the first.
fn hashes_by_id(state: &SavedState) -> HashMap<u64, &str> {
    let mut seen = std::collections::HashSet::new();
    let mut books: Vec<_> = state.books.iter().collect();
    books.sort_by_key(|b| b.id);
    books.into_iter().filter_map(|b| b.hash.as_deref().filter(|h| seen.insert(*h)).map(|h| (b.id, h))).collect()
}

/// This device's records, named by hash.
pub fn local_doc(state: &SavedState) -> SyncDoc {
    let hashes = hashes_by_id(state);
    let mut books: BTreeMap<String, BookRecord> =
        state.removed.iter().map(|(hash, &modified)| (hash.clone(), BookRecord::removal(modified))).collect();
    for book in &state.books {
        // The first book of each hash
        if let Some(&hash) = hashes.get(&book.id) {
            let record = BookRecord {
                modified: book.modified,
                removed: false,
                name: book.name.clone(),
                file_name: book.file_name.clone(),
                learned: book.learned,
            };
            // A book added again replaces its removal (its time is later, see `crate::state::stamp`)
            if books.get(hash).is_none_or(|old| newer(&record, old)) {
                books.insert(hash.to_string(), record);
            }
        }
    }
    let places = state.reader_books.iter().filter_map(|(id, place)| Some((hashes.get(id)?.to_string(), place.clone()))).collect();
    // A view never changed, or on a book not hashed yet, is not synced
    let view = match state.view.reading {
        _ if state.view.modified == 0 => None,
        None => Some(ViewRecord { modified: state.view.modified, reading: None }),
        Some(id) => hashes.get(&id).map(|hash| ViewRecord { modified: state.view.modified, reading: Some(hash.to_string()) }),
    };
    let settings = (state.settings_modified > 0).then(|| SettingsRecord::of(state));
    SyncDoc { format: FORMAT, settings, books, places, view }
}

/// What [`adopt`] changed in the local state.
#[derive(Debug, Default, PartialEq)]
pub struct Adopted {
    /// The synced settings
    pub settings: bool,
    /// Books whose entry (name, *Learned up to*) changed, by local id
    pub books: Vec<u64>,
    /// Books removed on another device, now removed here (with their place)
    pub removed: Vec<u64>,
    /// Books whose place changed
    pub places: Vec<u64>,
    /// A newer place for the book open in the reader, not applied (see [`adopt`])
    pub offered: Option<(u64, BookReaderState)>,
    /// The view
    pub view: bool,
}

/// Take into the local state each record of `remote` that is newer than this device's. Books this device
/// does not have are left to download (they are added once their file is here).
///
/// - `open`: the book showing in the reader. It never jumps while it is read: a newer place for it is
///   `offered` rather than applied, and a removal of it waits until the reader leaves it.
/// - `take_view`: whether a newer view may switch the view (only before this device has shown the reader).
pub fn adopt(state: &mut SavedState, remote: &SyncDoc, open: Option<u64>, take_view: bool) -> Adopted {
    let mut adopted = Adopted::default();
    let local = local_doc(state);
    let ids: HashMap<String, u64> = hashes_by_id(state).into_iter().map(|(id, hash)| (hash.to_string(), id)).collect();

    if let Some(settings) = &remote.settings {
        if local.settings.as_ref().is_none_or(|mine| newer(settings, mine)) {
            settings.apply(state);
            adopted.settings = true;
        }
    }

    for (hash, record) in &remote.books {
        if local.books.get(hash).is_some_and(|mine| !newer(record, mine)) {
            continue;
        }
        match (ids.get(hash), record.removed) {
            (Some(&id), false) => {
                if let Some(book) = state.books.iter_mut().find(|b| b.id == id) {
                    book.name = record.name.clone();
                    book.learned = record.learned;
                    book.modified = record.modified;
                    adopted.books.push(id);
                }
            }
            (Some(&id), true) if Some(id) != open => {
                state.books.retain(|b| b.id != id);
                state.reader_books.remove(&id);
                state.removed.insert(hash.clone(), record.modified);
                adopted.removed.push(id);
            }
            (Some(_), true) => {}
            (None, true) => {
                state.removed.insert(hash.clone(), record.modified);
            }
            // Downloaded later
            (None, false) => {}
        }
    }

    for (hash, place) in &remote.places {
        let Some(&id) = ids.get(hash) else { continue };
        if adopted.removed.contains(&id) || state.reader_books.get(&id).is_some_and(|mine| !newer(place, mine)) {
            continue;
        }
        if Some(id) == open {
            adopted.offered = Some((id, place.clone()));
        } else {
            state.reader_books.insert(id, place.clone());
            adopted.places.push(id);
        }
    }

    if let Some(view) = remote.view.as_ref().filter(|_| take_view) {
        let reading = match &view.reading {
            Some(hash) => ids.get(hash).copied().map(Some),
            None => Some(None),
        };
        // Not when the book is not here (yet)
        if let Some(reading) = reading {
            if local.view.as_ref().is_none_or(|mine| newer(view, mine)) {
                state.view = ViewState { reading, modified: view.modified };
                adopted.view = true;
            }
        }
    }
    adopted
}
