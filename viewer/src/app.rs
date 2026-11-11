use std::collections::HashMap;

use egui::{Color32, RichText, Ui};
use egui_plot::{GridInput, GridMark, HLine, HoverPosition, Line, LineStyle, Plot, Points, VLine};
use jpfe::chart::{format_count, format_ratio, log_ticks};
use jpfe::models::ChapterStat;
use jpfe::tokenizer::JapaneseTokenizer;
use web_time::{Duration, Instant};

use crate::book::{Book, BookStats, PendingBook};
use crate::platform::{self, Inbox, Incoming};
use crate::theme::{self, Palette};

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
        match book.step(tokenizer, Instant::now() + WORK_PER_FRAME) {
            Ok(false) => {}
            Ok(true) => {
                let book = self.pending.remove(0).finish();
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

    /// Recompute the shown book's statistics if the books or progress changed.
    fn refresh_stats(&mut self) {
        if self.books.is_empty() {
            self.stats = None;
            return;
        }
        self.shown = self.shown.min(self.books.len() - 1);
        let key = StatsKey {
            generation: self.generation,
            shown: self.shown,
            learned: self.books.iter().map(|b| b.learned).collect(),
        };
        if self.stats.as_ref().is_none_or(|(k, _)| *k != key) {
            self.stats = Some((key, Book::stats(&self.books, self.shown)));
        }
    }

    fn sidebar(&mut self, ui: &mut Ui, palette: Palette) {
        ui.add_space(4.0);
        ui.label(RichText::new("Books").heading());
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

        let mut remove = None;
        egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
            for (i, book) in self.books.iter_mut().enumerate() {
                let selected = i == self.shown;
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
        });
        if let Some(i) = remove {
            self.books.remove(i);
            self.generation += 1;
            if self.shown > i {
                self.shown -= 1;
            }
        }
    }

    fn central(&mut self, ui: &mut Ui, palette: Palette) {
        if self.books.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() / 3.0);
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
        ui.horizontal(|ui| {
            ui.label(RichText::new("Statistics for").color(palette.secondary_text));
            egui::ComboBox::from_id_salt("shown")
                .width(320.0)
                .truncate()
                .selected_text(&names[shown])
                .show_ui(ui, |ui| {
                    for (i, name) in names.iter().enumerate() {
                        ui.selectable_value(&mut shown, i, name);
                    }
                });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                download = ui.add(theme::primary_button("Download word lists (.zip)", palette)).clicked();
            });
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
        let metrics = [
            Metric { title: "New vocabulary density, relative to the first chapter", short: "Ratio", log: true, color: palette.blue, value: |s| s.normalized_ratio, format: format_ratio, baseline: Some(1.0) },
            Metric { title: "New words to learn", short: "Words", log: true, color: palette.teal, value: |s| s.filtered_words as f64, format: format_count, baseline: None },
            Metric { title: "Chapter length (characters)", short: "Characters", log: false, color: palette.purple, value: |s| s.characters as f64, format: format_count, baseline: None },
        ];
        // Each card adds its margins, border, title row and the spacing after it
        let card_overhead = 2.0 * 12.0 + 2.0 + 22.0 + 2.0 * ui.spacing().item_spacing.y;
        let height = ((ui.available_height() - 3.0 * card_overhead) / 3.0).max(120.0);
        for (i, metric) in metrics.iter().enumerate() {
            theme::card(palette).show(ui, |ui| {
                ui.set_width(ui.available_width());
                metric_plot(ui, i, metric, chapters, learned_x, height, palette);
            });
        }
    }
}

struct Metric {
    title: &'static str,
    short: &'static str,
    log: bool,
    color: Color32,
    value: fn(&ChapterStat) -> f64,
    format: fn(&f64) -> String,
    /// Dashed horizontal reference line
    baseline: Option<f64>,
}

fn metric_plot(ui: &mut Ui, i: usize, m: &Metric, chapters: &[ChapterStat], learned_x: Option<f64>, height: f32, palette: Palette) {
    let values: Vec<f64> = chapters.iter().map(m.value).collect();
    let (min, max) = values.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    let avg = values.iter().sum::<f64>() / values.len().max(1) as f64;
    let f = m.format;
    ui.horizontal(|ui| {
        ui.label(RichText::new(m.title).strong());
        ui.label(RichText::new(if m.log { "log scale" } else { "linear scale" }).small().color(palette.secondary_text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(format!("min {} · avg {} · max {}", f(&min), f(&avg), f(&max))).small().color(palette.secondary_text));
        });
    });

    // Log axes plot log10 of the values; non-positive values cannot be shown there
    let to_y = |v: f64| if m.log { v.log10() } else { v };
    let points: Vec<[f64; 2]> = chapters
        .iter()
        .zip(&values)
        .filter(|&(_, &v)| !m.log || v > 0.0)
        .map(|(s, &v)| [s.chapter_index as f64, to_y(v)])
        .collect();

    let link = egui::Id::unique("chapter-axis");
    let log = m.log;
    let mut plot = Plot::new(("metric", i))
        .height(height)
        .link_axis(link, [true, false])
        .link_cursor(link, [true, false])
        .allow_zoom([true, false])
        .allow_drag([true, false])
        .allow_scroll([true, false])
        .allow_boxed_zoom(false)
        .y_axis_min_width(48.0)
        .show_grid([false, true])
        .x_grid_spacer(|input| nice_grid(input, 12.0, 1.0))
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
        let palette = Palette::of(ui.visuals());

        egui::Panel::left("books")
            .resizable(true)
            .default_size(290.0)
            .frame(egui::Frame::new().fill(palette.sidebar).inner_margin(egui::Margin::same(14)))
            .show(ui, |ui| self.sidebar(ui, palette));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(palette.content).inner_margin(egui::Margin::same(18)))
            .show(ui, |ui| self.central(ui, palette));
        // After drawing, so the window shows this frame while the slow work runs
        self.do_work(&ctx);
    }
}
