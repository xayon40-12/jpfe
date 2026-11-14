//! The reader: a chapter's text, where clicking a word shows its definitions, and clicking a kanji of that
//! word shows the kanji's readings and meanings.

use std::ops::{Range, RangeInclusive};

use anyhow::{Context, Result};
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, CursorIcon, FontId, Galley, Pos2, Rect, RichText, Sense, Ui};
use jpfe::dictionary::{Definitions, Kanjidic, DEFINITIONS_FILE, KANJI_FILE};
use jpfe::tokenizer::{is_cjk_kanji, JapaneseTokenizer, ParsedWord};

use crate::book::Book;
use crate::theme::Palette;

pub const DEFAULT_FONT_SIZE: f32 = 20.0;
pub const FONT_SIZES: RangeInclusive<f32> = 10.0..=48.0;
/// Line height, in font sizes: Japanese text needs more room than egui's default.
const LINE_HEIGHT: f32 = 1.75;
/// Widest line, in font sizes (about as many characters).
const MAX_LINE: f32 = 38.0;
/// Font size change for each notch of the mouse wheel with Shift.
const WHEEL_ZOOM: f32 = 1.06;
/// Entries shown at most for a word.
const MAX_ENTRIES: usize = 5;

/// Definitions and kanji the reader shows.
pub struct Lookup {
    pub words: Definitions,
    pub kanji: Kanjidic,
}

impl Lookup {
    /// From the contents of [`DEFINITIONS_FILE`] and [`KANJI_FILE`] (web: downloaded with the dictionary).
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub fn from_files(mut file: impl FnMut(&str) -> Option<Vec<u8>>) -> Result<Self> {
        let mut text = |name: &str| -> Result<String> {
            let bytes = file(name).with_context(|| format!("Missing dictionary file {name}"))?;
            Ok(String::from_utf8(bytes)?)
        };
        Ok(Self { words: Definitions::parse(text(DEFINITIONS_FILE)?)?, kanji: Kanjidic::parse(text(KANJI_FILE)?)? })
    }

    /// The files built into the desktop app.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn embedded() -> Self {
        Self { words: Definitions::embedded(), kanji: Kanjidic::embedded() }
    }
}

struct Paragraph {
    /// Bytes of the paragraph in the chapter text
    span: Range<usize>,
    /// Its words, as positions in `Reader::words`
    words: Range<usize>,
}

struct Word {
    parsed: ParsedWord,
    /// Characters of the word in its paragraph
    chars: Range<usize>,
}

/// The word whose definitions are shown, in a window opened at `anchor`.
struct WordPopup {
    word: usize,
    anchor: Pos2,
    /// Each window gets its own id, so a new one opens at its anchor rather than where the last one was
    id: u64,
}

struct KanjiPopup {
    kanji: char,
    anchor: Pos2,
    id: u64,
}

/// What the reader asks of the app.
#[derive(Default)]
pub struct ReaderResponse {
    /// Open this chapter
    pub chapter: Option<usize>,
}

pub struct Reader {
    pub book: u64,
    pub chapter: usize,
    /// The book's progress when the reader was opened: the reader opens there again once it changes
    pub opened_at: Option<usize>,
    book_name: String,
    title: String,
    chapters: usize,
    text: String,
    paragraphs: Vec<Paragraph>,
    words: Vec<Word>,
    /// Words are found once the dictionary is ready
    tokenized: bool,
    word_popup: Option<WordPopup>,
    kanji_popup: Option<KanjiPopup>,
    /// Where the word window was in the last frame, to open the kanji window next to it
    word_popup_rect: Option<Rect>,
    popups: u64,
}

impl Reader {
    pub fn new(book: &Book, chapter: usize) -> Self {
        let text = book.chapter_text(chapter).to_string();
        let mut at = 0;
        let paragraphs = text
            .split('\n')
            .map(|line| {
                let span = at..at + line.len();
                at = span.end + 1;
                Paragraph { span, words: 0..0 }
            })
            .collect();
        Self {
            book: book.id,
            chapter,
            opened_at: book.learned,
            book_name: book.name.clone(),
            title: book.chapter_label(chapter),
            chapters: book.chapters.len(),
            text,
            paragraphs,
            words: Vec::new(),
            tokenized: false,
            word_popup: None,
            kanji_popup: None,
            word_popup_rect: None,
            popups: 0,
        }
    }

    pub fn needs_words(&self) -> bool {
        !self.tokenized
    }

    /// Find the words of the chapter, and which paragraph each one is in.
    pub fn tokenize(&mut self, tokenizer: &mut JapaneseTokenizer) {
        self.tokenized = true;
        let mut parsed = match tokenizer.tokenize(&self.text) {
            Ok(words) => words,
            Err(e) => {
                log::warn!("Reader: {e:#}");
                return;
            }
        };
        parsed.sort_by_key(|w| w.span.start);
        let mut parsed = parsed.into_iter().peekable();
        for paragraph in &mut self.paragraphs {
            let start = self.words.len();
            let text = &self.text[paragraph.span.clone()];
            // Character positions of the words, counted as they come (they are in order)
            let (mut byte, mut char) = (0, 0);
            let mut char_at = |target: usize| {
                char += text[byte..target].chars().count();
                byte = target;
                char
            };
            while let Some(word) = parsed.next_if(|w| w.span.start < paragraph.span.end) {
                if word.span.start < paragraph.span.start || word.span.end > paragraph.span.end {
                    continue;
                }
                let (start, end) = (word.span.start - paragraph.span.start, word.span.end - paragraph.span.start);
                let chars = char_at(start)..char_at(end);
                self.words.push(Word { parsed: word, chars });
            }
            paragraph.words = start..self.words.len();
        }
    }

    /// Book name and chapter, for the top bar.
    pub fn book_name(&self) -> &str {
        &self.book_name
    }

    pub fn ui(&mut self, ui: &mut Ui, lookup: Option<&Lookup>, font_size: &mut f32, palette: Palette) -> ReaderResponse {
        let mut response = ReaderResponse::default();
        zoom(ui, font_size);
        let size = *font_size;
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) && self.kanji_popup.take().is_none() {
            self.word_popup = None;
        }
        if !self.tokenized {
            ui.label(RichText::new("Words can be looked up once the dictionary is ready.").small().color(palette.secondary_text));
        }

        let text_color = ui.visuals().text_color();
        let mut clicked: Option<Option<(usize, Pos2)>> = None;
        egui::ScrollArea::vertical()
            .id_salt(("reader", self.book, self.chapter))
            .auto_shrink(false)
            .show(ui, |ui| {
                // A centered column, no wider than a comfortable line
                let width = ui.available_width().min(size * MAX_LINE);
                let margin = (ui.available_width() - width) / 2.0;
                ui.horizontal(|ui| {
                    ui.add_space(margin);
                    ui.vertical(|ui| {
                        ui.set_width(width);
                        ui.add_space(size);
                        ui.label(RichText::new(&self.title).size(size * 1.4).strong());
                        ui.add_space(size * 0.8);
                        ui.spacing_mut().item_spacing.y = size * 0.3;
                        let selected = self.word_popup.as_ref().map(|p| p.word);
                        for paragraph in &self.paragraphs {
                            let text = &self.text[paragraph.span.clone()];
                            if text.trim().is_empty() {
                                ui.add_space(size * 0.5);
                                continue;
                            }
                            let words = &self.words[paragraph.words.clone()];
                            let selected = selected.and_then(|w| w.checked_sub(paragraph.words.start)).filter(|&w| w < words.len());
                            let colors = Highlight { text: text_color, hovered: palette.accent.gamma_multiply(0.12), selected: palette.accent.gamma_multiply(0.28) };
                            if let Some(click) = paragraph_ui(ui, text, words, size, colors, selected) {
                                clicked = Some(click.map(|(w, anchor)| (paragraph.words.start + w, anchor)));
                            }
                        }
                        ui.add_space(size);
                        if self.chapter + 1 < self.chapters {
                            ui.vertical_centered(|ui| {
                                let next = egui::Button::new(RichText::new("Next chapter ›").size(size * 0.8));
                                if ui.add(next).clicked() {
                                    response.chapter = Some(self.chapter + 1);
                                }
                            });
                        }
                        ui.add_space(size * 2.0);
                    });
                });
            });
        match clicked {
            Some(Some((word, anchor))) => {
                self.popups += 1;
                self.word_popup = Some(WordPopup { word, anchor, id: self.popups });
                self.kanji_popup = None;
            }
            // A click beside the words closes the windows
            Some(None) => {
                self.word_popup = None;
                self.kanji_popup = None;
            }
            None => {}
        }
        self.popups_ui(ui.ctx(), lookup, size, palette);
        response
    }

    fn popups_ui(&mut self, ctx: &egui::Context, lookup: Option<&Lookup>, size: f32, palette: Palette) {
        let body = (size * 0.85).clamp(13.0, 20.0);
        let max_width = (ctx.content_rect().width() - 24.0).max(200.0);
        let max_height = ctx.content_rect().height() * 0.7;
        if let Some(popup) = &self.word_popup {
            let word = &self.words[popup.word];
            let surface = &self.text[word.parsed.span.clone()];
            let mut open = true;
            let mut kanji = None;
            let shown = egui::Window::new("Dictionary")
                .id(egui::Id::unique(("word-popup", popup.id)))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_pos(popup.anchor)
                .default_width((body * 20.0).min(max_width))
                .max_width(max_width)
                .show(ctx, |ui| {
                    // Scrolls when long, and is no taller than its content otherwise (egui's own window
                    // scrolling always takes the maximum height)
                    egui::ScrollArea::vertical().max_height(max_height).auto_shrink([false, true]).show(ui, |ui| {
                        kanji = word_ui(ui, &word.parsed, surface, lookup, body, palette);
                    });
                });
            self.word_popup_rect = shown.map(|s| s.response.rect);
            if !open {
                self.word_popup = None;
                self.kanji_popup = None;
            }
            if let Some(kanji) = kanji {
                self.popups += 1;
                let anchor = self.word_popup_rect.map_or(Pos2::ZERO, |r| r.right_top() + egui::vec2(8.0, 0.0));
                self.kanji_popup = Some(KanjiPopup { kanji, anchor, id: self.popups });
            }
        }
        if let (Some(popup), Some(lookup)) = (&self.kanji_popup, lookup) {
            let mut open = true;
            egui::Window::new("Kanji")
                .id(egui::Id::unique(("kanji-popup", popup.id)))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_pos(popup.anchor)
                .default_width((body * 16.0).min(max_width))
                .max_width(max_width)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(max_height)
                        .auto_shrink([false, true])
                        .show(ui, |ui| kanji_ui(ui, popup.kanji, &lookup.kanji, body, palette));
                });
            if !open {
                self.kanji_popup = None;
            }
        }
    }
}

/// Pinching, Cmd/Ctrl + scrolling and Shift + scrolling change the font size.
fn zoom(ui: &Ui, font_size: &mut f32) {
    let (zoom, notches) = ui.input(|i| {
        let notches: f32 = i
            .events
            .iter()
            .filter_map(|e| match e {
                // macOS turns Shift + wheel into horizontal scrolling, and egui does elsewhere
                egui::Event::MouseWheel { unit, delta, modifiers, .. } if modifiers.shift && !modifiers.command => {
                    let amount = delta.x + delta.y;
                    Some(match unit {
                        egui::MouseWheelUnit::Line => amount,
                        // Browsers report about 100 points per notch of a mouse wheel
                        egui::MouseWheelUnit::Point => amount / 100.0,
                        egui::MouseWheelUnit::Page => amount * 10.0,
                    })
                }
                _ => None,
            })
            .sum();
        (i.zoom_delta(), notches)
    });
    if zoom != 1.0 || notches != 0.0 {
        *font_size = (*font_size * zoom * WHEEL_ZOOM.powf(notches)).clamp(*FONT_SIZES.start(), *FONT_SIZES.end());
    }
}

#[derive(Clone, Copy)]
struct Highlight {
    text: Color32,
    hovered: Color32,
    selected: Color32,
}

/// Draws a paragraph whose words can be clicked. Returns `Some` when it was clicked: the word clicked
/// (its position in `words`) and where to open its window, or `None` beside the words.
fn paragraph_ui(ui: &mut Ui, text: &str, words: &[Word], size: f32, colors: Highlight, selected: Option<usize>) -> Option<Option<(usize, Pos2)>> {
    let format = TextFormat { font_id: FontId::proportional(size), color: colors.text, line_height: Some(size * LINE_HEIGHT), ..Default::default() };
    let mut job = LayoutJob::single_section(text.to_string(), format);
    job.wrap.max_width = ui.available_width();
    let galley = ui.painter().layout_job(job);
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::click());
    let word_at = |pos: Pos2| char_at(&galley, pos - rect.min).and_then(|c| words.iter().position(|w| w.chars.contains(&c)));
    let hovered = response.hover_pos().and_then(word_at);
    if hovered.is_some() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        for (word, color) in [(hovered, colors.hovered), (selected, colors.selected)] {
            if let Some(word) = word {
                for r in word_rects(&galley, words[word].chars.clone()) {
                    painter.rect_filled(r.translate(rect.min.to_vec2()).expand2(egui::vec2(1.0, -size * 0.12)), 3.0, color);
                }
            }
        }
        painter.galley(rect.min, galley.clone(), colors.text);
    }
    response.clicked().then(|| {
        let word = response.interact_pointer_pos().and_then(word_at)?;
        let first_row = word_rects(&galley, words[word].chars.clone()).into_iter().next()?;
        Some((word, rect.min + first_row.left_bottom().to_vec2() + egui::vec2(0.0, 4.0)))
    })
}

/// The character under `pos` (relative to the galley), if any. The galley has no line breaks.
fn char_at(galley: &Galley, pos: egui::Vec2) -> Option<usize> {
    let mut index = 0;
    for row in &galley.rows {
        let r = row.rect();
        if (r.min.y..r.max.y).contains(&pos.y) {
            return row.row.glyphs.iter().position(|g| (row.pos.x + g.pos.x..row.pos.x + g.pos.x + g.advance_width).contains(&pos.x)).map(|i| index + i);
        }
        index += row.row.glyphs.len() + usize::from(row.ends_with_newline);
    }
    None
}

/// Rectangles covering the characters `chars` on each row they are on (relative to the galley).
fn word_rects(galley: &Galley, chars: Range<usize>) -> Vec<Rect> {
    let mut rects = Vec::new();
    let mut index = 0;
    for row in &galley.rows {
        let glyphs = &row.row.glyphs;
        let (start, end) = (chars.start.max(index), chars.end.min(index + glyphs.len()));
        if start < end {
            let (first, last) = (&glyphs[start - index], &glyphs[end - 1 - index]);
            let r = row.rect();
            rects.push(Rect::from_x_y_ranges(row.pos.x + first.pos.x..=row.pos.x + last.pos.x + last.advance_width, r.y_range()));
        }
        index += glyphs.len() + usize::from(row.ends_with_newline);
    }
    rects
}

/// The word, its reading and its definitions; returns a kanji of the word that was clicked.
fn word_ui(ui: &mut Ui, word: &ParsedWord, surface: &str, lookup: Option<&Lookup>, body: f32, palette: Palette) -> Option<char> {
    let mut clicked = None;
    let secondary = |text: &str, size: f32| RichText::new(text).size(size).color(palette.secondary_text);
    ui.spacing_mut().item_spacing.y = body * 0.35;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for c in word.base_word.chars() {
            let known = is_cjk_kanji(c) && lookup.is_some_and(|l| l.kanji.lookup(c).is_some());
            let text = RichText::new(c.to_string()).size(body * 1.5);
            if known {
                let label = ui.add(egui::Label::new(text.color(palette.accent)).sense(Sense::click()));
                if label.hovered() {
                    ui.painter().hline(label.rect.x_range(), label.rect.bottom() - 1.0, egui::Stroke::new(1.0, palette.accent));
                }
                if label.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Show this kanji").clicked() {
                    clicked = Some(c);
                }
            } else {
                ui.label(text);
            }
        }
    });
    let entries = lookup.map(|l| l.words.lookup(&word.base_word)).unwrap_or_default();
    if word.pronunciation != word.base_word {
        ui.label(RichText::new(&word.pronunciation).size(body * 1.1));
    }
    if surface != word.base_word {
        ui.label(secondary(&format!("Written {surface} in the text"), body * 0.8));
    }
    let Some(lookup) = lookup else {
        ui.label(secondary("The dictionary is still loading.", body * 0.85));
        return clicked;
    };
    if entries.is_empty() {
        ui.label(secondary("Not in the dictionary.", body * 0.85));
    }
    let several = entries.len() > 1;
    for (i, entry) in entries.iter().take(MAX_ENTRIES).enumerate() {
        ui.separator();
        // Other spellings and readings, when they say more than the word above
        let spellings: Vec<&str> = entry.spellings.iter().copied().filter(|s| *s != word.base_word).collect();
        let readings: Vec<&str> = entry.readings.iter().copied().filter(|r| *r != word.pronunciation && *r != word.base_word).collect();
        if several || !spellings.is_empty() || !readings.is_empty() {
            let mut header = entry.spellings.first().or(entry.readings.first()).copied().unwrap_or_default().to_string();
            if !entry.spellings.is_empty() && !entry.readings.is_empty() {
                header = format!("{header}【{}】", entry.readings.join("・"));
            }
            ui.label(RichText::new(header).size(body).strong());
            if entry.spellings.len() > 1 {
                ui.label(secondary(&format!("Also written {}", entry.spellings[1..].join("・")), body * 0.8));
            }
        }
        let numbered = entry.senses.len() > 1;
        let mut previous_tags = None;
        for (n, sense) in entry.senses.iter().enumerate() {
            // Senses often share their tags: shown again only when they change
            if !sense.tags.is_empty() && previous_tags != Some(&sense.tags) {
                let tags: Vec<&str> = sense.tags.iter().map(|t| short_tag(t).unwrap_or_else(|| lookup.words.tag(t))).collect();
                ui.label(secondary(&tags.join(", "), body * 0.75).italics());
            }
            previous_tags = Some(&sense.tags);
            let text = if numbered { format!("{}. {}", n + 1, sense.glosses) } else { sense.glosses.to_string() };
            ui.label(RichText::new(text).size(body));
        }
        if i + 1 == MAX_ENTRIES && entries.len() > MAX_ENTRIES {
            ui.label(secondary(&format!("{} more entries", entries.len() - MAX_ENTRIES), body * 0.8));
        }
    }
    clicked
}

/// A short name for the most common JMdict tags (JMdict's own descriptions are long).
fn short_tag(code: &str) -> Option<&'static str> {
    Some(match code {
        "n" => "noun",
        "pn" => "pronoun",
        "adj-i" | "adj-ix" => "i-adjective",
        "adj-na" => "na-adjective",
        "adj-no" => "takes の",
        "adj-pn" | "adj-f" => "pre-noun adjective",
        "adj-t" => "taru adjective",
        "adv" => "adverb",
        "adv-to" => "adverb taking と",
        "aux" => "auxiliary",
        "aux-v" => "auxiliary verb",
        "aux-adj" => "auxiliary adjective",
        "conj" => "conjunction",
        "cop" => "copula",
        "ctr" => "counter",
        "exp" => "expression",
        "int" => "interjection",
        "num" => "number",
        "pref" => "prefix",
        "suf" => "suffix",
        "n-suf" => "noun suffix",
        "n-pref" => "noun prefix",
        "prt" => "particle",
        "v1" | "v1-s" => "ichidan verb",
        "vk" => "kuru verb",
        "vs" | "vs-i" => "suru verb",
        "vs-s" => "suru verb (special)",
        "vz" => "zuru verb",
        "vt" => "transitive",
        "vi" => "intransitive",
        "uk" => "usually in kana",
        "on-mim" => "onomatopoeia",
        "yoji" => "four-character idiom",
        "id" => "idiom",
        "col" => "colloquial",
        "sl" => "slang",
        "hon" => "honorific",
        "hum" => "humble",
        "pol" => "polite",
        "fam" => "familiar",
        "male" => "male speech",
        "fem" => "female speech",
        "arch" => "archaic",
        "obs" => "obsolete",
        "abbr" => "abbreviation",
        "derog" => "derogatory",
        "vulg" => "vulgar",
        "joc" => "jocular",
        "chn" => "children's language",
        code if code.starts_with("v5") => "godan verb",
        _ => return None,
    })
}

/// The kanji, its readings and its meanings.
fn kanji_ui(ui: &mut Ui, kanji: char, kanjidic: &Kanjidic, body: f32, palette: Palette) {
    let Some(k) = kanjidic.lookup(kanji) else {
        ui.label(RichText::new(kanji.to_string()).size(body * 3.0));
        ui.label(RichText::new("Not in the kanji dictionary.").size(body * 0.85).color(palette.secondary_text));
        return;
    };
    ui.spacing_mut().item_spacing.y = body * 0.35;
    ui.label(RichText::new(k.literal).size(body * 3.0));
    let details: Vec<String> = [
        k.strokes.map(|s| format!("{s} strokes")),
        k.grade.map(|g| match g {
            1..=6 => format!("grade {g}"),
            8 => "secondary school".to_string(),
            _ => "name kanji".to_string(),
        }),
        k.jlpt.map(|l| format!("old JLPT {l}")),
        k.frequency.map(|f| format!("#{f} in newspapers")),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !details.is_empty() {
        ui.label(RichText::new(details.join(" · ")).size(body * 0.75).color(palette.secondary_text));
    }
    ui.separator();
    egui::Grid::new("kanji-readings").num_columns(2).spacing([body * 0.6, body * 0.35]).show(ui, |ui| {
        for (name, values, separator) in [("On", &k.on, "、"), ("Kun", &k.kun, "、"), ("Names", &k.nanori, "、"), ("Meanings", &k.meanings, "; ")] {
            if values.is_empty() {
                continue;
            }
            ui.label(RichText::new(name).size(body * 0.8).color(palette.secondary_text));
            ui.add(egui::Label::new(RichText::new(values.join(separator)).size(body)).wrap());
            ui.end_row();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{vec2, Event, Modifiers, MouseWheelUnit, RawInput, TouchPhase};

    /// The font size after one frame with `events`, from 20.
    fn zoomed(events: Vec<Event>) -> f32 {
        let ctx = egui::Context::default();
        let mut size = 20.0;
        let mut output = ctx.run_ui(RawInput { events, ..Default::default() }, |ui| zoom(ui, &mut size));
        // No renderer applies the font texture in tests
        output.textures_delta.clear();
        size
    }

    fn wheel(modifiers: Modifiers) -> Event {
        Event::MouseWheel { unit: MouseWheelUnit::Line, delta: vec2(0.0, 2.0), phase: TouchPhase::Move, source: egui::MouseWheelSource::Wheel, modifiers }
    }

    #[test]
    fn shift_wheel_and_pinch_change_the_font_size() {
        assert_eq!(zoomed(vec![wheel(Modifiers::NONE)]), 20.0);
        assert!((zoomed(vec![wheel(Modifiers::SHIFT)]) - 20.0 * WHEEL_ZOOM * WHEEL_ZOOM).abs() < 1e-3);
        assert!((zoomed(vec![Event::Zoom(1.5)]) - 30.0).abs() < 1e-3);
        assert_eq!(zoomed(vec![Event::Zoom(10.0)]), *FONT_SIZES.end());
    }
}
