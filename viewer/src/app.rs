use std::collections::HashMap;

use egui::{Color32, RichText, Ui};
use egui_plot::{GridInput, GridMark, HLine, HoverPosition, Line, LineStyle, Plot, Points, VLine};
use jpfe::chart::{format_count, format_ratio, log_ticks};
use jpfe::models::ChapterStat;
use jpfe::tokenizer::{JapaneseTokenizer, DICTIONARY_FILES};
use serde::{Deserialize, Serialize};
use web_time::{Duration, Instant};

use crate::book::{Book, BookStats, CacheKey, PendingBook, SavedBook, Settings};
use crate::platform::{self, Inbox, Incoming};
use crate::storage::{self, Key};
use crate::theme::{self, Palette};

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

/// Everything remembered between sessions, besides the files and analyses.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct SavedState {
    settings: Settings,
    log_scales: [bool; 3],
    shown: Option<u64>,
    next_id: u64,
    books: Vec<SavedBook>,
    /// Show load times and storage errors in the sidebar
    debug: bool,
}

impl Default for SavedState {
    fn default() -> Self {
        Self { settings: Settings::default(), log_scales: [true, false, true], shown: None, next_id: 0, books: Vec::new(), debug: false }
    }
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
    /// Log scale for each plot, in display order (words, characters, density)
    log_scales: [bool; 3],
    /// Settings the word lists are built with, and the ones being edited in the settings window
    settings: Settings,
    edited_settings: Settings,
    show_settings: bool,
    /// Show load times and storage errors in the sidebar
    debug: bool,
    /// For each book, its words not learned in the other books
    remaining: Option<(StatsKey, Vec<usize>)>,

    /// The saved state has been read; until then nothing is saved, so it is not overwritten
    state_loaded: bool,
    /// Last state written to storage
    saved: Option<SavedState>,
    next_id: u64,
    /// Saved books waiting for the dictionary, then the ones whose analysis or file is being read
    to_restore: Vec<SavedBook>,
    restoring: Vec<SavedBook>,
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
        storage::read(&inbox, ctx, Key::State);
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
            log_scales: [true, false, true],
            settings: Settings::default(),
            edited_settings: Settings::default(),
            show_settings: false,
            debug: false,
            remaining: None,
            state_loaded: false,
            saved: None,
            next_id: 0,
            to_restore: Vec::new(),
            restoring: Vec::new(),
            wanted_shown: None,
            dictionary_writes: None,
            storage_error: None,
            inspecting: LongPress::default(),
            plot_rects: Vec::new(),
            inspected_chapter: None,
            load_times: LoadTimes::new(),
        }
    }

    fn handle_incoming(&mut self, ctx: &egui::Context) {
        let mut deferred = Vec::new();
        for message in self.inbox.drain() {
            match message {
                // New books get ids after the saved ones, so wait for the saved state
                Incoming::File { .. } if !self.state_loaded => deferred.push(message),
                Incoming::File { name, bytes } => {
                    let id = self.next_id;
                    self.next_id += 1;
                    storage::write(&self.inbox, ctx, Key::Source(id), &bytes);
                    match PendingBook::parse(id, &name, bytes, None) {
                        Ok(book) => self.pending.push(book),
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
                                if *received == DICTIONARY_FILES.len() {
                                    self.load_times.fetched("downloading the dictionary");
                                }
                            }
                            Err(e) => self.dictionary = DictionaryState::Failed(format!("Downloading {name}: {e}")),
                        }
                    }
                }
                Incoming::Stored { key, bytes } => self.handle_stored(ctx, key, bytes),
                Incoming::Written(Key::DictionaryFile(_)) => {
                    if let Some((version, written)) = &mut self.dictionary_writes {
                        *written += 1;
                        // Written last, so an interrupted download is never taken as complete
                        if *written == DICTIONARY_FILES.len() {
                            storage::write(&self.inbox, ctx, Key::DictionaryVersion, version.as_bytes());
                            self.dictionary_writes = None;
                        }
                    }
                }
                Incoming::Written(_) => {}
                Incoming::StorageError(e) => {
                    log::warn!("Storage: {e}");
                    self.storage_error.get_or_insert(e);
                }
                Incoming::Font { name, bytes, primary } => {
                    let priority = if primary { egui::epaint::text::FontPriority::Highest } else { egui::epaint::text::FontPriority::Lowest };
                    let families = [egui::FontFamily::Proportional, egui::FontFamily::Monospace]
                        .map(|family| egui::epaint::text::InsertFontFamily { family, priority })
                        .to_vec();
                    ctx.add_font(egui::epaint::text::FontInsert::new(&name, egui::FontData::from_owned(bytes), families));
                }
                Incoming::Error(e) => self.errors.push(e),
            }
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
                let state: SavedState = bytes.and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
                self.settings = state.settings;
                self.edited_settings = state.settings;
                self.log_scales = state.log_scales;
                self.debug = state.debug;
                self.wanted_shown = state.shown;
                self.next_id = state.books.iter().map(|b| b.id + 1).max().unwrap_or(0).max(state.next_id);
                self.to_restore = state.books.clone();
                self.saved = Some(state);
                self.state_loaded = true;
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
                            if *received == DICTIONARY_FILES.len() {
                                self.load_times.fetched("reading the dictionary");
                            }
                            // One file at a time, to keep memory low
                            let next = DICTIONARY_FILES.iter().position(|f| *f == name).and_then(|i| DICTIONARY_FILES.get(i + 1));
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
                let book = self.load_times.timed("restoring books", || bytes.and_then(|b| Book::from_analysis(saved, &b, &key, &settings)));
                match book {
                    Some(book) => {
                        self.restoring.remove(pos);
                        self.insert_book(book);
                    }
                    // Missing, or made by another build or dictionary: analyze the book again
                    None => storage::read(&self.inbox, ctx, Key::Source(id)),
                }
            }
            Key::Source(id) => {
                let Some(pos) = self.restoring.iter().position(|b| b.id == id) else { return };
                let saved = self.restoring.remove(pos);
                match bytes.map(|b| PendingBook::parse(saved.id, &saved.file_name, b, saved.learned)) {
                    Some(Ok(book)) => self.pending.push(book),
                    Some(Err(e)) => self.errors.push(format!("{e:#}")),
                    None => self.errors.push(format!("{}: the stored file is missing; open it again", saved.name)),
                }
            }
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
            for saved in std::mem::take(&mut self.to_restore) {
                storage::read(&self.inbox, ctx, Key::Analysis(saved.id));
                self.restoring.push(saved);
            }
        }
        if self.state_loaded && self.to_restore.is_empty() && self.restoring.is_empty() && self.load_times.books.is_none() {
            self.load_times.books = Some(self.load_times.started.elapsed().as_secs_f64());
        }

        #[cfg(target_arch = "wasm32")]
        if self.prepare_dictionary(ctx) {
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
        tokenizer.set_include_particles(self.settings.include_particles);
        let started = Instant::now();
        let stepped = book.step(tokenizer, started + WORK_PER_FRAME);
        self.load_times.add("analyzing books", started.elapsed().as_secs_f64());
        match stepped {
            Ok(false) => {}
            Ok(true) => {
                let book = self.pending.remove(0).finish(&self.settings);
                let key = CacheKey { build: BUILD_ID, dictionary: &dictionary, include_particles: self.settings.include_particles };
                storage::write(&self.inbox, ctx, Key::Analysis(book.id), &book.analysis_bytes(&key));
                self.insert_book(book);
            }
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
                if let Some((name, bytes)) = compressed.pop() {
                    match self.load_times.timed("unpacking", || platform::decompress(&bytes)) {
                        Ok(data) => {
                            files.insert(name, data);
                        }
                        Err(e) => self.dictionary = DictionaryState::Failed(format!("Unpacking {name}: {e}")),
                    }
                } else if files.len() == DICTIONARY_FILES.len() {
                    let (version, mut files) = (version.clone(), std::mem::take(files));
                    log::info!("Dictionary {version} {}", if *from_storage { "from storage" } else { "downloaded" });
                    let dictionary = self.load_times.timed("loading the dictionary", || jpfe::tokenizer::dictionary_from_files(|name| files.remove(name)));
                    self.dictionary = match dictionary {
                        Ok(dictionary) => DictionaryState::Ready {
                            tokenizer: Box::new(JapaneseTokenizer::with_dictionary(dictionary, true, false)),
                            version,
                        },
                        Err(e) => DictionaryState::Failed(format!("{e:#}")),
                    };
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
        storage::read(&self.inbox, ctx, Key::DictionaryFile(DICTIONARY_FILES[0].to_string()));
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
            log_scales: self.log_scales,
            debug: self.debug,
            shown: self.wanted_shown.or_else(|| self.books.get(self.shown).map(|b| b.id)),
            next_id: self.next_id,
            books,
        }
    }

    fn save_state(&mut self, ctx: &egui::Context) {
        if !self.state_loaded {
            return;
        }
        let state = self.current_state();
        if self.saved.as_ref() != Some(&state) {
            if let Ok(json) = serde_json::to_vec(&state) {
                storage::write(&self.inbox, ctx, Key::State, &json);
            }
            self.saved = Some(state);
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
        egui::Window::new("Word list settings")
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
                let s = &mut self.edited_settings;
                let note = |ui: &mut Ui, text: &str| {
                    ui.label(RichText::new(text).small().color(palette.secondary_text));
                    ui.add_space(4.0);
                };
                ui.label("Coverage target");
                ui.add(egui::Slider::new(&mut s.target_percentage, 50.0..=100.0).suffix(" %").fixed_decimals(1));
                note(ui, "Each chapter's list stops once its words cover this share of the chapter's words.");

                ui.label("Minimum occurrences in the book");
                ui.add(egui::Slider::new(&mut s.min_book_count, 1..=20));
                note(ui, "Words that appear fewer times in the whole book are left out.");

                ui.checkbox(&mut s.hide_previous_words, "Only words new to the book");
                note(ui, "Leave out words already listed in an earlier chapter of the same book.");

                ui.checkbox(&mut s.hide_katakana_words, "Leave out katakana-only words");
                note(ui, "Mostly loanwords such as パソコン or エルフ, which are often easy to guess.");

                ui.checkbox(&mut s.include_particles, "Include particles and auxiliary verbs (助詞・助動詞)");
                note(ui, "Changing this analyzes the books again.");

                ui.separator();
                ui.checkbox(&mut self.debug, "Show debug information");
                note(ui, "Load times and storage errors, under the Open button.");

                ui.horizontal(|ui| {
                    if ui.button("Restore defaults").clicked() {
                        *s = Settings::default();
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
            ui.label(RichText::new("Books").heading());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let button = ui.add(egui::Button::new("⚙ Settings").selected(self.show_settings));
                if button.on_hover_text("Coverage target, minimum count and other word list settings").clicked() {
                    self.show_settings = !self.show_settings;
                }
            });
        });
        ui.add_space(2.0);
        if ui.add(theme::primary_button("Open EPUB or PDF…", palette)).clicked() {
            platform::pick_files(&self.inbox, ui.ctx());
        }
        ui.label(RichText::new("You can also drop files on the window.").small().color(palette.secondary_text));

        let total = DICTIONARY_FILES.len();
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
        if let Some(e) = &self.storage_error {
            ui.label(RichText::new("Books cannot be saved in this browser, so they will not be kept.").small().color(palette.secondary_text));
            if self.debug {
                ui.label(RichText::new(e).small().color(palette.secondary_text));
            }
        }
        ui.add_space(8.0);

        let remaining = self.remaining_counts();
        let mut remove = None;
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
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add(egui::Button::new("✕").frame(false)).on_hover_text("Remove this book").clicked() {
                                    remove = Some(i);
                                }
                            });
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
                    ui.label(RichText::new(&book.saved.name).strong());
                    let waiting = !matches!(self.dictionary, DictionaryState::Ready { .. });
                    let text = if waiting { "Waiting for the dictionary…" } else { "Analyzing chapters…" };
                    ui.add(egui::ProgressBar::new(book.progress()).text(text));
                });
                ui.add_space(6.0);
            }
            for saved in self.restoring.iter().chain(&self.to_restore) {
                theme::card(palette).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(&saved.name).strong());
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
        if let Some(i) = remove {
            let book = self.books.remove(i);
            storage::remove_book(&self.inbox, ui.ctx(), book.id);
            self.generation += 1;
            if self.shown > i {
                self.shown -= 1;
            }
        }
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

        let chapters = &stats.chapters;
        let [words_log, characters_log, density_log] = self.log_scales;
        let metrics = [
            Metric { title: "New words to learn", hint: None, short: "Words", log: words_log, color: palette.teal, value: |s| s.filtered_words as f64, format: format_count, baseline: None },
            Metric { title: "Chapter length (characters)", hint: None, short: "Characters", log: characters_log, color: palette.purple, value: |s| s.characters as f64, format: format_count, baseline: None },
            Metric { title: "Vocabulary density", hint: Some("New words ÷ characters, relative to the first chapter"), short: "Density", log: density_log, color: palette.blue, value: |s| s.normalized_ratio, format: format_ratio, baseline: Some(1.0) },
        ];
        // Fill the window with the three cards, each adding its margins, border, title row and
        // the spacing after it; below a minimum height the page scrolls instead
        let card_overhead = 2.0 * 12.0 + 2.0 + 22.0 + 2.0 * ui.spacing().item_spacing.y;
        let header = ui.cursor().top() - top;
        let height = if stacked { MIN_PLOT_HEIGHT } else { ((viewport_height - header - 3.0 * card_overhead) / 3.0).max(MIN_PLOT_HEIGHT) };
        // While inspecting (touch screens), the finger is not a hover for egui, so the charts draw
        // the inspected chapter themselves, and a tooltip follows the finger
        let finger = if self.inspecting.active { ui.ctx().input(|i| i.pointer.latest_pos()) } else { None };
        let inspect = Inspect { finger, chapter: finger.and(self.inspected_chapter) };
        let mut toggled = None;
        let mut plot_rects = Vec::with_capacity(metrics.len());
        let mut inspected = None;
        for (i, metric) in metrics.iter().enumerate() {
            theme::card(palette).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let drawn = metric_plot(ui, metric, chapters, learned_x, height, palette, inspect);
                plot_rects.push(drawn.rect);
                inspected = inspected.or(drawn.inspected);
                if drawn.toggle {
                    toggled = Some(i);
                }
            });
        }
        self.plot_rects = plot_rects;
        self.inspected_chapter = inspected;
        if let (Some(finger), Some(text)) = (finger, inspected.and_then(|x| chapter_text(chapters, x))) {
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
    log: bool,
    color: Color32,
    value: fn(&ChapterStat) -> f64,
    format: fn(&f64) -> String,
    /// Dashed horizontal reference line
    baseline: Option<f64>,
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
fn metric_plot(
    ui: &mut Ui,
    m: &Metric,
    chapters: &[ChapterStat],
    learned_x: Option<f64>,
    height: f32,
    palette: Palette,
    inspect: Inspect,
) -> DrawnPlot {
    let values: Vec<f64> = chapters.iter().map(m.value).collect();
    let (min, max) = values.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    let avg = values.iter().sum::<f64>() / values.len().max(1) as f64;
    let f = m.format;
    let mut toggle = false;
    let wide = ui.available_width() > 640.0;
    let summary = RichText::new(format!("min {} · avg {} · max {}", f(&min), f(&avg), f(&max))).small().color(palette.secondary_text);
    ui.horizontal_wrapped(|ui| {
        let title = ui.label(RichText::new(m.title).strong());
        if let Some(hint) = m.hint {
            title.on_hover_text(hint);
        }
        let (scale, other) = if m.log { ("log scale", "linear") } else { ("linear scale", "log") };
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
    let to_y = |v: f64| if m.log { v.log10() } else { v };
    let points: Vec<[f64; 2]> = chapters
        .iter()
        .zip(&values)
        .filter(|&(_, &v)| !m.log || v > 0.0)
        .map(|(s, &v)| [s.chapter_index as f64, to_y(v)])
        .collect();

    let log = m.log;
    let x_ticks = (ui.available_width() as f64 / 70.0).clamp(3.0, 12.0);
    // The scale is part of the id, so switching it resets the y range
    let mut plot = Plot::new(("metric", m.short, m.log))
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
        .label_formatter(move |pos| hover_text(pos, chapters));
    plot = if m.log { plot.y_grid_spacer(log_grid) } else { plot.y_grid_spacer(|input| nice_grid(input, 5.0, 0.0)) };
    let shown = plot.show(ui, |plot_ui| {
        if let Some(b) = m.baseline {
            plot_ui.hline(HLine::new("Baseline", to_y(b)).color(palette.secondary_text).style(LineStyle::dashed_loose()).width(1.0));
        }
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
fn hover_text(pos: &HoverPosition<'_>, chapters: &[ChapterStat]) -> Option<String> {
    let x = match pos {
        HoverPosition::NearDataPoint { position, .. } | HoverPosition::Elsewhere { position } => position.x,
    };
    chapter_text(chapters, x)
}

/// Every metric of the chapter at `x` on the chapter axis.
fn chapter_text(chapters: &[ChapterStat], x: f64) -> Option<String> {
    let s = chapters.iter().find(|s| s.chapter_index as f64 == x.round())?;
    Some(format!(
        "{}: {}\nNew words: {}\nCharacters: {}\nNormalized ratio: {}",
        s.chapter_index,
        s.title,
        format_count(&(s.filtered_words as f64)),
        format_count(&(s.characters as f64)),
        format_ratio(&s.normalized_ratio),
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
        let ctx = ui.ctx().clone();
        self.handle_incoming(&ctx);
        self.apply_settings(&ctx);
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
        if ui.available_width() < STACK_BELOW_WIDTH {
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
        self.save_state(&ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, Event, Modifiers, PointerButton, RawInput, TouchDeviceId, TouchId, TouchPhase};

    /// Feeds one frame at `time` and returns whether the chart is being inspected.
    fn frame(ctx: &egui::Context, press: &mut LongPress, time: f64, events: Vec<Event>) -> bool {
        let input = RawInput { time: Some(time), events, ..Default::default() };
        let chart = [egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(300.0, 200.0))];
        let _ = ctx.run_ui(input, |ui| {
            ui.ctx().input(|i| press.update(i, &chart));
        });
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
