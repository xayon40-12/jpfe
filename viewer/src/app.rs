use std::collections::HashMap;

use egui::{Color32, RichText, Ui};
use egui_plot::{GridInput, GridMark, HLine, HoverPosition, Line, LineStyle, Plot, Points, VLine};
use jpfe::chart::{format_count, format_ratio, log_ticks};
use jpfe::models::ChapterStat;
use jpfe::tokenizer::JapaneseTokenizer;
use web_time::{Duration, Instant};

use crate::book::{Book, BookStats, PendingBook, Settings};
use crate::platform::{self, Inbox, Incoming};
use crate::theme::{self, Palette};

/// Below this window width the books and the charts are stacked in one column (phones).
const STACK_BELOW_WIDTH: f32 = 760.0;
/// Below this the charts stop shrinking and the page scrolls.
const MIN_PLOT_HEIGHT: f32 = 170.0;

/// Tokenizing time per frame, so the window keeps redrawing while books load.
const WORK_PER_FRAME: Duration = Duration::from_millis(25);

enum DictionaryState {
    /// Web only: compressed files still to unpack, and the unpacked ones
    Loading { compressed: Vec<(String, Vec<u8>)>, files: HashMap<String, Vec<u8>>, received: usize },
    Ready(Box<JapaneseTokenizer>),
    Failed(String),
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
    /// For each book, its words not learned in the other books
    remaining: Option<(StatsKey, Vec<usize>)>,
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
        platform::load_assets(&inbox, &cc.egui_ctx);
        let dictionary = match platform::embedded_tokenizer() {
            Some(Ok(tokenizer)) => DictionaryState::Ready(Box::new(tokenizer)),
            Some(Err(e)) => DictionaryState::Failed(e.to_string()),
            None => DictionaryState::Loading { compressed: Vec::new(), files: HashMap::new(), received: 0 },
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
            remaining: None,
        }
    }

    fn handle_incoming(&mut self, ctx: &egui::Context) {
        for message in self.inbox.drain() {
            match message {
                Incoming::File { name, bytes } => match PendingBook::parse(&name, bytes) {
                    Ok(book) => self.pending.push(book),
                    Err(e) => self.errors.push(format!("{e:#}")),
                },
                Incoming::DictionaryFile { name, bytes } => {
                    if let DictionaryState::Loading { compressed, received, .. } = &mut self.dictionary {
                        match bytes {
                            Ok(bytes) => {
                                compressed.push((name, bytes));
                                *received += 1;
                            }
                            Err(e) => self.dictionary = DictionaryState::Failed(format!("Downloading {name}: {e}")),
                        }
                    }
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
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for file in dropped {
            platform::read_dropped(&self.inbox, file, ctx);
        }
    }

    /// Unpack the dictionary and tokenize pending books, a little per frame.
    fn do_work(&mut self, ctx: &egui::Context) {
        #[cfg(target_arch = "wasm32")]
        if let DictionaryState::Loading { compressed, files, received } = &mut self.dictionary {
            if let Some((name, bytes)) = compressed.pop() {
                match platform::decompress(&bytes) {
                    Ok(data) => {
                        files.insert(name, data);
                    }
                    Err(e) => self.dictionary = DictionaryState::Failed(format!("Unpacking {name}: {e}")),
                }
                ctx.request_repaint();
                return;
            }
            if *received == jpfe::tokenizer::DICTIONARY_FILES.len() && files.len() == *received {
                let mut files = std::mem::take(files);
                self.dictionary = match jpfe::tokenizer::dictionary_from_files(|name| files.remove(name)) {
                    Ok(dictionary) => DictionaryState::Ready(Box::new(JapaneseTokenizer::with_dictionary(dictionary, true, false))),
                    Err(e) => DictionaryState::Failed(format!("{e:#}")),
                };
                ctx.request_repaint();
                return;
            }
        }

        let DictionaryState::Ready(tokenizer) = &mut self.dictionary else { return };
        let Some(book) = self.pending.first_mut() else { return };
        tokenizer.set_include_particles(self.settings.include_particles);
        match book.step(tokenizer, Instant::now() + WORK_PER_FRAME) {
            Ok(false) => {}
            Ok(true) => {
                let book = self.pending.remove(0).finish(&self.settings);
                self.books.push(book);
                self.generation += 1;
            }
            Err(e) => {
                let book = self.pending.remove(0);
                self.errors.push(format!("{}: {e:#}", book.name));
            }
        }
        ctx.request_repaint();
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

        match &self.dictionary {
            DictionaryState::Loading { files, received, .. } => {
                let total = jpfe::tokenizer::DICTIONARY_FILES.len();
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
            DictionaryState::Ready(_) => {}
        }
        ui.add_space(8.0);

        let remaining = self.remaining_counts();
        let mut remove = None;
        {
            let any_learned = self.books.iter().any(|b| b.learned.is_some());
            for (i, book) in self.books.iter_mut().enumerate() {
                let selected = i == self.shown;
                let others_learned = any_learned && remaining[i] < book.words.iter().map(Vec::len).sum();
                let mut frame = theme::card(palette);
                if selected {
                    frame = frame.stroke(egui::Stroke::new(1.5, palette.accent));
                }
                frame.show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let name = ui.add(egui::Label::new(RichText::new(&book.name).strong()).truncate().sense(egui::Sense::click()));
                        if name.clicked() {
                            self.shown = i;
                        }
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
                            ui.selectable_value(&mut book.learned, None, "Not started");
                            for pos in 0..book.chapters.len() {
                                let label = book.chapter_label(pos);
                                ui.selectable_value(&mut book.learned, Some(pos), label);
                            }
                        });
                });
                ui.add_space(6.0);
            }
            for book in &self.pending {
                theme::card(palette).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(&book.name).strong());
                    let waiting = !matches!(self.dictionary, DictionaryState::Ready(_));
                    let text = if waiting { "Waiting for the dictionary…" } else { "Analyzing chapters…" };
                    ui.add(egui::ProgressBar::new(book.progress()).text(text));
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
            self.books.remove(i);
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
                    for (i, name) in names.iter().enumerate() {
                        ui.selectable_value(&mut shown, i, name);
                    }
                });
            let button = theme::primary_button("Download word lists (.zip)", palette);
            download = if stacked {
                ui.add(button).clicked()
            } else {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.add(button).clicked()).inner
            };
        });
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
        let mut toggled = None;
        for (i, metric) in metrics.iter().enumerate() {
            theme::card(palette).show(ui, |ui| {
                ui.set_width(ui.available_width());
                if metric_plot(ui, metric, chapters, learned_x, height, palette) {
                    toggled = Some(i);
                }
            });
        }
        if let Some(i) = toggled {
            self.log_scales[i] = !self.log_scales[i];
        }
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

/// Draws one plot card; returns true when its scale label was clicked.
fn metric_plot(ui: &mut Ui, m: &Metric, chapters: &[ChapterStat], learned_x: Option<f64>, height: f32, palette: Palette) -> bool {
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
    plot.show(ui, |plot_ui| {
        if let Some(b) = m.baseline {
            plot_ui.hline(HLine::new("Baseline", to_y(b)).color(palette.secondary_text).style(LineStyle::dashed_loose()).width(1.0));
        }
        if let Some(x) = learned_x {
            plot_ui.vline(VLine::new("Learned up to here", x).color(palette.orange).width(2.0));
        }
        plot_ui.line(Line::new(m.short, points.clone()).color(m.color).width(1.8));
        plot_ui.points(Points::new(m.short, points).color(m.color).radius(if chapters.len() <= 60 { 3.0 } else { 1.8 }));
    });
    toggle
}

/// Every metric of the chapter under the cursor.
fn hover_text(pos: &HoverPosition<'_>, chapters: &[ChapterStat]) -> Option<String> {
    let x = match pos {
        HoverPosition::NearDataPoint { position, .. } | HoverPosition::Elsewhere { position } => position.x,
    };
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
        let palette = Palette::of(ui.visuals());

        let content = egui::Frame::new().fill(palette.content).inner_margin(egui::Margin::same(18));
        if ui.available_width() < STACK_BELOW_WIDTH {
            // Phones and narrow windows: the books, then the charts, in one scrolling column
            egui::CentralPanel::default().frame(content).show(ui, |ui| {
                let viewport_height = ui.available_height();
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
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
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.central(ui, palette, viewport_height, false));
            });
        }
        self.settings_window(&ctx, palette);
        // After drawing, so the window shows this frame while the slow work runs
        self.do_work(&ctx);
    }
}
