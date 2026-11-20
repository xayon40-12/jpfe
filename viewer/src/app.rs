use std::collections::{BTreeMap, HashMap, HashSet};

use egui::{Color32, RichText, Ui};
use egui_plot::{GridInput, GridMark, HoverPosition, Line, Plot, Points, VLine};
use crate::chart::{format_count, format_ratio, log_ticks};
use jpfe::tokenizer::JapaneseTokenizer;
use web_time::{Duration, Instant};

use crate::book::{Book, BookStats, CacheKey, PendingBook, RestoringBook, SavedBook, Settings};
use crate::platform::{self, Inbox, Incoming, WEB_DICTIONARY_FILES};
use crate::reader::{self, BookReaderState, Lookup, Reader};
use crate::state::{stamp, SavedState, ViewState, DEFAULT_LOG_SCALES};
use crate::storage::{self, Key};
use crate::sync::{self, account::Account, BookRecord, SettingsRecord, SyncDoc};
use crate::theme::{self, Palette};
use crate::timeline;

/// Below this window width the books and the charts are stacked in one column (phones).
const STACK_BELOW_WIDTH: f32 = 760.0;
/// Below this the charts stop shrinking and the page scrolls.
const MIN_PLOT_HEIGHT: f32 = 170.0;

/// Tokenizing time per frame, so the window keeps redrawing while books load.
const WORK_PER_FRAME: Duration = Duration::from_millis(25);

/// On touch screens, holding a finger this long (seconds) on a chart without moving it
/// inspects the chart: the finger then moves the hover instead of scrolling, until it is lifted.
const LONG_PRESS: f64 = 0.4;
/// How far (points) the finger may move during the long press.
const LONG_PRESS_SLOP: f32 = 8.0;

/// Identifies the code that computed a cached analysis (see `build.rs`).
const BUILD_ID: &str = env!("JPFE_BUILD_ID");

enum DictionaryState {
    /// Web only: waiting for the server's and the stored dictionary versions (`None` = unknown yet)
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    Checking { remote: Option<Option<String>>, stored: Option<Option<String>> },
    /// Web only: files of `version`, read from storage or downloaded (`received` compressed ones;
    /// `compressed` still to unpack, `files` unpacked)
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    Loading { version: String, from_storage: bool, compressed: Vec<(String, Vec<u8>)>, files: HashMap<String, Vec<u8>>, received: usize },
    /// `version` is part of the key of cached analyses
    Ready { tokenizer: Box<JapaneseTokenizer>, version: String },
    Failed(String),
}

/// How long the steps of this page load took, shown in the sidebar (loading is slow on some phones).
struct LoadTimes {
    started: Instant,
    /// Total time of each step, in the order they first happened
    steps: Vec<(&'static str, f64)>,
    /// When the current download or storage read of the dictionary started
    fetching_since: Option<Instant>,
    /// Seconds until the saved books were shown (some may still need analyzing)
    books: Option<f64>,
    /// Seconds until the dictionary and every book were ready
    total: Option<f64>,
}

impl LoadTimes {
    fn new() -> Self {
        Self { started: Instant::now(), steps: Vec::new(), fetching_since: None, books: None, total: None }
    }

    fn add(&mut self, step: &'static str, seconds: f64) {
        match self.steps.iter_mut().find(|(s, _)| *s == step) {
            Some((_, total)) => *total += seconds,
            None => self.steps.push((step, seconds)),
        }
    }

    fn timed<T>(&mut self, step: &'static str, work: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let result = work();
        self.add(step, start.elapsed().as_secs_f64());
        result
    }

    fn fetched(&mut self, step: &'static str) {
        if let Some(since) = self.fetching_since.take() {
            self.add(step, since.elapsed().as_secs_f64());
        }
    }

    fn summary(&self) -> Option<String> {
        let steps: Vec<String> = self.steps.iter().map(|(step, s)| format!("{step} {s:.1} s")).collect();
        let books = self.books.map_or(String::new(), |b| format!("books shown {b:.1} s, "));
        Some(format!("After start: {books}all ready {:.1} s ({})", self.total?, steps.join(", ")))
    }
}

/// `count` per 100 of `total` (0 for an empty chapter).
fn per_100(count: usize, total: usize) -> f64 {
    if total == 0 { 0.0 } else { 100.0 * count as f64 / total as f64 }
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Statistics,
    Reader,
}

pub struct ViewerApp {
    inbox: Inbox,
    dictionary: DictionaryState,
    books: Vec<Book>,
    pending: Vec<PendingBook>,
    errors: Vec<String>,
    /// Book whose statistics are shown
    shown: usize,
    /// Bumped whenever books are added or removed, to invalidate `stats`
    generation: u64,
    stats: Option<(StatsKey, BookStats)>,
    /// Log scale of each plot, in the order of [`DEFAULT_LOG_SCALES`]
    log_scales: [bool; 4],
    /// Settings the word lists are built with, and the ones being edited in the settings window
    settings: Settings,
    edited_settings: Settings,
    show_settings: bool,
    /// Show load times and storage errors in the sidebar
    debug: bool,
    /// For each book, its words not learned in the other books
    remaining: Option<(StatsKey, Vec<usize>)>,

    view: View,
    /// The chapter being read, kept when going back to the statistics
    reader: Option<Reader>,
    reader_font_size: f32,
    reader_vertical: bool,
    reader_paginate: bool,
    reader_highlight: bool,
    /// Where the reader was left in each book, by book id
    reader_books: BTreeMap<u64, BookReaderState>,
    /// The view showing, as saved
    view_state: ViewState,
    /// When the synced settings last changed, and their values as of then: a change is stamped when they differ
    settings_modified: u64,
    stamped_settings: Option<SettingsRecord>,
    /// Books removed, by hash, with when (see [`SavedState::removed`])
    removed: BTreeMap<String, u64>,
    /// Dropbox sync
    account: Account,
    /// The records last given to the sync, which pushes them when they change
    synced: Option<SyncDoc>,
    /// Books whose file is being read for the sync to upload: id and hash
    sync_reads: HashMap<u64, String>,
    /// A book whose file is read to name it by its hash (states saved before sync), and the books whose file
    /// is missing
    hashing: Option<u64>,
    unhashable: HashSet<u64>,
    /// A newer place from another device for the book open in the reader, offered rather than applied
    place_offer: Option<(u64, BookReaderState)>,
    /// Whether the first sync since the app started is still to come, and whether the view was switched since:
    /// another device's view is only taken before
    first_sync: bool,
    view_touched: bool,
    ctx: egui::Context,
    /// The book the reader showed when the app was last closed, reopened once it is ready
    wanted_reader: Option<u64>,
    /// The book selected last (being read, or else shown in the statistics), restored before the others
    first_book: Option<u64>,
    /// Definitions and kanji for the reader (web: once the dictionary is loaded)
    lookup: Option<Lookup>,

    /// The saved state has been read; until then nothing is saved, so it is not overwritten
    state_loaded: bool,
    /// Ids of the books something is stored for, once listed; with the saved state, what new books' ids follow
    stored_books: Option<Vec<u64>>,
    /// Ids of the books in the saved state as it was read
    saved_ids: Vec<u64>,
    /// Whether the browser keeps what is stored, once asked (asked once there are books to keep)
    persistent: Option<bool>,
    persistence_asked: bool,
    /// Last state written to storage
    saved: Option<SavedState>,
    next_id: u64,
    /// Saved books waiting for the dictionary, then the ones whose analysis or file is being read (or whose
    /// word lists are being built)
    to_restore: Vec<SavedBook>,
    restoring: Vec<SavedBook>,
    /// Books of `restoring` whose analysis is decoded, while their word lists are built
    decoded: Vec<RestoringBook>,
    /// Saved shown book, selected once it is restored
    wanted_shown: Option<u64>,
    /// Version of the dictionary being written to storage, and how many of its files are written
    dictionary_writes: Option<(String, usize)>,
    storage_error: Option<String>,

    /// A long press on a chart is in progress: the finger moves the hover, the page does not scroll
    inspecting: LongPress,
    /// Where the charts were drawn in the last frame, where a long press starts inspecting
    plot_rects: Vec<egui::Rect>,
    /// Chapter under the finger while inspecting, found while drawing the last frame
    inspected_chapter: Option<f64>,
    load_times: LoadTimes,
    /// When this frame started: the dictionary is unpacked in what is left of the frame's time
    frame_started: Instant,
    /// Web: the dictionary file being unpacked, a little each frame
    #[cfg(target_arch = "wasm32")]
    unpacking: Option<(String, platform::Unpacker)>,
}

#[derive(PartialEq)]
struct StatsKey {
    generation: u64,
    shown: usize,
    learned: Vec<Option<usize>>,
}

impl ViewerApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        let inbox = Inbox::default();
        let ctx = &cc.egui_ctx;
        platform::load_assets(&inbox, ctx);
        platform::watch_page_hidden(sync::account::flush_now);
        let account = Account::new(&inbox, ctx);
        storage::read(&inbox, ctx, Key::State);
        storage::list_books(&inbox, ctx);
        let dictionary = match platform::embedded_tokenizer() {
            Some(Ok(tokenizer)) => DictionaryState::Ready { tokenizer: Box::new(tokenizer), version: "embedded".into() },
            Some(Err(e)) => DictionaryState::Failed(e.to_string()),
            None => {
                storage::read(&inbox, ctx, Key::DictionaryVersion);
                DictionaryState::Checking { remote: None, stored: None }
            }
        };
        Self {
            inbox,
            dictionary,
            books: Vec::new(),
            pending: Vec::new(),
            errors: Vec::new(),
            shown: 0,
            generation: 0,
            stats: None,
            log_scales: DEFAULT_LOG_SCALES,
            settings: Settings::default(),
            edited_settings: Settings::default(),
            show_settings: false,
            debug: false,
            remaining: None,
            view: View::Statistics,
            reader: None,
            reader_font_size: reader::DEFAULT_FONT_SIZE,
            reader_vertical: reader::DEFAULT_VERTICAL,
            reader_paginate: reader::DEFAULT_PAGINATE,
            reader_highlight: reader::DEFAULT_HIGHLIGHT,
            reader_books: BTreeMap::new(),
            view_state: ViewState::default(),
            settings_modified: 0,
            stamped_settings: None,
            removed: BTreeMap::new(),
            account,
            synced: None,
            sync_reads: HashMap::new(),
            hashing: None,
            unhashable: HashSet::new(),
            place_offer: None,
            first_sync: true,
            view_touched: false,
            ctx: ctx.clone(),
            wanted_reader: None,
            first_book: None,
            lookup: platform::embedded_lookup(),
            state_loaded: false,
            stored_books: None,
            saved_ids: Vec::new(),
            persistent: None,
            persistence_asked: false,
            saved: None,
            next_id: 0,
            to_restore: Vec::new(),
            restoring: Vec::new(),
            decoded: Vec::new(),
            wanted_shown: None,
            dictionary_writes: None,
            storage_error: None,
            inspecting: LongPress::default(),
            plot_rects: Vec::new(),
            inspected_chapter: None,
            load_times: LoadTimes::new(),
            frame_started: Instant::now(),
            #[cfg(target_arch = "wasm32")]
            unpacking: None,
        }
    }

    fn handle_incoming(&mut self, ctx: &egui::Context) {
        let mut deferred = Vec::new();
        // One stored book per frame (its analysis decoded, or its file parsed: up to tens of milliseconds each), so
        // the window keeps redrawing while books are restored
        let mut book_read = false;
        for message in self.inbox.drain() {
            match message {
                Incoming::Stored { key: Key::Analysis(_) | Key::Source(_), .. } if book_read => deferred.push(message),
                // New books get ids after the saved and stored ones, so wait until both are known
                Incoming::File { .. } if !self.state_loaded || self.stored_books.is_none() => deferred.push(message),
                Incoming::File { name, bytes } => {
                    // Books are named by their contents: the same file twice is the same book
                    let hash = sync::content_hash(&bytes);
                    if let Some(open) = self.book_with_hash(&hash) {
                        self.errors.push(format!("{name}: already open, as {open}"));
                        continue;
                    }
                    let id = self.next_id;
                    self.next_id += 1;
                    self.ask_persistence(ctx);
                    storage::write(&self.inbox, ctx, Key::Source(id), &bytes);
                    let start = Instant::now();
                    let parsed = self.load_times.timed("reading books", || PendingBook::parse(id, &name, bytes, None));
                    timeline::note(format!("{name}: file parsed in {:.0} ms", timeline::since(start)));
                    match parsed {
                        Ok(mut book) => {
                            // Added again after it was removed: after the removal, so that it comes back everywhere
                            book.saved.modified = stamp(self.removed.remove(&hash).unwrap_or(0));
                            book.saved.hash = Some(hash);
                            self.pending.push(book);
                        }
                        Err(e) => {
                            storage::remove_book(&self.inbox, ctx, id);
                            self.errors.push(format!("{e:#}"));
                        }
                    }
                }
                Incoming::DictionaryVersion(version) => {
                    if let DictionaryState::Checking { remote, .. } = &mut self.dictionary {
                        *remote = Some(version);
                    }
                }
                Incoming::DictionaryFile { name, bytes } => {
                    if let DictionaryState::Loading { compressed, received, from_storage: false, .. } = &mut self.dictionary {
                        match bytes {
                            Ok(bytes) => {
                                // Stored as downloaded (35 MB in all), and unpacked again on each load
                                storage::write(&self.inbox, ctx, Key::DictionaryFile(name.clone()), &bytes);
                                compressed.push((name, bytes));
                                *received += 1;
                                if *received == WEB_DICTIONARY_FILES.len() {
                                    self.load_times.fetched("downloading the dictionary");
                                }
                            }
                            Err(e) => self.dictionary = DictionaryState::Failed(format!("Downloading {name}: {e}")),
                        }
                    }
                }
                Incoming::Stored { key, bytes } => {
                    book_read |= matches!(key, Key::Analysis(_) | Key::Source(_));
                    self.handle_stored(ctx, key, bytes);
                }
                Incoming::Written(Key::DictionaryFile(_)) => {
                    if let Some((version, written)) = &mut self.dictionary_writes {
                        *written += 1;
                        // Written last, so an interrupted download is never taken as complete
                        if *written == WEB_DICTIONARY_FILES.len() {
                            storage::write(&self.inbox, ctx, Key::DictionaryVersion, version.as_bytes());
                            self.dictionary_writes = None;
                        }
                    }
                }
                Incoming::Written(_) => {}
                Incoming::StoredBooks(ids) => {
                    self.stored_books = Some(ids);
                    self.books_known();
                }
                Incoming::Persistent(granted) => {
                    platform::log(&format!("Persistent storage {}", if granted { "granted" } else { "refused" }));
                    self.persistent = Some(granted);
                }
                Incoming::StorageError(e) => {
                    log::warn!("Storage: {e}");
                    self.storage_error.get_or_insert(e);
                }
                Incoming::Font { name, bytes, primary } => {
                    let priority = if primary { egui::epaint::text::FontPriority::Highest } else { egui::epaint::text::FontPriority::Lowest };
                    let families = [egui::FontFamily::Proportional, egui::FontFamily::Monospace]
                        .map(|family| egui::epaint::text::InsertFontFamily { family, priority })
                        .to_vec();
                    let start = Instant::now();
                    ctx.add_font(egui::epaint::text::FontInsert::new(&name, egui::FontData::from_owned(bytes), families));
                    timeline::note(format!("font {name} added in {:.0} ms", timeline::since(start)));
                }
                Incoming::Error(e) => self.errors.push(e),
                Incoming::SyncSignIn(result) => {
                    self.account.finish_sign_in(result, ctx);
                    self.first_sync = true;
                }
            }
        }
        if !deferred.is_empty() {
            ctx.request_repaint();
        }
        for message in deferred {
            self.inbox.push(message);
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for file in dropped {
            platform::read_dropped(&self.inbox, file, ctx);
        }
    }

    fn handle_stored(&mut self, ctx: &egui::Context, key: Key, bytes: Option<Vec<u8>>) {
        match key {
            Key::State => {
                let state = SavedState::load(bytes.as_deref());
                self.settings = state.settings;
                self.edited_settings = state.settings;
                self.log_scales = std::array::from_fn(|i| state.log_scales.get(i).copied().unwrap_or(DEFAULT_LOG_SCALES[i]));
                self.debug = state.debug;
                self.reader_font_size = state.reader_font_size.clamp(*reader::FONT_SIZES.start(), *reader::FONT_SIZES.end());
                self.reader_vertical = state.reader_vertical;
                self.reader_paginate = state.reader_paginate;
                self.reader_highlight = state.reader_highlight;
                self.reader_books = state.reader_books.clone();
                self.settings_modified = state.settings_modified;
                self.stamped_settings = Some(SettingsRecord::of(&state));
                self.removed = state.removed.clone();
                self.wanted_reader = state.view.reading;
                self.view_state = state.view.clone();
                self.wanted_shown = state.shown;
                self.first_book = state.view.reading.or(state.shown);
                self.saved_ids = state.books.iter().map(|b| b.id).collect();
                self.next_id = state.next_id;
                if !state.books.is_empty() {
                    self.ask_persistence(ctx);
                }
                self.to_restore = state.books.clone();
                self.saved = Some(state);
                self.state_loaded = true;
                self.books_known();
            }
            Key::DictionaryVersion => {
                if let DictionaryState::Checking { stored, .. } = &mut self.dictionary {
                    *stored = Some(bytes.and_then(|b| String::from_utf8(b).ok()).map(|v| v.trim().to_string()));
                }
            }
            Key::DictionaryFile(name) => {
                if let DictionaryState::Loading { version, compressed, received, from_storage: true, .. } = &mut self.dictionary {
                    match bytes {
                        Some(bytes) => {
                            compressed.push((name.clone(), bytes));
                            *received += 1;
                            if *received == WEB_DICTIONARY_FILES.len() {
                                self.load_times.fetched("reading the dictionary");
                            }
                            // One file at a time, to keep memory low
                            let next = WEB_DICTIONARY_FILES.iter().position(|f| *f == name).and_then(|i| WEB_DICTIONARY_FILES.get(i + 1));
                            if let Some(next) = next {
                                storage::read(&self.inbox, ctx, Key::DictionaryFile(next.to_string()));
                            }
                        }
                        // Incomplete cache: download this version instead
                        None => {
                            let version = version.clone();
                            self.start_download(ctx, version);
                        }
                    }
                }
            }
            Key::Analysis(id) => {
                let Some(pos) = self.restoring.iter().position(|b| b.id == id) else { return };
                let Some(dictionary) = self.dictionary_version() else { return };
                let key = CacheKey { build: BUILD_ID, dictionary: &dictionary, include_particles: self.settings.include_particles };
                let saved = self.restoring[pos].clone();
                let settings = self.settings;
                let start = Instant::now();
                let book = self.load_times.timed("restoring books", || bytes.and_then(|b| Book::from_analysis(saved, b, &key, &settings)));
                if book.is_none() {
                    timeline::note(format!("{}: to analyze again ({:.0} ms)", self.restoring[pos].name, timeline::since(start)));
                }
                match book {
                    // Its word lists are built next, in `do_work`; it stays in `restoring` until they are
                    Some(book) => self.decoded.push(book),
                    // Missing, or made by another build or dictionary: analyze the book again
                    None => storage::read(&self.inbox, ctx, Key::Source(id)),
                }
            }
            Key::Source(id) => {
                let mut bytes = bytes;
                let restoring = self.restoring.iter().position(|b| b.id == id);
                if let Some(hash) = self.sync_reads.remove(&id) {
                    if let Some(engine) = self.account.engine() {
                        let bytes = if restoring.is_some() { bytes.clone() } else { bytes.take() };
                        engine.borrow_mut().source(&hash, bytes);
                    }
                }
                if self.hashing == Some(id) {
                    self.hashing = None;
                    match &bytes {
                        Some(file) => self.set_hash(id, sync::content_hash(file)),
                        None => _ = self.unhashable.insert(id),
                    }
                }
                let Some(pos) = restoring else { return };
                let mut saved = self.restoring.remove(pos);
                // Read again for a new build: named by its hash if it was not yet (states saved before sync)
                if saved.hash.is_none() {
                    saved.hash = bytes.as_deref().map(sync::content_hash);
                }
                let start = Instant::now();
                let parsed = self.load_times.timed("reading books", || bytes.map(|b| PendingBook::parse(saved.id, &saved.file_name, b, saved.learned)));
                timeline::note(format!("{}: file parsed in {:.0} ms", saved.name, timeline::since(start)));
                match parsed {
                    Some(Ok(mut book)) => {
                        // Its saved entry as it was (the name may come from another device), with its hash
                        book.saved = SavedBook { id: book.saved.id, file_name: book.saved.file_name, ..saved };
                        self.push_pending(book);
                    }
                    Some(Err(e)) => self.errors.push(format!("{e:#}")),
                    None => self.errors.push(format!("{}: the stored file is missing; open it again", saved.name)),
                }
            }
        }
    }

    /// Queue a book to analyze: the last selected book first, then the others by id (their order on screen),
    /// whatever order their files are read in.
    fn push_pending(&mut self, book: PendingBook) {
        let rank = |id: u64| (Some(id) != self.first_book, id);
        let new = rank(book.saved.id);
        let pos = self.pending.iter().position(|b| rank(b.saved.id) > new).unwrap_or(self.pending.len());
        self.pending.insert(pos, book);
    }

    /// Once both the saved state and the stored books are known: the id of the next new book, which is after
    /// both, and a message about books stored but missing from the saved state (kept, never overwritten).
    fn books_known(&mut self) {
        let (true, Some(stored)) = (self.state_loaded, &self.stored_books) else { return };
        self.next_id = next_book_id(self.next_id, &self.saved_ids, stored);
        let unlisted: Vec<String> = stored.iter().filter(|id| !self.saved_ids.contains(id)).map(|id| format!("books/{id}")).collect();
        if !unlisted.is_empty() {
            let books = if unlisted.len() == 1 { "a book".to_string() } else { format!("{} books", unlisted.len()) };
            self.errors.push(format!("Files of {books} missing from the book list are kept in this browser's storage: {}", unlisted.join(", ")));
        }
    }

    /// Ask the browser to keep the stored books (once).
    fn ask_persistence(&mut self, ctx: &egui::Context) {
        if !std::mem::replace(&mut self.persistence_asked, true) {
            storage::persist(&self.inbox, ctx);
        }
    }

    /// The dictionary version in use, or being loaded. Cached analyses only need this, not
    /// the dictionary itself, so books are restored while the dictionary is still unpacked.
    fn dictionary_version(&self) -> Option<String> {
        match &self.dictionary {
            DictionaryState::Ready { version, .. } | DictionaryState::Loading { version, .. } => Some(version.clone()),
            DictionaryState::Checking { .. } | DictionaryState::Failed(_) => None,
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_variables))]
    fn start_download(&mut self, ctx: &egui::Context, version: String) {
        self.load_times.fetching_since = Some(Instant::now());
        // Also removes files stored by older versions of the app
        storage::remove_dictionary(&self.inbox, ctx);
        #[cfg(target_arch = "wasm32")]
        platform::download_dictionary(&self.inbox, ctx, &version);
        self.dictionary_writes = Some((version.clone(), 0));
        self.dictionary = DictionaryState::Loading { version, from_storage: false, compressed: Vec::new(), files: HashMap::new(), received: 0 };
    }

    /// Add a ready book, keeping import order and the shown book.
    fn insert_book(&mut self, book: Book) {
        let shown_id = self.wanted_shown.or_else(|| self.books.get(self.shown).map(|b| b.id));
        self.books.push(book);
        self.books.sort_by_key(|b| b.id);
        if let Some(pos) = shown_id.and_then(|id| self.books.iter().position(|b| b.id == id)) {
            self.shown = pos;
            if self.wanted_shown == Some(self.books[pos].id) {
                self.wanted_shown = None;
            }
        }
        self.generation += 1;
    }

    /// Get the dictionary ready, restore saved books, and tokenize pending ones, a little per frame.
    fn do_work(&mut self, ctx: &egui::Context) {
        if self.state_loaded && !self.to_restore.is_empty() && self.dictionary_version().is_some() {
            // The last selected book first, alone, so that it is ready (or analyzed) before the others; they are
            // read once it is no longer being read
            let first = self.first_book.and_then(|id| self.to_restore.iter().position(|b| b.id == id));
            let reading_first = self.first_book.is_some_and(|id| self.restoring.iter().any(|b| b.id == id));
            let batch = match first {
                Some(pos) => vec![self.to_restore.remove(pos)],
                None if reading_first => Vec::new(),
                None => std::mem::take(&mut self.to_restore),
            };
            for saved in batch {
                storage::read(&self.inbox, ctx, Key::Analysis(saved.id));
                self.restoring.push(saved);
            }
        }
        // Word lists of a restored book, in the frame's time
        if let Some(book) = self.decoded.first_mut() {
            let deadline = self.frame_started + WORK_PER_FRAME;
            match timeline::step("restoring a book", || book.step(deadline)) {
                Some(false) => {}
                Some(true) => timeline::step("finishing a restored book", || {
                    let book = self.decoded.remove(0).finish(&self.settings);
                    self.restoring.retain(|b| b.id != book.id);
                    self.insert_book(book);
                }),
                // Damaged: analyze the book again from its file (it stays in `restoring` until it is read)
                None => {
                    let id = self.decoded.remove(0).id();
                    timeline::note(format!("book {id}: stored analysis damaged, analyzed again"));
                    storage::read(&self.inbox, ctx, Key::Source(id));
                }
            }
            ctx.request_repaint();
        }
        if self.state_loaded && self.to_restore.is_empty() && self.restoring.is_empty() && self.load_times.books.is_none() {
            self.load_times.books = Some(self.load_times.started.elapsed().as_secs_f64());
        }

        #[cfg(target_arch = "wasm32")]
        if timeline::step("dictionary", || self.prepare_dictionary(ctx)) {
            ctx.request_repaint();
            return;
        }
        let Some(dictionary) = self.dictionary_version() else { return };

        let all_ready = self.state_loaded && self.to_restore.is_empty() && self.restoring.is_empty() && self.pending.is_empty();
        if all_ready && self.load_times.total.is_none() {
            self.load_times.total = Some(self.load_times.started.elapsed().as_secs_f64());
            if let Some(summary) = self.load_times.summary() {
                platform::log(&summary);
            }
        }

        let DictionaryState::Ready { tokenizer, .. } = &mut self.dictionary else { return };
        let Some(book) = self.pending.first_mut() else { return };
        let started = Instant::now();
        let stepped = timeline::step("analyzing a book", || book.step(tokenizer, self.settings.include_particles, started + WORK_PER_FRAME));
        self.load_times.add("analyzing books", started.elapsed().as_secs_f64());
        match stepped {
            Ok(false) => {}
            Ok(true) => timeline::step("finishing an analysis", || {
                let start = Instant::now();
                let book = self.pending.remove(0).finish(&self.settings);
                let key = CacheKey { build: BUILD_ID, dictionary: &dictionary, include_particles: self.settings.include_particles };
                let bytes = book.analysis_bytes(&key);
                timeline::note(format!("{}: analyzed; word lists and encoding {:.0} ms", book.name, timeline::since(start)));
                storage::write(&self.inbox, ctx, Key::Analysis(book.id), &bytes);
                self.insert_book(book);
            }),
            Err(e) => {
                let book = self.pending.remove(0);
                self.errors.push(format!("{}: {e:#}", book.saved.name));
            }
        }
        ctx.request_repaint();
    }

    /// Web: pick the stored or the downloaded dictionary, and unpack it; true while busy.
    #[cfg(target_arch = "wasm32")]
    fn prepare_dictionary(&mut self, ctx: &egui::Context) -> bool {
        match &mut self.dictionary {
            DictionaryState::Checking { remote: Some(remote), stored: Some(stored) } => {
                match (remote.clone(), stored.clone()) {
                    // Same as the stored one, or offline with a stored one: no download
                    (Some(remote), Some(stored)) if remote == stored => self.read_stored_dictionary(ctx, stored),
                    (None, Some(stored)) => self.read_stored_dictionary(ctx, stored),
                    (Some(remote), _) => self.start_download(ctx, remote),
                    (None, None) => self.dictionary = DictionaryState::Failed("could not download it".into()),
                }
                true
            }
            DictionaryState::Checking { .. } => true,
            DictionaryState::Loading { version, from_storage, compressed, files, .. } => {
                // A file at a time, a little of it each frame, in what is left of the frame's time
                if self.unpacking.is_none() {
                    if let Some((name, bytes)) = compressed.pop() {
                        match platform::Unpacker::new(bytes) {
                            Ok(unpacker) => self.unpacking = Some((name, unpacker)),
                            Err(e) => {
                                self.dictionary = DictionaryState::Failed(format!("Unpacking {name}: {e}"));
                                return true;
                            }
                        }
                    }
                }
                if let Some((name, unpacker)) = &mut self.unpacking {
                    let deadline = self.frame_started + WORK_PER_FRAME;
                    match self.load_times.timed("unpacking", || unpacker.step(deadline)) {
                        Ok(Some(data)) => {
                            timeline::note(format!(
                                "{name} unpacked: {} in {:.0} ms over {} frames (at most {:.0} ms in one)",
                                timeline::size(data.len()),
                                unpacker.total_ms,
                                unpacker.frames,
                                unpacker.longest_ms
                            ));
                            files.insert(name.clone(), data);
                            self.unpacking = None;
                        }
                        Ok(None) => {}
                        Err(e) => {
                            self.dictionary = DictionaryState::Failed(format!("Unpacking {name}: {e}"));
                            self.unpacking = None;
                        }
                    }
                } else if files.len() == WEB_DICTIONARY_FILES.len() {
                    let (version, mut files) = (version.clone(), std::mem::take(files));
                    let start = Instant::now();
                    log::info!("Dictionary {version} {}", if *from_storage { "from storage" } else { "downloaded" });
                    let tokenizer = self.load_times.timed("loading the dictionary", || jpfe::tokenizer::tokenizer_from_files(|name| files.remove(name), true));
                    match self.load_times.timed("loading the definitions", || Lookup::from_files(|name| files.remove(name))) {
                        Ok(lookup) => self.lookup = Some(lookup),
                        Err(e) => self.errors.push(format!("Definitions unavailable: {e:#}")),
                    }
                    self.dictionary = match tokenizer {
                        Ok(tokenizer) => DictionaryState::Ready { tokenizer: Box::new(tokenizer), version },
                        Err(e) => DictionaryState::Failed(format!("{e:#}")),
                    };
                    timeline::note(format!("dictionary and definitions loaded in {:.0} ms", timeline::since(start)));
                }
                true
            }
            DictionaryState::Ready { .. } | DictionaryState::Failed(_) => false,
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn read_stored_dictionary(&mut self, ctx: &egui::Context, version: String) {
        self.load_times.fetching_since = Some(Instant::now());
        // The others are read one after the other, as each one arrives
        storage::read(&self.inbox, ctx, Key::DictionaryFile(WEB_DICTIONARY_FILES[0].to_string()));
        self.dictionary = DictionaryState::Loading { version, from_storage: true, compressed: Vec::new(), files: HashMap::new(), received: 0 };
    }

    /// Everything to remember, written whenever it changes.
    fn current_state(&self) -> SavedState {
        let mut books: Vec<SavedBook> = self
            .books
            .iter()
            .map(Book::saved)
            .chain(self.pending.iter().map(|b| b.saved.clone()))
            .chain(self.restoring.iter().chain(&self.to_restore).cloned())
            .collect();
        books.sort_by_key(|b| b.id);
        SavedState {
            settings: self.settings,
            log_scales: self.log_scales.to_vec(),
            debug: self.debug,
            reader_font_size: self.reader_font_size,
            reader_vertical: self.reader_vertical,
            reader_paginate: self.reader_paginate,
            reader_highlight: self.reader_highlight,
            settings_modified: self.settings_modified,
            reader_books: self.reader_books.clone(),
            view: self.view_state.clone(),
            removed: self.removed.clone(),
            shown: self.wanted_shown.or_else(|| self.books.get(self.shown).map(|b| b.id)),
            next_id: self.next_id,
            books,
        }
    }

    fn save_state(&mut self, ctx: &egui::Context) {
        if !self.state_loaded {
            return;
        }
        self.stamp_settings(ctx);
        let state = self.current_state();
        if self.saved.as_ref() != Some(&state) {
            if let Ok(json) = serde_json::to_vec(&state) {
                storage::write(&self.inbox, ctx, Key::State, &json);
            }
            self.saved = Some(state);
            self.touch_sync(ctx);
        }
    }

    /// The synced settings as chosen (the word-list settings being edited, rather than the ones the lists
    /// were last built with).
    fn synced_settings(&self) -> SettingsRecord {
        SettingsRecord {
            modified: self.settings_modified,
            settings: self.edited_settings,
            log_scales: self.log_scales.to_vec(),
            reader_font_size: self.reader_font_size,
            reader_vertical: self.reader_vertical,
            reader_paginate: self.reader_paginate,
            reader_highlight: self.reader_highlight,
        }
    }

    /// Stamp the synced settings with the time they changed (once a slider is let go).
    fn stamp_settings(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.pointer.any_down()) {
            return;
        }
        let current = self.synced_settings();
        if let Some(stamped) = &self.stamped_settings {
            // Changed here, rather than taken with its time from another device
            if current.modified == stamped.modified && (SettingsRecord { modified: stamped.modified, ..current }) != *stamped {
                self.settings_modified = stamp(self.settings_modified);
            }
        }
        self.stamped_settings = Some(self.synced_settings());
    }

    /// This device's synced records.
    fn local_records(&self) -> SyncDoc {
        let mut state = self.current_state();
        state.settings = self.edited_settings;
        sync::local_doc(&state)
    }

    /// Tell the sync when the synced records changed.
    fn touch_sync(&mut self, ctx: &egui::Context) {
        let Some(engine) = self.account.engine() else { return };
        let records = self.local_records();
        if self.synced.as_ref() != Some(&records) {
            self.synced = Some(records.clone());
            engine.borrow_mut().touch(ctx.input(|i| i.time), records);
        }
    }

    /// Run the sync (when signed in): name the books saved before sync by their hash, then let the sync engine
    /// handle its replies and start what is due.
    fn sync_step(&mut self, ctx: &egui::Context) {
        self.account.check_revoked();
        let Some(engine) = self.account.engine() else {
            self.synced = None;
            return;
        };
        // New books get ids after the saved and stored ones (downloaded ones too)
        if !self.state_loaded || self.stored_books.is_none() {
            return;
        }
        if self.hashing.is_none() {
            if let Some(book) = self.books.iter().find(|b| b.hash.is_none() && !self.unhashable.contains(&b.id)) {
                self.hashing = Some(book.id);
                storage::read(&self.inbox, ctx, Key::Source(book.id));
            }
        }
        let wait = engine.borrow_mut().poll(ctx.input(|i| i.time), self);
        if let Some(wait) = wait {
            ctx.request_repaint_after(Duration::from_secs_f64(wait.clamp(0.0, 3600.0)));
        }
    }

    /// The name of the book with this hash, at any stage.
    fn book_with_hash(&self, hash: &str) -> Option<String> {
        self.current_state().books.into_iter().find(|b| b.hash.as_deref() == Some(hash)).map(|b| b.name)
    }

    /// Change a book's saved entry (name, progress, hash, time), at any stage.
    fn set_book(&mut self, saved: SavedBook) {
        let id = saved.id;
        if let Some(book) = self.books.iter_mut().find(|b| b.id == id) {
            book.name = saved.name;
            book.learned = saved.learned.map(|pos| pos.min(book.chapters.len().saturating_sub(1)));
            book.hash = saved.hash;
            book.modified = saved.modified;
            self.generation += 1;
            return;
        }
        for book in self.pending.iter_mut().filter(|b| b.saved.id == id) {
            book.saved = saved.clone();
        }
        for book in self.restoring.iter_mut().chain(self.to_restore.iter_mut()).filter(|b| b.id == id) {
            *book = saved.clone();
        }
        for book in self.decoded.iter_mut().filter(|b| b.id() == id) {
            *book.saved_mut() = saved.clone();
        }
    }

    fn set_hash(&mut self, id: u64, hash: String) {
        if let Some(mut saved) = self.current_state().books.into_iter().find(|b| b.id == id) {
            saved.hash = Some(hash);
            self.set_book(saved);
        }
    }

    /// Take what is newer in another device's records (see [`sync::adopt`]).
    fn adopt_records(&mut self, remote: &SyncDoc) {
        let mut state = self.current_state();
        state.settings = self.edited_settings;
        let open = self.reader.as_ref().filter(|_| self.view == View::Reader).map(|r| r.book);
        let take_view = std::mem::replace(&mut self.first_sync, false) && !self.view_touched && open.is_none();
        let adopted = sync::adopt(&mut state, remote, open, take_view);
        if adopted.settings {
            // The word lists are rebuilt next frame, as after editing the settings
            self.edited_settings = state.settings;
            self.log_scales = std::array::from_fn(|i| state.log_scales.get(i).copied().unwrap_or(DEFAULT_LOG_SCALES[i]));
            self.reader_font_size = state.reader_font_size.clamp(*reader::FONT_SIZES.start(), *reader::FONT_SIZES.end());
            self.reader_vertical = state.reader_vertical;
            self.reader_paginate = state.reader_paginate;
            self.reader_highlight = state.reader_highlight;
            self.settings_modified = state.settings_modified;
            self.stamped_settings = Some(self.synced_settings());
        }
        for id in &adopted.books {
            if let Some(saved) = state.books.iter().find(|b| b.id == *id) {
                self.set_book(saved.clone());
            }
        }
        for &id in &adopted.removed {
            self.forget_book(id);
        }
        self.removed = state.removed;
        // A book left in the reader (behind the statistics) opens at its new place
        if self.reader.as_ref().is_some_and(|r| adopted.places.contains(&r.book)) {
            self.reader = None;
        }
        self.reader_books = state.reader_books;
        self.place_offer = adopted.offered;
        if adopted.view {
            self.view_state = state.view.clone();
            self.wanted_reader = state.view.reading;
            if state.view.reading.is_none() {
                self.view = View::Statistics;
            }
        }
    }

    /// Take the records of the last sync again (the reader has left a book whose changes waited).
    fn adopt_again(&mut self) {
        let remote = self.account.engine().and_then(|engine| engine.borrow().remote().cloned());
        if let Some(remote) = remote {
            self.adopt_records(&remote);
        }
    }

    /// A book downloaded from another device: stored and analyzed, as a book opened here.
    fn add_downloaded(&mut self, hash: &str, record: &BookRecord, bytes: Vec<u8>) -> Result<(), String> {
        if sync::content_hash(&bytes) != hash {
            return Err("the downloaded file is damaged".into());
        }
        if self.book_with_hash(hash).is_some() {
            return Ok(());
        }
        let id = self.next_id;
        self.next_id += 1;
        let ctx = self.ctx.clone();
        self.ask_persistence(&ctx);
        storage::write(&self.inbox, &ctx, Key::Source(id), &bytes);
        let start = Instant::now();
        let parsed = PendingBook::parse(id, &record.file_name, bytes, record.learned);
        timeline::note(format!("{}: downloaded, file parsed in {:.0} ms", record.name, timeline::since(start)));
        match parsed {
            Ok(mut book) => {
                book.saved.name = record.name.clone();
                book.saved.hash = Some(hash.to_string());
                book.saved.modified = record.modified;
                self.removed.remove(hash);
                self.push_pending(book);
                Ok(())
            }
            Err(e) => {
                storage::remove_book(&self.inbox, &ctx, id);
                Err(format!("{e:#}"))
            }
        }
    }

    /// Rebuild the word lists once the edited settings differ, but not while a slider is dragged.
    fn apply_settings(&mut self, ctx: &egui::Context) {
        if self.edited_settings == self.settings || ctx.input(|i| i.pointer.any_down()) {
            return;
        }
        let retokenize = self.edited_settings.include_particles != self.settings.include_particles;
        self.settings = self.edited_settings;
        if retokenize {
            for book in &mut self.pending {
                book.restart();
            }
            let books: Vec<PendingBook> = self.books.drain(..).map(Book::retokenize).collect();
            self.pending.splice(0..0, books);
            // Books being restored were analyzed with the old setting too
            for book in std::mem::take(&mut self.decoded) {
                let id = book.id();
                match book.retokenize() {
                    Some(book) => {
                        self.restoring.retain(|b| b.id != id);
                        self.pending.push(book);
                    }
                    // Its chapters are not decoded yet: from its file
                    None => storage::read(&self.inbox, ctx, Key::Source(id)),
                }
            }
        } else {
            for book in &mut self.books {
                book.apply(&self.settings);
            }
        }
        self.generation += 1;
    }

    fn progress_key(&self, shown: usize) -> StatsKey {
        StatsKey { generation: self.generation, shown, learned: self.books.iter().map(|b| b.learned).collect() }
    }

    /// Words of each book not learned in the other books, recomputed when books or progress change.
    fn remaining_counts(&mut self) -> Vec<usize> {
        let key = self.progress_key(0);
        if self.remaining.as_ref().is_none_or(|(k, _)| *k != key) {
            self.remaining = Some((key, Book::remaining_counts(&self.books)));
        }
        self.remaining.as_ref().map(|(_, r)| r.clone()).unwrap_or_default()
    }

    fn settings_window(&mut self, ctx: &egui::Context, palette: Palette) {
        let mut open = self.show_settings;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .max_width(ctx.content_rect().width() - 24.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 48.0])
            .show(ctx, |ui| {
                // Slider tracks use the control fill, which is white on the white window
                ui.visuals_mut().widgets.inactive.bg_fill = palette.separator;
                ui.spacing_mut().slider_width = (ui.available_width() - 90.0).clamp(120.0, 240.0);
                // The settings scroll when the screen is too short for them (phones), under the title and above the
                // buttons, which stay in view. egui's own window scrolling left the end of the settings out of reach
                // on phones.
                let max_height = (ctx.content_rect().height() - 170.0).max(120.0);
                egui::ScrollArea::vertical().max_height(max_height).auto_shrink([false, true]).show(ui, |ui| self.settings_ui(ui, palette));
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Restore defaults").clicked() {
                        self.edited_settings = Settings::default();
                        self.reader_font_size = reader::DEFAULT_FONT_SIZE;
                        self.reader_vertical = reader::DEFAULT_VERTICAL;
                        self.reader_paginate = reader::DEFAULT_PAGINATE;
                        self.reader_highlight = reader::DEFAULT_HIGHLIGHT;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(theme::primary_button("Done", palette)).clicked() {
                            self.show_settings = false;
                        }
                    });
                });
            });
        self.show_settings &= open;
    }

    /// The settings, in the settings window.
    fn settings_ui(&mut self, ui: &mut Ui, palette: Palette) {
        let s = &mut self.edited_settings;
        let note = |ui: &mut Ui, text: &str| {
            ui.label(RichText::new(text).small().color(palette.secondary_text));
            ui.add_space(4.0);
        };
        ui.label("Reader font size");
        ui.add(egui::Slider::new(&mut self.reader_font_size, reader::FONT_SIZES).suffix(" pt").fixed_decimals(0));
        note(ui, "In the reader, you can also pinch, or scroll with Shift held down.");
        ui.label("Reader text direction");
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.reader_vertical, true, "Vertical (縦書き)");
            ui.radio_value(&mut self.reader_vertical, false, "Horizontal (横書き)");
        });
        note(ui, "Vertical text reads from top to bottom, in columns from right to left.");
        ui.checkbox(&mut self.reader_paginate, "Paginate");
        note(ui, "One page at a time: drag the page sideways, or press the arrow keys, to turn it. Off, the text scrolls.");
        ui.checkbox(&mut self.reader_highlight, "Highlight words");
        note(ui, "Every word on a color for its part of speech, with its prefix, endings and suffixes in their own colors.");
        ui.separator();

        ui.label("Coverage target");
        ui.add(egui::Slider::new(&mut s.target_percentage, 50.0..=100.0).suffix(" %").fixed_decimals(1));
        note(ui, "Each chapter's list stops once its words cover this share of the chapter's words.");

        ui.label("Minimum occurrences in the book");
        ui.add(egui::Slider::new(&mut s.min_book_count, 1..=20));
        note(ui, "Words that appear fewer times in the whole book are left out.");

        ui.label("Minimum chapters in the book");
        ui.add(egui::Slider::new(&mut s.min_chapters, 1..=20));
        note(ui, "Words found in fewer chapters are left out, so that a word repeated in a single scene is not listed.");

        ui.checkbox(&mut s.hide_previous_words, "Only words new to the book");
        note(ui, "Leave out words already listed in an earlier chapter of the same book.");

        ui.checkbox(&mut s.hide_katakana_words, "Leave out katakana-only words");
        note(ui, "Mostly loanwords such as パソコン or エルフ, which are often easy to guess.");

        ui.checkbox(&mut s.include_particles, "Include particles and auxiliary verbs (助詞・助動詞)");
        note(ui, "Changing this analyzes the books again.");

        ui.separator();
        self.account.ui(ui, palette, &self.inbox);

        ui.separator();
        ui.checkbox(&mut self.debug, "Show debug information");
        note(ui, "Load times, memory and storage errors, under the Open button.");

        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let small = |text: &str| egui::RichText::new(text).small().color(palette.secondary_text);
            ui.label(small("Words are found with "));
            ui.hyperlink_to(small("UniDic").color(palette.accent), "https://clrd.ninjal.ac.jp/unidic/");
            ui.label(small(" (BSD) and "));
            ui.hyperlink_to(small("JMdict").color(palette.accent), "https://www.edrdg.org/wiki/index.php/JMdict-EDICT_Dictionary_Project");
            ui.label(small(", the property of the EDRDG, used under "));
            ui.hyperlink_to(small("CC BY-SA 4.0").color(palette.accent), "https://www.edrdg.org/edrdg/licence.html");
            ui.label(small("."));
        });

        ui.separator();
        reader_help(ui, self.reader_vertical, self.reader_paginate, palette);
    }

    /// The small button at the top left that switches between the statistics and the reader.
    fn view_toggle(&mut self, ui: &mut Ui) {
        let (icon, hint) = match self.view {
            View::Statistics => ("📖", "Read the selected book, where you left it"),
            View::Reader => ("📊", "Back to the statistics"),
        };
        let button = egui::Button::new(RichText::new(icon).size(15.0)).min_size(egui::vec2(28.0, 24.0));
        if ui.add_enabled(!self.books.is_empty(), button).on_hover_text(hint).clicked() {
            self.toggle_view();
        }
    }

    /// Switch between the statistics and the reader on the selected book.
    fn toggle_view(&mut self) {
        if self.view == View::Reader {
            match self.place_offer.take() {
                // A newer place from another device, not taken while the book was read: it opens there next time
                Some((id, place)) if self.reader.as_ref().is_some_and(|r| r.book == id) => {
                    self.reader_books.insert(id, place);
                    self.reader = None;
                }
                offer => {
                    self.place_offer = offer;
                    self.remember_place();
                }
            }
            self.view = View::Statistics;
            // What waited while the book was read (a removal)
            self.adopt_again();
            return;
        }
        self.wanted_reader = None;
        self.open_reader(self.shown);
    }

    /// Show the reader on the book at `pos`, where it was left (in memory, or saved), or else at its first
    /// chapter. The book's *Learned up to* does not move it: readers often learn words chapters ahead.
    fn open_reader(&mut self, pos: usize) {
        let Some(book) = self.books.get(pos) else { return };
        if self.reader.as_ref().is_none_or(|r| r.book != book.id) {
            let reader = match self.reader_books.get(&book.id) {
                Some(saved) if saved.chapter < book.chapters.len() => {
                    let mut reader = Reader::new(book, saved.chapter);
                    reader.go_to(saved.offset, &saved.text);
                    reader
                }
                _ => Reader::new(book, 0),
            };
            self.reader = Some(reader);
        }
        self.view = View::Reader;
    }

    /// Reopen the reader on the book it showed when the app was closed, once that book is ready; then keep
    /// the saved view up to date.
    fn restore_view(&mut self) {
        if let Some(id) = self.wanted_reader {
            if let Some(pos) = self.books.iter().position(|b| b.id == id) {
                self.wanted_reader = None;
                self.open_reader(pos);
            } else if self.saved_book_name(id).is_none() || matches!(self.dictionary, DictionaryState::Failed(_)) {
                // The book is gone, or could not be read or analyzed: the statistics instead
                self.wanted_reader = None;
            }
            return;
        }
        let reading = self.reader.as_ref().filter(|_| self.view == View::Reader).map(|r| r.book);
        if reading != self.view_state.reading {
            self.view_state = ViewState { reading, modified: stamp(self.view_state.modified) };
            self.view_touched = true;
        }
    }

    /// Save where the reader is in its book (once it differs from the saved place).
    fn remember_place(&mut self) {
        if let Some(reader) = &self.reader {
            self.store_place(reader.book, reader.place());
        }
    }

    fn store_place(&mut self, book: u64, place: BookReaderState) {
        // Reading on here: this place replaces the one another device offered
        let offered = match self.place_offer.take() {
            Some((id, offer)) if id == book => offer.modified,
            offer => {
                self.place_offer = offer;
                0
            }
        };
        let previous = self.reader_books.get(&book);
        if offered > 0 || previous.is_none_or(|saved| !saved.same_place(&place)) {
            let modified = stamp(previous.map_or(0, |p| p.modified).max(offered));
            self.reader_books.insert(book, BookReaderState { modified, ..place });
        }
    }

    /// The name of a book still being restored or analyzed.
    fn saved_book_name(&self, id: u64) -> Option<&str> {
        let saved = self.restoring.iter().chain(&self.to_restore).find(|b| b.id == id);
        saved.or_else(|| self.pending.iter().map(|b| &b.saved).find(|b| b.id == id)).map(|b| b.name.as_str())
    }

    /// The reader, while the book it showed when the app was closed is not ready yet: its bar, and a note in
    /// place of the text (rather than the statistics for a moment).
    fn opening_reader_view(&mut self, ui: &mut Ui, palette: Palette) {
        let name = self.wanted_reader.and_then(|id| self.saved_book_name(id)).unwrap_or_default().to_string();
        ui.horizontal(|ui| {
            let button = egui::Button::new(RichText::new("📊").size(15.0)).min_size(egui::vec2(28.0, 24.0));
            if ui.add(button).on_hover_text("Back to the statistics").clicked() {
                self.wanted_reader = None;
            }
            ui.add(egui::Label::new(RichText::new(name).strong()).truncate());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new("⚙").selected(self.show_settings)).on_hover_text("Settings, with the font size and text direction").clicked() {
                    self.show_settings = !self.show_settings;
                }
            });
        });
        ui.add_space(8.0);
        ui.label(RichText::new("Opening the book…").color(palette.secondary_text));
    }

    /// The reader, full screen, under a bar to switch views and chapters.
    fn reader_view(&mut self, ui: &mut Ui, palette: Palette) {
        let Some(reader) = self.reader.as_mut() else { return };
        // The book is missing while it is analyzed again: the chapter stays readable, without navigation
        let book = self.books.iter().find(|b| b.id == reader.book);
        let mut chapter = reader.chapter;
        let mut toggle = false;
        ui.horizontal(|ui| {
            let button = egui::Button::new(RichText::new("📊").size(15.0)).min_size(egui::vec2(28.0, 24.0));
            toggle = ui.add(button).on_hover_text("Back to the statistics").clicked();
            // Phones only have room for the chapter
            if book.is_none() || ui.available_width() > 520.0 {
                ui.add(egui::Label::new(RichText::new(reader.book_name()).strong()).truncate());
            }
            if ui.add_enabled(book.is_some() && chapter > 0, egui::Button::new("‹")).on_hover_text("Previous chapter").clicked() {
                chapter -= 1;
            }
            // From the right: settings, next chapter, then the chapter list in the room left
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new("⚙").selected(self.show_settings)).on_hover_text("Settings, with the font size and text direction").clicked() {
                    self.show_settings = !self.show_settings;
                }
                let Some(book) = book else { return };
                if ui.add_enabled(chapter + 1 < book.chapters.len(), egui::Button::new("›")).on_hover_text("Next chapter").clicked() {
                    chapter += 1;
                }
                egui::ComboBox::from_id_salt("reader-chapter")
                    // The box adds its padding and arrow to this width
                    .width((ui.available_width() - 40.0).clamp(40.0, 320.0))
                    .height(400.0)
                    .truncate()
                    .selected_text(book.chapter_label(chapter))
                    .show_ui(ui, |ui| {
                        list_rows(ui, |ui| {
                            for pos in 0..book.chapters.len() {
                                ui.selectable_value(&mut chapter, pos, book.chapter_label(pos));
                            }
                        });
                    });
            });
        });
        // A newer place from another device: the page stays until the reader chooses
        let mut offer_taken = None;
        if let Some((_, place)) = self.place_offer.as_ref().filter(|(id, _)| *id == reader.book) {
            ui.horizontal_wrapped(|ui| {
                let at = book.filter(|b| place.chapter < b.chapters.len()).map_or_else(String::new, |b| format!(": {}", b.chapter_label(place.chapter)));
                ui.label(RichText::new(format!("Newer place from another device{at}")).small().color(palette.secondary_text));
                if ui.small_button("Go there").clicked() {
                    offer_taken = Some(true);
                }
                if ui.small_button("Stay here").clicked() {
                    offer_taken = Some(false);
                }
            });
        }
        if reader.needs_words() {
            if let DictionaryState::Ready { tokenizer, .. } = &mut self.dictionary {
                // Every word the tokenizer finds can be looked up, whether word lists count it or not
                let started = Instant::now();
                timeline::step("tokenizing the chapter", || reader.tokenize(tokenizer));
                timeline::note(format!("reader: chapter tokenized in {:.0} ms", timeline::since(started)));
            }
        }
        let response = reader.ui(ui, self.lookup.as_ref(), &mut self.reader_font_size, self.reader_vertical, self.reader_paginate, self.reader_highlight, palette);
        let chapter = response.chapter.unwrap_or(chapter);
        let settled = reader.settled_place(ui.ctx()).map(|place| (reader.book, place));
        if let Some(book) = book.filter(|_| chapter != reader.chapter) {
            let mut next = Reader::new(book, chapter);
            if response.chapter == Some(chapter) {
                if response.at_end {
                    next.go_to_end();
                }
                // A drag turning pages goes on in that chapter, and can come back
                if let Some(drag) = response.drag {
                    next.continue_drag(drag);
                }
            }
            self.reader = Some(next);
        }
        if let Some((book, place)) = settled {
            self.store_place(book, place);
        }
        match (offer_taken, self.place_offer.take()) {
            (Some(true), Some((id, place))) => {
                if let Some(book) = self.books.iter().find(|b| b.id == id && place.chapter < b.chapters.len()) {
                    let mut reader = Reader::new(book, place.chapter);
                    reader.go_to(place.offset, &place.text);
                    self.reader = Some(reader);
                }
                self.reader_books.insert(id, place);
            }
            // Stay: this place, stamped after the offered one, so it replaces it
            (Some(false), Some((id, offered))) => {
                if let Some(here) = self.reader.as_ref().filter(|r| r.book == id).map(Reader::place) {
                    let previous = self.reader_books.get(&id).map_or(0, |p| p.modified);
                    self.reader_books.insert(id, BookReaderState { modified: stamp(previous.max(offered.modified)), ..here });
                }
            }
            (_, offer) => self.place_offer = offer,
        }
        if toggle {
            self.toggle_view();
        }
    }

    /// Recompute the shown book's statistics if the books or progress changed.
    fn refresh_stats(&mut self) {
        if self.books.is_empty() {
            self.stats = None;
            return;
        }
        self.shown = self.shown.min(self.books.len() - 1);
        let key = self.progress_key(self.shown);
        if self.stats.as_ref().is_none_or(|(k, _)| *k != key) {
            self.stats = Some((key, Book::stats(&self.books, self.shown)));
        }
    }

    fn sidebar(&mut self, ui: &mut Ui, palette: Palette) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            self.view_toggle(ui);
            ui.label(RichText::new("Books").heading());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let button = ui.add(egui::Button::new("⚙ Settings").selected(self.show_settings));
                if button.on_hover_text("Word list settings and the reader's font size").clicked() {
                    self.show_settings = !self.show_settings;
                }
            });
        });
        ui.add_space(2.0);
        if ui.add(theme::primary_button("Open EPUB or PDF…", palette)).clicked() {
            platform::pick_files(&self.inbox, ui.ctx());
        }
        ui.label(RichText::new("You can also drop files on the window.").small().color(palette.secondary_text));
        if let Some(status) = self.account.status() {
            if status.downloads > 0 {
                let books = if status.downloads == 1 { "a book".to_string() } else { format!("{} books", status.downloads) };
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(format!("Downloading {books} from Dropbox…")).small().color(palette.secondary_text));
                });
            }
        }

        let total = WEB_DICTIONARY_FILES.len();
        match &self.dictionary {
            DictionaryState::Checking { .. } => {
                ui.add(egui::ProgressBar::new(0.0).text("Checking the dictionary…"));
            }
            DictionaryState::Loading { files, received, from_storage: true, .. } => {
                let text = format!("Loading the saved dictionary ({}/{total})…", files.len());
                ui.add(egui::ProgressBar::new((received + files.len()) as f32 / (2 * total) as f32).text(text));
            }
            DictionaryState::Loading { files, received, .. } => {
                let text = if *received < total {
                    format!("Downloading the dictionary ({received}/{total})…")
                } else {
                    format!("Preparing the dictionary ({}/{total})…", files.len())
                };
                ui.add(egui::ProgressBar::new((received + files.len()) as f32 / (2 * total) as f32).text(text));
            }
            DictionaryState::Failed(e) => {
                ui.colored_label(ui.visuals().error_fg_color, format!("Dictionary unavailable: {e}"));
            }
            DictionaryState::Ready { .. } => {}
        }
        if let Some(summary) = self.load_times.summary().filter(|_| self.debug) {
            ui.label(RichText::new(summary).small().color(palette.secondary_text));
        }
        if let Some(bytes) = platform::memory_bytes().filter(|_| self.debug) {
            let text = format!("Memory used at most: {} MB", bytes / 1_000_000);
            ui.label(RichText::new(text).small().color(palette.secondary_text));
        }
        if self.debug {
            egui::CollapsingHeader::new(RichText::new("Startup timeline").small().color(palette.secondary_text)).id_salt("startup-timeline").show(ui, |ui| {
                for line in timeline::lines() {
                    ui.label(RichText::new(line).small().monospace().color(palette.secondary_text));
                }
            });
        }
        if self.debug && self.persistent == Some(false) {
            let text = "This browser did not grant persistent storage: it may clear the saved books when it needs room.";
            ui.label(RichText::new(text).small().color(palette.secondary_text));
        }
        if let Some(e) = &self.storage_error {
            ui.label(RichText::new("Books cannot be saved in this browser, so they will not be kept.").small().color(palette.secondary_text));
            if self.debug {
                ui.label(RichText::new(e).small().color(palette.secondary_text));
            }
        }
        ui.add_space(8.0);

        let remaining = self.remaining_counts();
        // Id of the book to remove, whether it is ready, being analyzed or being restored
        let mut remove = None;
        let remove_button = |ui: &mut Ui, remove: &mut Option<u64>, id: u64| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new("✕").frame(false)).on_hover_text("Remove this book").clicked() {
                    *remove = Some(id);
                }
            });
        };
        {
            let any_learned = self.books.iter().any(|b| b.learned.is_some());
            for (i, book) in self.books.iter_mut().enumerate() {
                let selected = i == self.shown;
                let others_learned = any_learned && remaining[i] < book.words.iter().map(Vec::len).sum();
                // The whole card selects the book; its button and chapter selector take their own clicks
                let card = ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
                    let hovered = ui.response().hovered();
                    let mut frame = theme::card(palette);
                    if selected {
                        frame = frame.stroke(egui::Stroke::new(1.5, palette.accent));
                    } else if hovered {
                        frame = frame.stroke(egui::Stroke::new(1.0, palette.accent.linear_multiply(0.5)));
                    }
                    frame.show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.add(egui::Label::new(RichText::new(&book.name).strong()).truncate());
                            remove_button(ui, &mut remove, book.id);
                        });
                        let total: usize = book.words.iter().map(Vec::len).sum();
                        ui.label(
                            RichText::new(format!("{} chapters · {} words to learn", book.chapters.len(), format_count(&(total as f64))))
                                .small()
                                .color(palette.secondary_text),
                        );
                        if others_learned {
                            ui.label(
                                RichText::new(format!("{} left after your other books", format_count(&(remaining[i] as f64))))
                                    .small()
                                    .color(palette.accent),
                            );
                        }
                        ui.label(RichText::new("Learned up to (included)").small().color(palette.secondary_text));
                        let selected_text = book.learned.map_or("Not started".to_string(), |pos| book.chapter_label(pos));
                        let before = book.learned;
                        egui::ComboBox::from_id_salt(("learned", i))
                            .width(ui.available_width())
                            .height(400.0)
                            .truncate()
                            .selected_text(selected_text)
                            .show_ui(ui, |ui| {
                                list_rows(ui, |ui| {
                                    ui.selectable_value(&mut book.learned, None, "Not started");
                                    for pos in 0..book.chapters.len() {
                                        let label = book.chapter_label(pos);
                                        ui.selectable_value(&mut book.learned, Some(pos), label);
                                    }
                                });
                            });
                        if book.learned != before {
                            book.modified = stamp(book.modified);
                        }
                    });
                });
                if card.response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    self.shown = i;
                    self.wanted_shown = None;
                }
                ui.add_space(6.0);
            }
            for book in &self.pending {
                theme::card(palette).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.add(egui::Label::new(RichText::new(&book.saved.name).strong()).truncate());
                        remove_button(ui, &mut remove, book.saved.id);
                    });
                    let waiting = !matches!(self.dictionary, DictionaryState::Ready { .. });
                    let text = if waiting { "Waiting for the dictionary…" } else { "Analyzing chapters…" };
                    ui.add(egui::ProgressBar::new(book.progress()).text(text));
                });
                ui.add_space(6.0);
            }
            for saved in self.restoring.iter().chain(&self.to_restore) {
                theme::card(palette).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.add(egui::Label::new(RichText::new(&saved.name).strong()).truncate());
                        remove_button(ui, &mut remove, saved.id);
                    });
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Restoring…").small().color(palette.secondary_text));
                    });
                });
                ui.add_space(6.0);
            }
            if !self.errors.is_empty() {
                for e in &self.errors {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                if ui.small_button("Clear messages").clicked() {
                    self.errors.clear();
                }
            }
        }
        if let Some(id) = remove {
            self.remove_book(ui.ctx(), id);
        }
    }

    /// Remove a book, here and on the other devices (a record of the removal is synced), at any stage: ready,
    /// being analyzed, or being restored (so a book whose analysis makes the page reload can still be removed).
    fn remove_book(&mut self, ctx: &egui::Context, id: u64) {
        let books = self.current_state().books;
        if let Some(book) = books.iter().find(|b| b.id == id) {
            // Not if the same file is open twice (from before books were named by their contents)
            if let Some(hash) = book.hash.as_ref().filter(|h| !books.iter().any(|b| b.id != id && b.hash.as_ref() == Some(*h))) {
                let previous = self.removed.get(hash).copied().unwrap_or(0).max(book.modified);
                self.removed.insert(hash.clone(), stamp(previous));
            }
        }
        self.forget_book(id);
        // Save the book list now, before any further work that could make the page reload
        self.save_state(ctx);
    }

    /// Forget a book and its stored files, at any stage.
    fn forget_book(&mut self, id: u64) {
        if let Some(i) = self.books.iter().position(|b| b.id == id) {
            self.books.remove(i);
            if self.shown > i {
                self.shown -= 1;
            }
            self.generation += 1;
        }
        // Answers still to come for a restored book are ignored once it is no longer listed
        self.pending.retain(|b| b.saved.id != id);
        self.restoring.retain(|b| b.id != id);
        self.decoded.retain(|b| b.id() != id);
        self.to_restore.retain(|b| b.id != id);
        if self.wanted_shown == Some(id) {
            self.wanted_shown = None;
        }
        self.reader_books.remove(&id);
        if self.reader.as_ref().is_some_and(|r| r.book == id) {
            self.reader = None;
        }
        if self.wanted_reader == Some(id) {
            self.wanted_reader = None;
        }
        if self.place_offer.as_ref().is_some_and(|(book, _)| *book == id) {
            self.place_offer = None;
        }
        self.sync_reads.remove(&id);
        storage::remove_book(&self.inbox, &self.ctx, id);
    }

    /// `viewport_height` is the visible height (the content scrolls when it does not fit).
    /// When `stacked`, the plots get a fixed height instead of filling the window.
    fn central(&mut self, ui: &mut Ui, palette: Palette, viewport_height: f32, stacked: bool) {
        let top = ui.cursor().top();
        if self.books.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(if stacked { 16.0 } else { viewport_height / 3.0 });
                ui.label(RichText::new("No books yet").heading());
                ui.label(RichText::new("Open one or more EPUB or PDF files to see their vocabulary statistics.").color(palette.secondary_text));
                ui.add_space(8.0);
                if ui.add(theme::primary_button("Open EPUB or PDF…", palette)).clicked() {
                    platform::pick_files(&self.inbox, ui.ctx());
                }
            });
            return;
        }

        // Header: book selector and download
        let names: Vec<String> = self.books.iter().map(|b| b.name.clone()).collect();
        let mut shown = self.shown.min(names.len() - 1);
        let mut download = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Statistics for").color(palette.secondary_text));
            egui::ComboBox::from_id_salt("shown")
                .width(ui.available_width().min(320.0))
                .truncate()
                .selected_text(&names[shown])
                .show_ui(ui, |ui| {
                    list_rows(ui, |ui| {
                        for (i, name) in names.iter().enumerate() {
                            ui.selectable_value(&mut shown, i, name);
                        }
                    });
                });
            let button = theme::primary_button("Download word lists (.zip)", palette);
            download = if stacked {
                ui.add(button).clicked()
            } else {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.add(button).clicked()).inner
            };
        });
        if shown != self.shown {
            self.wanted_shown = None;
        }
        self.shown = shown;
        let learned_x = self.books[shown].learned.map(|pos| self.books[shown].chapters[pos].index as f64);
        let others_learning = self.books.iter().enumerate().any(|(i, b)| i != shown && b.learned.is_some());

        self.refresh_stats();
        let Some((_, stats)) = &self.stats else { return };
        let excluded = stats.excluded;
        let note = if others_learning {
            format!("{} of this book's words are already learned in your other books and are left out.", format_count(&(excluded as f64)))
        } else {
            "Set your progress in other books to leave out the words you already know.".to_string()
        };
        ui.label(RichText::new(note).small().color(palette.secondary_text));
        ui.add_space(4.0);

        if download {
            let book = &self.books[self.shown];
            let result = stats.words_zip(book).and_then(|bytes| platform::save_file(&format!("{}_words.zip", book.name), bytes));
            if let Err(e) = result {
                self.errors.push(format!("Download failed: {e:#}"));
            }
        }

        let metrics = [
            Metric {
                title: "New words to learn",
                hint: None,
                short: "Words",
                scale: 0,
                color: palette.teal,
                value: |s, i| s.chapters[i].filtered_words as f64,
                format: format_count,
                total: false,
            },
            Metric {
                title: "Vocabulary density",
                hint: Some("New words per 100 words of the chapter: the share of its words that are new (particles not counted)"),
                short: "Density",
                scale: 2,
                color: palette.blue,
                value: |s, i| per_100(s.chapters[i].filtered_words, s.chapter_words[i]),
                format: format_ratio,
                total: false,
            },
            Metric {
                title: "Words learned so far",
                hint: Some("All the words to learn from the start of the book up to and including each chapter"),
                short: "Learned",
                scale: 3,
                color: palette.green,
                value: |s, i| s.learned_so_far[i] as f64,
                format: format_count,
                total: true,
            },
            Metric {
                title: "Chapter length (characters)",
                hint: None,
                short: "Characters",
                scale: 1,
                color: palette.purple,
                value: |s, i| s.chapters[i].characters as f64,
                format: format_count,
                total: false,
            },
        ];
        // Fill the window with the cards, each adding its margins, border, title row and the
        // spacing after it; below a minimum height the page scrolls instead
        let card_overhead = 2.0 * 12.0 + 2.0 + 22.0 + 2.0 * ui.spacing().item_spacing.y;
        let header = ui.cursor().top() - top;
        let count = metrics.len() as f32;
        let height = if stacked { MIN_PLOT_HEIGHT } else { ((viewport_height - header - count * card_overhead) / count).max(MIN_PLOT_HEIGHT) };
        // While inspecting (touch screens), the finger is not a hover for egui, so the charts draw
        // the inspected chapter themselves, and a tooltip follows the finger
        let finger = if self.inspecting.active { ui.ctx().input(|i| i.pointer.latest_pos()) } else { None };
        let inspect = Inspect { finger, chapter: finger.and(self.inspected_chapter) };
        let mut toggled = None;
        let mut plot_rects = Vec::with_capacity(metrics.len());
        let mut inspected = None;
        for metric in &metrics {
            theme::card(palette).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let log = self.log_scales[metric.scale];
                let drawn = metric_plot(ui, metric, log, stats, learned_x, height, palette, inspect);
                plot_rects.push(drawn.rect);
                inspected = inspected.or(drawn.inspected);
                if drawn.toggle {
                    toggled = Some(metric.scale);
                }
            });
        }
        self.plot_rects = plot_rects;
        self.inspected_chapter = inspected;
        if let (Some(finger), Some(text)) = (finger, inspected.and_then(|x| chapter_text(stats, x))) {
            egui::Area::new(egui::Id::unique("inspected-chapter"))
                .order(egui::Order::Tooltip)
                .pivot(egui::Align2::CENTER_BOTTOM)
                .fixed_pos(finger - egui::vec2(0.0, 28.0))
                .interactable(false)
                .show(ui.ctx(), |ui| egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(text)));
        }
        if let Some(i) = toggled {
            self.log_scales[i] = !self.log_scales[i];
        }
    }
}

/// How to use the reader, for its current text direction and pagination: a short list at the bottom of the
/// settings window, until the reader's actions have a place of their own.
fn reader_help(ui: &mut Ui, vertical: bool, paginate: bool, palette: Palette) {
    ui.label(RichText::new("How to use the reader").strong());
    let (next, previous, next_key, previous_key) = if vertical { ("right", "left", "←", "→") } else { ("left", "right", "→", "←") };
    let mut items = vec![
        "Click or tap a word to see its dictionary entry, then a kanji in that window to see the kanji.".to_string(),
        "Escape, or a click beside the words, closes them.".to_string(),
    ];
    if paginate {
        items.push(format!("Drag or swipe toward the {next} for the next page, toward the {previous} for the previous one; keep holding and move back to stay on the page."));
        items.push(format!("The {next_key} key shows the next page, {previous_key} the previous one."));
    } else {
        items.push(if vertical { "The mouse wheel scrolls through the columns.".to_string() } else { "The mouse wheel scrolls through the text.".to_string() });
    }
    items.push("Shift or Cmd/Ctrl + the mouse wheel, or a pinch, changes the font size.".to_string());
    for item in items {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new("•").small().color(palette.secondary_text));
            ui.add(egui::Label::new(RichText::new(item).small().color(palette.secondary_text)).wrap());
        });
    }
}

/// Rows of a drop-down list that fill its width with no gaps, so a click anywhere on a row selects it.
fn list_rows(ui: &mut Ui, rows: impl FnOnce(&mut Ui)) {
    ui.spacing_mut().item_spacing.y = 0.0;
    ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), rows);
}

/// On touch screens, a still long press on a chart starts inspecting it, until the finger is lifted.
#[derive(Default)]
struct LongPress {
    active: bool,
}

impl LongPress {
    /// Returns how long to wait (seconds) before the current press may become long enough.
    fn update(&mut self, input: &egui::InputState, charts: &[egui::Rect]) -> Option<f64> {
        let pointer = &input.pointer;
        if !pointer.primary_down() {
            self.active = false;
            return None;
        }
        if self.active || !input.any_touches() {
            return None;
        }
        let (origin, start, pos) = (pointer.press_origin()?, pointer.press_start_time()?, pointer.latest_pos()?);
        if !charts.iter().any(|r| r.contains(origin)) || origin.distance(pos) > LONG_PRESS_SLOP {
            return None;
        }
        let held = input.time - start;
        self.active = held >= LONG_PRESS;
        (!self.active).then_some(LONG_PRESS - held)
    }
}

struct Metric {
    title: &'static str,
    /// Shown when hovering the title
    hint: Option<&'static str>,
    short: &'static str,
    /// Index of its log scale setting in `log_scales`
    scale: usize,
    color: Color32,
    /// Value for the chapter at a position
    value: fn(&BookStats, usize) -> f64,
    format: fn(&f64) -> String,
    /// Summarized by its last value (a running total) rather than min, average and max
    total: bool,
}

/// A chart being inspected on a touch screen.
#[derive(Clone, Copy)]
struct Inspect {
    /// Where the finger is
    finger: Option<egui::Pos2>,
    /// The chapter under it, to mark in every chart
    chapter: Option<f64>,
}

struct DrawnPlot {
    /// The scale label was clicked
    toggle: bool,
    rect: egui::Rect,
    /// Chapter under the finger, if it is on this plot
    inspected: Option<f64>,
}

/// Draws one plot card.
#[allow(clippy::too_many_arguments)]
fn metric_plot(
    ui: &mut Ui,
    m: &Metric,
    log: bool,
    stats: &BookStats,
    learned_x: Option<f64>,
    height: f32,
    palette: Palette,
    inspect: Inspect,
) -> DrawnPlot {
    let chapters = &stats.chapters;
    let values: Vec<f64> = (0..chapters.len()).map(|i| (m.value)(stats, i)).collect();
    let (min, max) = values.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    let avg = values.iter().sum::<f64>() / values.len().max(1) as f64;
    let f = m.format;
    let mut toggle = false;
    let wide = ui.available_width() > 640.0;
    let summary = if m.total {
        format!("total {}", f(values.last().unwrap_or(&0.0)))
    } else {
        format!("min {} · avg {} · max {}", f(&min), f(&avg), f(&max))
    };
    let summary = RichText::new(summary).small().color(palette.secondary_text);
    ui.horizontal_wrapped(|ui| {
        let title = ui.label(RichText::new(m.title).strong());
        if let Some(hint) = m.hint {
            title.on_hover_text(hint);
        }
        let (scale, other) = if log { ("log scale", "linear") } else { ("linear scale", "log") };
        let button = egui::Button::new(RichText::new(format!("{scale} ⇄")).small().color(palette.accent)).frame(false);
        toggle = ui
            .add(button)
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(format!("Switch to a {other} scale"))
            .clicked();
        if wide {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(summary.clone()));
        }
    });
    if !wide {
        ui.label(summary);
    }

    // Log axes plot log10 of the values; non-positive values cannot be shown there
    let to_y = |v: f64| if log { v.log10() } else { v };
    let points: Vec<[f64; 2]> = chapters
        .iter()
        .zip(&values)
        .filter(|&(_, &v)| !log || v > 0.0)
        .map(|(s, &v)| [s.chapter_index as f64, to_y(v)])
        .collect();

    let x_ticks = (ui.available_width() as f64 / 70.0).clamp(3.0, 12.0);
    // The scale is part of the id, so switching it resets the y range
    let mut plot = Plot::new(("metric", m.short, log))
        .height(height)
        // Hover only: no panning or zooming, and drags go to the page (scrolling on touch screens)
        .sense(egui::Sense::hover())
        .allow_zoom(false)
        .allow_drag(false)
        .allow_scroll(false)
        .allow_boxed_zoom(false)
        .allow_double_click_reset(false)
        .allow_axis_zoom_drag(false)
        .link_cursor(egui::Id::unique("chapter-cursor"), [true, false])
        .y_axis_min_width(48.0)
        .show_grid([false, true])
        // Same chapter range in every plot, even when a log plot cannot show its zero values
        .include_x(chapters.first().map_or(0.0, |c| c.chapter_index as f64 - 0.5))
        .include_x(chapters.last().map_or(1.0, |c| c.chapter_index as f64 + 0.5))
        // About one chapter label per 70 points, so labels stay readable on phones
        .x_grid_spacer(move |input| nice_grid(input, x_ticks, 1.0))
        .x_axis_formatter(|mark, _| if mark.value.fract() == 0.0 { format!("{:.0}", mark.value) } else { String::new() })
        .y_axis_formatter(move |mark, _| f(&if log { 10f64.powf(mark.value) } else { mark.value }))
        .label_formatter(move |pos| hover_text(pos, stats));
    plot = if log { plot.y_grid_spacer(log_grid) } else { plot.y_grid_spacer(|input| nice_grid(input, 5.0, 0.0)) };
    let shown = plot.show(ui, |plot_ui| {
        if let Some(x) = learned_x {
            plot_ui.vline(VLine::new("Learned up to here", x).color(palette.orange).width(2.0));
        }
        if let Some(x) = inspect.chapter {
            plot_ui.vline(VLine::new("Inspected", x).color(palette.secondary_text).width(1.0));
        }
        plot_ui.line(Line::new(m.short, points.clone()).color(m.color).width(1.8));
        plot_ui.points(Points::new(m.short, points).color(m.color).radius(if chapters.len() <= 60 { 3.0 } else { 1.8 }));
        let finger = inspect.finger.filter(|&p| plot_ui.response().rect.contains(p));
        finger.map(|p| plot_ui.plot_from_screen(p).x.round())
    });
    DrawnPlot { toggle, rect: shown.response.rect, inspected: shown.inner }
}

/// Every metric of the chapter under the cursor.
fn hover_text(pos: &HoverPosition<'_>, stats: &BookStats) -> Option<String> {
    let x = match pos {
        HoverPosition::NearDataPoint { position, .. } | HoverPosition::Elsewhere { position } => position.x,
    };
    chapter_text(stats, x)
}

/// Every metric of the chapter at `x` on the chapter axis.
fn chapter_text(stats: &BookStats, x: f64) -> Option<String> {
    let i = stats.chapters.iter().position(|s| s.chapter_index as f64 == x.round())?;
    let s = &stats.chapters[i];
    Some(format!(
        "{}: {}\nNew words: {}\nDensity: {} per 100 words\nLearned so far: {}\nLength: {} words, {} characters",
        s.chapter_index,
        s.title,
        format_count(&(s.filtered_words as f64)),
        format_ratio(&per_100(s.filtered_words, stats.chapter_words[i])),
        format_count(&(stats.learned_so_far[i] as f64)),
        format_count(&(stats.chapter_words[i] as f64)),
        format_count(&(s.characters as f64)),
    ))
}

/// Evenly spaced grid lines at a 1-2-5 step giving about `target` lines, and at least `min_step` apart.
fn nice_grid(input: GridInput, target: f64, min_step: f64) -> Vec<GridMark> {
    let (lo, hi) = input.bounds;
    let span = hi - lo;
    if span.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return Vec::new();
    }
    let raw = span / target;
    let magnitude = 10f64.powf(raw.log10().floor());
    let step = magnitude * [1.0, 2.0, 5.0, 10.0].into_iter().find(|&m| raw <= m * magnitude).unwrap_or(10.0);
    let step = step.max(min_step);
    let first = (lo / step).ceil() as i64;
    let last = (hi / step).floor() as i64;
    (first..=last).map(|k| GridMark { value: k as f64 * step, step_size: step }).collect()
}

/// Grid lines at 1-2-5 steps of a log10 axis.
fn log_grid(input: GridInput) -> Vec<GridMark> {
    let (lo, hi) = input.bounds;
    if hi.partial_cmp(&lo) != Some(std::cmp::Ordering::Greater) || hi - lo > 12.0 {
        return Vec::new();
    }
    log_ticks(10f64.powf(lo), 10f64.powf(hi))
        .into_iter()
        .map(|v| {
            let value = v.log10();
            GridMark { value, step_size: if value.fract().abs() < 1e-9 { 1.0 } else { 0.3 } }
        })
        .collect()
}

impl eframe::App for ViewerApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.frame_started = Instant::now();
        timeline::frame_started();
        let ctx = ui.ctx().clone();
        timeline::step("storage replies", || self.handle_incoming(&ctx));
        timeline::step("settings", || self.apply_settings(&ctx));
        if self.state_loaded {
            self.restore_view();
        }
        if let Some(wait) = ctx.input(|i| self.inspecting.update(i, &self.plot_rects)) {
            // Nothing else happens while the finger stays still: wake up when the press is long enough
            ctx.request_repaint_after(Duration::from_secs_f64(wait));
        }
        let palette = Palette::of(ui.visuals());
        // While inspecting a chart, dragging moves the hover instead of scrolling
        let scrolling = if self.inspecting.active {
            egui::scroll_area::ScrollSource { drag: egui::scroll_area::DragScroll::Never, ..Default::default() }
        } else {
            egui::scroll_area::ScrollSource::default()
        };

        let content = egui::Frame::new().fill(palette.content).inner_margin(egui::Margin::same(18));
        let reader_frame = egui::Frame::new().fill(palette.content).inner_margin(egui::Margin::symmetric(16, 8));
        if !self.state_loaded {
            // Nothing until the saved state is read (on the web, from storage, a few frames later): it says
            // whether to show the reader or the statistics
            egui::CentralPanel::default().frame(egui::Frame::new().fill(palette.content)).show(ui, |_| {});
        } else if self.view == View::Reader && self.reader.is_some() {
            self.plot_rects.clear();
            egui::CentralPanel::default().frame(reader_frame).show(ui, |ui| self.reader_view(ui, palette));
        } else if self.wanted_reader.is_some() {
            self.plot_rects.clear();
            egui::CentralPanel::default().frame(reader_frame).show(ui, |ui| self.opening_reader_view(ui, palette));
        } else if ui.available_width() < STACK_BELOW_WIDTH {
            // Phones and narrow windows: the books, then the charts, in one scrolling column
            egui::CentralPanel::default().frame(content).show(ui, |ui| {
                let viewport_height = ui.available_height();
                egui::ScrollArea::vertical().auto_shrink(false).scroll_source(scrolling).show(ui, |ui| {
                    egui::Frame::new()
                        .fill(palette.sidebar)
                        .corner_radius(egui::CornerRadius::same(10))
                        .inner_margin(egui::Margin::same(12))
                        .show(ui, |ui| self.sidebar(ui, palette));
                    ui.add_space(12.0);
                    self.central(ui, palette, viewport_height, true);
                });
            });
        } else {
            egui::Panel::left("books")
                .resizable(true)
                .default_size(290.0)
                .frame(egui::Frame::new().fill(palette.sidebar).inner_margin(egui::Margin::same(14)))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.sidebar(ui, palette));
                });
            egui::CentralPanel::default().frame(content).show(ui, |ui| {
                let viewport_height = ui.available_height();
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .scroll_source(scrolling)
                    .show(ui, |ui| self.central(ui, palette, viewport_height, false));
            });
        }
        self.settings_window(&ctx, palette);
        // After drawing, so the window shows this frame while the slow work runs
        self.do_work(&ctx);
        timeline::step("sync", || self.sync_step(&ctx));
        timeline::step("saving the state", || self.save_state(&ctx));
        let loading = !self.state_loaded
            || !self.to_restore.is_empty()
            || !self.restoring.is_empty()
            || !self.pending.is_empty()
            || matches!(self.dictionary, DictionaryState::Checking { .. } | DictionaryState::Loading { .. });
        timeline::frame_ended(loading);
    }
}

/// The id of the next new book: after every id the saved state used (`saved_next`, `saved`) and every book
/// something is stored for (`stored`), so a new book never takes the files of another.
fn next_book_id(saved_next: u64, saved: &[u64], stored: &[u64]) -> u64 {
    saved.iter().chain(stored).map(|id| id + 1).max().unwrap_or(0).max(saved_next)
}

/// What the sync engine asks of the app.
impl sync::engine::Host for ViewerApp {
    fn local(&self) -> SyncDoc {
        self.local_records()
    }

    fn adopt(&mut self, remote: &SyncDoc) {
        self.adopt_records(remote);
    }

    fn local_files(&self) -> Vec<String> {
        self.books.iter().filter_map(|b| b.hash.clone()).collect()
    }

    fn can_download(&self) -> bool {
        self.current_state().books.iter().all(|b| b.hash.is_some() || self.unhashable.contains(&b.id))
    }

    fn read_source(&mut self, hash: &str) -> bool {
        let Some(id) = self.books.iter().find(|b| b.hash.as_deref() == Some(hash)).map(|b| b.id) else { return false };
        self.sync_reads.insert(id, hash.to_string());
        storage::read(&self.inbox, &self.ctx, Key::Source(id));
        true
    }

    fn downloaded(&mut self, hash: &str, record: &BookRecord, bytes: Vec<u8>) -> Result<(), String> {
        self.add_downloaded(hash, record, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, Event, Modifiers, PointerButton, RawInput, TouchDeviceId, TouchId, TouchPhase};

    #[test]
    fn new_books_never_take_the_id_of_stored_ones() {
        // Saved state and storage agree
        assert_eq!(next_book_id(2, &[0, 1], &[0, 1]), 2);
        // The saved state is gone or older, but a book is still stored: its files are not overwritten
        assert_eq!(next_book_id(0, &[], &[0]), 1);
        assert_eq!(next_book_id(3, &[0], &[0, 5]), 6);
        // Ids of removed books are not reused either
        assert_eq!(next_book_id(4, &[1], &[1]), 4);
        assert_eq!(next_book_id(0, &[], &[]), 0);
    }

    /// Feeds one frame at `time` and returns whether the chart is being inspected.
    fn frame(ctx: &egui::Context, press: &mut LongPress, time: f64, events: Vec<Event>) -> bool {
        let input = RawInput { time: Some(time), events, ..Default::default() };
        let chart = [egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(300.0, 200.0))];
        let mut output = ctx.run_ui(input, |ui| {
            ui.ctx().input(|i| press.update(i, &chart));
        });
        // No renderer applies the font texture in tests
        output.textures_delta.clear();
        press.active
    }

    fn touch(phase: TouchPhase, x: f32, y: f32) -> Vec<Event> {
        let pos = pos2(x, y);
        let touch = Event::Touch { device_id: TouchDeviceId(1), id: TouchId(1), phase, pos, force: None };
        match phase {
            TouchPhase::Start => vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }, touch],
            TouchPhase::Move => vec![Event::PointerMoved(pos), touch],
            _ => vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }, touch, Event::PointerGone],
        }
    }

    #[test]
    fn still_long_press_on_a_chart_inspects_until_released() {
        let (ctx, mut press) = (egui::Context::default(), LongPress::default());
        assert!(!frame(&ctx, &mut press, 1.0, touch(TouchPhase::Start, 100.0, 100.0)));
        assert!(!frame(&ctx, &mut press, 1.2, vec![]));
        assert!(frame(&ctx, &mut press, 1.5, vec![]));
        // Dragging keeps inspecting, however far the finger goes
        assert!(frame(&ctx, &mut press, 1.6, touch(TouchPhase::Move, 250.0, 120.0)));
        assert!(!frame(&ctx, &mut press, 1.7, touch(TouchPhase::End, 250.0, 120.0)));
        // A later quick drag scrolls again
        assert!(!frame(&ctx, &mut press, 3.0, touch(TouchPhase::Start, 100.0, 100.0)));
        assert!(!frame(&ctx, &mut press, 3.1, touch(TouchPhase::Move, 100.0, 40.0)));
        assert!(!frame(&ctx, &mut press, 3.6, vec![]));
    }

    #[test]
    fn long_press_outside_charts_or_with_a_mouse_does_not_inspect() {
        let (ctx, mut press) = (egui::Context::default(), LongPress::default());
        assert!(!frame(&ctx, &mut press, 1.0, touch(TouchPhase::Start, 500.0, 500.0)));
        assert!(!frame(&ctx, &mut press, 2.0, vec![]));

        let (ctx, mut press) = (egui::Context::default(), LongPress::default());
        let click = Event::PointerButton { pos: pos2(100.0, 100.0), button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE };
        assert!(!frame(&ctx, &mut press, 1.0, vec![Event::PointerMoved(pos2(100.0, 100.0)), click]));
        assert!(!frame(&ctx, &mut press, 2.0, vec![]));
    }
}
