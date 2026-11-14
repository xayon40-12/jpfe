//! The reader: a chapter's text, where clicking a word shows its definitions, and clicking a kanji of that
//! word shows the kanji's readings and meanings.
//!
//! The text is set on a grid, one character per cell: in columns from top to bottom, right to left
//! (縦書き, the default), or in lines from left to right, top to bottom. egui only lays out horizontal text,
//! so the reader places every character itself.

use std::f32::consts::FRAC_PI_2;
use std::ops::{Range, RangeInclusive};

use anyhow::{Context, Result};
use egui::epaint::TextShape;
use egui::{pos2, vec2, Align2, Color32, CursorIcon, FontId, Painter, Pos2, Rect, RichText, Sense, Ui, Vec2};
use jpfe::dictionary::{Definitions, Kanjidic, DEFINITIONS_FILE, KANJI_FILE};
use jpfe::tokenizer::{is_cjk_kanji, JapaneseTokenizer, ParsedWord};

use crate::book::Book;
use crate::theme::Palette;

pub const DEFAULT_FONT_SIZE: f32 = 20.0;
pub const FONT_SIZES: RangeInclusive<f32> = 10.0..=48.0;
/// Vertical text (縦書き) unless the reader picks horizontal text.
pub const DEFAULT_VERTICAL: bool = true;
/// Distance from one line (or column) to the next, in font sizes.
const LINE_PITCH: f32 = 1.75;
/// Longest horizontal line, in characters.
const MAX_LINE: f32 = 38.0;
/// Margin around the text, in font sizes.
const MARGIN: f32 = 1.0;
/// Room after the text for the next chapter button, in font sizes: below horizontal text, or beside vertical
/// text (where the button, written horizontally, is wider than a column).
const END_SPACE: f32 = 4.0;
const END_SPACE_VERTICAL: f32 = 10.0;
/// Size of the chapter title and of PDF page numbers, in font sizes.
const TITLE_SCALE: f32 = 1.3;
const PAGE_NUMBER_SCALE: f32 = 0.5;
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
    /// Its characters as shown, one per cell (see [`full_width`])
    chars: Vec<char>,
    /// A page number of a PDF, shown small at the end of its line
    page_number: bool,
}

struct Word {
    parsed: ParsedWord,
    paragraph: usize,
    /// Characters of the word in its paragraph
    chars: Range<usize>,
}

/// The word whose definitions are shown, in a window whose `pivot` corner is at `anchor`.
struct WordPopup {
    word: usize,
    anchor: Pos2,
    pivot: Align2,
    /// Each window gets its own id, so a new one opens at its anchor rather than where the last one was
    id: u64,
}

struct KanjiPopup {
    kanji: char,
    anchor: Pos2,
    pivot: Align2,
    id: u64,
}

/// What the reader asks of the app.
#[derive(Default)]
pub struct ReaderResponse {
    /// Open this chapter
    pub chapter: Option<usize>,
}

/// What the text was laid out for; it is laid out again when any of it changes.
#[derive(Clone, Copy, PartialEq)]
struct LayoutKey {
    size: f32,
    vertical: bool,
    /// Room for the characters of a line (a column's height for vertical text)
    length: f32,
}

/// A line of horizontal text, or a column of vertical text.
struct Line {
    /// The paragraph it shows part of; `None` for the chapter title
    paragraph: Option<usize>,
    chars: Range<usize>,
    /// Cell of its first character (page numbers are set at the end of their line)
    first_cell: usize,
    scale: f32,
    /// Distance from the first line, and thickness of the line (its height, or a column's width)
    offset: f32,
    pitch: f32,
}

struct Layout {
    key: LayoutKey,
    lines: Vec<Line>,
    /// Lines of each paragraph, as positions in `lines`
    paragraph_lines: Vec<Range<usize>>,
    /// Thickness of all the lines
    extent: f32,
    /// Length of a full line of body text
    line_length: f32,
}

impl Layout {
    /// Where the first line starts: its top-left corner, or its top-right one for vertical text.
    fn origin(&self, content: Rect) -> Pos2 {
        let margin = self.key.size * MARGIN;
        if self.key.vertical {
            pos2(content.right() - margin, content.top() + margin)
        } else {
            // Horizontal text is centered
            pos2(content.left() + ((content.width() - self.line_length) / 2.0).max(margin), content.top() + margin)
        }
    }

    /// Size of everything to scroll through, for a visible area of `view`.
    fn content_size(&self, view: Vec2) -> Vec2 {
        let end = if self.key.vertical { END_SPACE_VERTICAL } else { END_SPACE };
        let thickness = self.extent + self.key.size * (2.0 * MARGIN + end);
        if self.key.vertical { vec2(thickness.max(view.x), view.y) } else { vec2(view.x, thickness.max(view.y)) }
    }

    fn cell(&self, line: &Line, cell: usize, origin: Pos2) -> Rect {
        let size = self.key.size * line.scale;
        if self.key.vertical {
            Rect::from_min_size(pos2(origin.x - line.offset - line.pitch, origin.y + cell as f32 * size), vec2(line.pitch, size))
        } else {
            Rect::from_min_size(pos2(origin.x + cell as f32 * size, origin.y + line.offset), vec2(size, line.pitch))
        }
    }

    /// The paragraph and character at `pos`.
    fn char_at(&self, pos: Pos2, origin: Pos2) -> Option<(usize, usize)> {
        let (across, along) = if self.key.vertical { (origin.x - pos.x, pos.y - origin.y) } else { (pos.y - origin.y, pos.x - origin.x) };
        let line = &self.lines[self.lines.partition_point(|l| l.offset + l.pitch <= across).min(self.lines.len().checked_sub(1)?)];
        if across < line.offset || along < 0.0 {
            return None;
        }
        let cell = (along / (self.key.size * line.scale)) as usize;
        let index = line.chars.start + cell.checked_sub(line.first_cell)?;
        (index < line.chars.end).then_some((line.paragraph?, index))
    }

    /// Lines that may show in `clip`.
    fn visible(&self, clip: Rect, origin: Pos2) -> Range<usize> {
        let (from, to) = if self.key.vertical { (origin.x - clip.right(), origin.x - clip.left()) } else { (clip.top() - origin.y, clip.bottom() - origin.y) };
        let start = self.lines.partition_point(|l| l.offset + l.pitch < from);
        start..self.lines.partition_point(|l| l.offset <= to).max(start)
    }

    /// Rectangles covering the characters `chars` of a paragraph, one for each line they are on.
    fn rects(&self, paragraph: usize, chars: &Range<usize>, origin: Pos2) -> Vec<Rect> {
        let lines = self.paragraph_lines.get(paragraph).cloned().unwrap_or_default();
        self.lines[lines]
            .iter()
            .filter_map(|line| {
                let (start, end) = (chars.start.max(line.chars.start), chars.end.min(line.chars.end));
                (start < end).then(|| {
                    let cell = |i: usize| self.cell(line, line.first_cell + i - line.chars.start, origin);
                    let rect = cell(start).union(cell(end - 1));
                    // The glyphs' band, not the space between lines
                    let gap = (line.pitch - self.key.size * line.scale * 1.3) / 2.0;
                    if self.key.vertical { rect.shrink2(vec2(gap, 0.0)) } else { rect.shrink2(vec2(0.0, gap)) }
                })
            })
            .collect()
    }
}

/// Characters that may not start a line: they hang at the end of the previous one (禁則処理).
fn no_line_start(c: char) -> bool {
    "、。，．」』）】〕〉》］｝！？ー…‥・：；ゝゞ々ぁぃぅぇぉっゃゅょゎゕゖァィゥェォッャュョヮヵヶ".contains(c)
}

/// Characters that may not end a line: they move to the next one, with what they open.
fn no_line_end(c: char) -> bool {
    "「『（【〔〈《［｛".contains(c)
}

fn is_small_kana(c: char) -> bool {
    "ぁぃぅぇぉっゃゅょゎゕゖァィゥェォッャュョヮヵヶ".contains(c)
}

/// Half-width letters, digits and signs as full-width characters, so every character fills one cell of
/// the grid (the text keeps one character for each, so word positions are unchanged).
fn full_width(c: char) -> char {
    match c {
        ' ' => '　',
        '!'..='~' => char::from_u32(c as u32 + 0xFEE0).unwrap_or(c),
        _ => c,
    }
}

/// Cut a paragraph into lines of `cells` characters, keeping punctuation off the start of a line.
fn wrap(chars: &[char], cells: usize) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut end = (start + cells).min(chars.len());
        if end < chars.len() {
            // Punctuation and closing brackets hang past the end of the line (two at most)
            let mut hung = 0;
            while end < chars.len() && hung < 2 && no_line_start(chars[end]) {
                end += 1;
                hung += 1;
            }
            if hung == 0 {
                while end > start + 1 && no_line_end(chars[end - 1]) {
                    end -= 1;
                }
            }
        }
        lines.push(start..end);
        start = end;
    }
    if lines.is_empty() {
        lines.push(0..0);
    }
    lines
}

/// How a character is drawn in vertical text.
enum VerticalForm {
    Upright,
    /// Turned a quarter clockwise: long vowel marks, dashes, ellipses and brackets
    Rotated,
    /// Moved by this much (in font sizes) to the top right of its cell: commas, periods and small kana
    Shifted(f32, f32),
}

fn vertical_form(c: char) -> VerticalForm {
    if "ー－―—～〜…‥「」『』（）【】〔〕〈〉《》［］｛｝＜＞＝：；｜→←".contains(c) {
        VerticalForm::Rotated
    } else if "、。，．".contains(c) {
        VerticalForm::Shifted(0.55, -0.55)
    } else if is_small_kana(c) {
        VerticalForm::Shifted(0.1, -0.1)
    } else {
        VerticalForm::Upright
    }
}

/// Draws one character in the middle of its cell.
fn draw_char(painter: &Painter, c: char, cell: Rect, font: &FontId, color: Color32, vertical: bool) {
    let galley = painter.layout_no_wrap(c.to_string(), font.clone(), color);
    let size = galley.size();
    let mut center = cell.center();
    if vertical {
        match vertical_form(c) {
            VerticalForm::Rotated => {
                // Turned around its top-left corner, the glyph lies to the left of it
                let pos = pos2(center.x + size.y / 2.0, center.y - size.x / 2.0);
                painter.add(TextShape::new(pos, galley, color).with_angle(FRAC_PI_2));
                return;
            }
            VerticalForm::Shifted(x, y) => center += vec2(x, y) * font.size,
            VerticalForm::Upright => {}
        }
    }
    painter.galley(center - size / 2.0, galley, color);
}

pub struct Reader {
    pub book: u64,
    pub chapter: usize,
    /// The book's progress when the reader was opened: the reader opens there again once it changes
    pub opened_at: Option<usize>,
    book_name: String,
    title: Vec<char>,
    chapters: usize,
    text: String,
    paragraphs: Vec<Paragraph>,
    words: Vec<Word>,
    /// Words are found once the dictionary is ready
    tokenized: bool,
    layout: Option<Layout>,
    /// How far into the chapter the reader is (0 to 1), kept when the text is laid out again
    position: f32,
    /// Layout and visible size the scroll position was last set for
    placed_for: Option<(LayoutKey, Vec2)>,
    word_popup: Option<WordPopup>,
    kanji_popup: Option<KanjiPopup>,
    /// Where the word window was in the last frame, to open the kanji window next to it
    word_popup_rect: Option<Rect>,
    popups: u64,
}

impl Reader {
    pub fn new(book: &Book, chapter: usize) -> Self {
        let text = book.chapter_text(chapter).to_string();
        let pdf = book.file_name.to_lowercase().ends_with(".pdf");
        let mut at = 0;
        let paragraphs = text
            .split('\n')
            .map(|line| {
                let span = at..at + line.len();
                at = span.end + 1;
                let page_number = pdf && !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit());
                Paragraph { span, words: 0..0, chars: line.chars().map(full_width).collect(), page_number }
            })
            .collect();
        Self {
            book: book.id,
            chapter,
            opened_at: book.learned,
            book_name: book.name.clone(),
            title: book.chapter_label(chapter).chars().map(full_width).collect(),
            chapters: book.chapters.len(),
            text,
            paragraphs,
            words: Vec::new(),
            tokenized: false,
            layout: None,
            position: 0.0,
            placed_for: None,
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
        for (index, paragraph) in self.paragraphs.iter_mut().enumerate() {
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
                self.words.push(Word { parsed: word, paragraph: index, chars });
            }
            paragraph.words = start..self.words.len();
        }
    }

    /// Book name, for the top bar.
    pub fn book_name(&self) -> &str {
        &self.book_name
    }

    /// The word at a character of a paragraph.
    fn word_at(&self, paragraph: usize, char: usize) -> Option<usize> {
        let range = self.paragraphs[paragraph].words.clone();
        self.words[range.clone()].iter().position(|w| w.chars.contains(&char)).map(|i| range.start + i)
    }

    fn lay_out(&self, key: LayoutKey) -> Layout {
        let mut lines = Vec::new();
        let mut paragraph_lines = Vec::with_capacity(self.paragraphs.len());
        let mut offset = 0.0;
        let cells = |scale: f32| ((key.length / (key.size * scale)).floor() as usize).max(4);
        let mut push = |lines: &mut Vec<Line>, paragraph: Option<usize>, chars: Range<usize>, first_cell: usize, scale: f32| {
            let pitch = key.size * scale * LINE_PITCH;
            lines.push(Line { paragraph, chars, first_cell, scale, offset, pitch });
            offset += pitch;
        };
        for chars in wrap(&self.title, cells(TITLE_SCALE)) {
            push(&mut lines, None, chars, 0, TITLE_SCALE);
        }
        push(&mut lines, None, 0..0, 0, 0.5);
        for (index, paragraph) in self.paragraphs.iter().enumerate() {
            let first = lines.len();
            if paragraph.page_number {
                let len = paragraph.chars.len();
                push(&mut lines, Some(index), 0..len, cells(PAGE_NUMBER_SCALE).saturating_sub(len), PAGE_NUMBER_SCALE);
            } else {
                for chars in wrap(&paragraph.chars, cells(1.0)) {
                    push(&mut lines, Some(index), chars, 0, 1.0);
                }
            }
            paragraph_lines.push(first..lines.len());
        }
        Layout { key, lines, paragraph_lines, extent: offset, line_length: cells(1.0) as f32 * key.size }
    }

    pub fn ui(&mut self, ui: &mut Ui, lookup: Option<&Lookup>, font_size: &mut f32, vertical: bool, palette: Palette) -> ReaderResponse {
        let mut response = ReaderResponse::default();
        zoom(ui, font_size);
        let size = *font_size;
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) && self.kanji_popup.take().is_none() {
            self.word_popup = None;
        }
        if !self.tokenized {
            ui.label(RichText::new("Words can be looked up once the dictionary is ready.").small().color(palette.secondary_text));
        }

        let view = ui.available_size();
        let margin = size * MARGIN;
        // A cell of room for punctuation hanging past the end of a line
        let length = if vertical { view.y - 2.0 * margin - size } else { (view.x - 2.0 * margin - size).min(size * MAX_LINE) };
        let key = LayoutKey { size, vertical, length: length.max(size * 4.0) };
        let mut layout = match self.layout.take() {
            Some(layout) if layout.key == key => layout,
            _ => self.lay_out(key),
        };
        let content = layout.content_size(view);
        let scrollable = if vertical { content.x - view.x } else { content.y - view.y }.max(0.0);
        let mut area = if vertical { egui::ScrollArea::horizontal() } else { egui::ScrollArea::vertical() }
            .id_salt(("reader", self.book, self.chapter, vertical))
            .auto_shrink(false);
        // Keep the place in the chapter when the text is laid out again (vertical text starts on the right)
        if self.placed_for != Some((key, view)) {
            self.placed_for = Some((key, view));
            area = if vertical {
                area.horizontal_scroll_offset(scrollable * (1.0 - self.position))
            } else {
                area.vertical_scroll_offset(scrollable * self.position)
            };
        }
        // The mouse wheel moves through vertical text: down goes on to the next columns, on the left
        let view_rect = Rect::from_min_size(ui.cursor().min, view);
        if vertical && ui.rect_contains_pointer(view_rect) {
            ui.ctx().input_mut(|i| {
                let delta = i.smooth_scroll_delta;
                i.smooth_scroll_delta = vec2(delta.x - delta.y, 0.0);
            });
        }

        let colors = (ui.visuals().text_color(), palette.secondary_text, palette.accent.gamma_multiply(0.12), palette.accent.gamma_multiply(0.28));
        let selected = self.word_popup.as_ref().map(|p| p.word);
        let mut clicked: Option<Option<(usize, Pos2, Align2)>> = None;
        let shown = area.show(ui, |ui| {
            let (rect, background) = ui.allocate_exact_size(content, Sense::click());
            let origin = layout.origin(rect);
            let word_at = |pos: Pos2| layout.char_at(pos, origin).and_then(|(p, c)| self.word_at(p, c));
            let hovered = background.hover_pos().and_then(word_at);
            if hovered.is_some() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            let painter = ui.painter();
            let (text, secondary, hover_fill, selected_fill) = colors;
            for (word, fill) in [(hovered, hover_fill), (selected, selected_fill)] {
                if let Some(word) = word.and_then(|w| self.words.get(w)) {
                    for r in layout.rects(word.paragraph, &word.chars, origin) {
                        painter.rect_filled(r, 3.0, fill);
                    }
                }
            }
            for line in &layout.lines[layout.visible(ui.clip_rect(), origin)] {
                let (chars, color) = match line.paragraph {
                    None => (&self.title, text),
                    Some(p) => (&self.paragraphs[p].chars, if self.paragraphs[p].page_number { secondary } else { text }),
                };
                let font = FontId::proportional(size * line.scale);
                for (i, &c) in chars[line.chars.clone()].iter().enumerate() {
                    if c != '　' {
                        draw_char(painter, c, layout.cell(line, line.first_cell + i, origin), &font, color, vertical);
                    }
                }
            }
            if self.chapter + 1 < self.chapters {
                // After the text: below it, or on its left for vertical text
                let (area, align) = if vertical {
                    let right = origin.x - layout.extent - size * 0.5;
                    (Rect::from_min_max(pos2(right - size * (END_SPACE_VERTICAL - 1.0), origin.y), pos2(right, origin.y + size * 2.0)), egui::Align::Max)
                } else {
                    let top = origin.y + layout.extent + size;
                    (Rect::from_min_size(pos2(origin.x, top), vec2(layout.line_length, size * 2.0)), egui::Align::Center)
                };
                let builder = egui::UiBuilder::new().max_rect(area).layout(egui::Layout::top_down(align));
                ui.scope_builder(builder, |ui| {
                    if ui.add(egui::Button::new(RichText::new("Next chapter ›").size(size * 0.8))).clicked() {
                        response.chapter = Some(self.chapter + 1);
                    }
                });
            }
            if background.clicked() {
                // The window opens beside the word: below it, or on its left for vertical text
                clicked = Some(background.interact_pointer_pos().and_then(word_at).and_then(|w| {
                    let word = &self.words[w];
                    let first = *layout.rects(word.paragraph, &word.chars, origin).first()?;
                    let width = popup_width(ui.ctx(), size, WORD_POPUP_WIDTH);
                    Some(if vertical && first.left() - 6.0 - width >= ui.ctx().content_rect().left() {
                        (w, first.left_top() - vec2(6.0, 0.0), Align2::RIGHT_TOP)
                    } else {
                        (w, first.left_bottom() + vec2(0.0, 4.0), Align2::LEFT_TOP)
                    })
                }));
            }
        });
        let offset = shown.state.clamped_offset();
        if scrollable > 0.0 {
            self.position = if vertical { 1.0 - offset.x / scrollable } else { offset.y / scrollable }.clamp(0.0, 1.0);
        }
        layout.key = key;
        self.layout = Some(layout);

        match clicked {
            Some(Some((word, anchor, pivot))) => {
                self.popups += 1;
                self.word_popup = Some(WordPopup { word, anchor, pivot, id: self.popups });
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
        let body = popup_body(size);
        let max_width = (ctx.content_rect().width() - 24.0).max(200.0);
        let max_height = ctx.content_rect().height() * 0.7;
        if let Some(popup) = &self.word_popup {
            let word = &self.words[popup.word];
            let surface = &self.text[word.parsed.span.clone()];
            let side = popup.pivot;
            let mut open = true;
            let mut kanji = None;
            let shown = egui::Window::new("Dictionary")
                .id(egui::Id::unique(("word-popup", popup.id)))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .pivot(popup.pivot)
                .default_pos(popup.anchor)
                .default_width(popup_width(ctx, size, WORD_POPUP_WIDTH))
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
            if let (Some(kanji), Some(rect)) = (kanji, self.word_popup_rect) {
                self.popups += 1;
                // Next to the word window, on the same side of it as it is of the word, or below it when
                // there is no room there (phones)
                let width = popup_width(ctx, size, KANJI_POPUP_WIDTH);
                let screen = ctx.content_rect();
                let (anchor, pivot) = if side == Align2::RIGHT_TOP && rect.left() - 8.0 - width >= screen.left() {
                    (rect.left_top() - vec2(8.0, 0.0), Align2::RIGHT_TOP)
                } else if side == Align2::LEFT_TOP && rect.right() + 8.0 + width <= screen.right() {
                    (rect.right_top() + vec2(8.0, 0.0), Align2::LEFT_TOP)
                } else {
                    (rect.left_bottom() + vec2(0.0, 8.0), Align2::LEFT_TOP)
                };
                self.kanji_popup = Some(KanjiPopup { kanji, anchor, pivot, id: self.popups });
            }
        }
        if let (Some(popup), Some(lookup)) = (&self.kanji_popup, lookup) {
            let mut open = true;
            egui::Window::new("Kanji")
                .id(egui::Id::unique(("kanji-popup", popup.id)))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .pivot(popup.pivot)
                .default_pos(popup.anchor)
                .default_width(popup_width(ctx, size, KANJI_POPUP_WIDTH))
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

/// Width of the word and kanji windows, in their font sizes.
const WORD_POPUP_WIDTH: f32 = 20.0;
const KANJI_POPUP_WIDTH: f32 = 16.0;

/// Font size of the word and kanji windows, for a reader font of `size`.
fn popup_body(size: f32) -> f32 {
    (size * 0.85).clamp(13.0, 20.0)
}

/// Width of a word or kanji window (`width` in its font sizes), no wider than the screen.
fn popup_width(ctx: &egui::Context, size: f32, width: f32) -> f32 {
    (popup_body(size) * width).min((ctx.content_rect().width() - 24.0).max(200.0))
}

/// Pinching, Cmd/Ctrl + scrolling and Shift + scrolling change the font size.
fn zoom(ui: &Ui, font_size: &mut f32) {
    let (zoom, notches) = ui.ctx().input_mut(|i| {
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
        // Shift + wheel zooms instead of scrolling sideways
        if i.modifiers.shift {
            i.smooth_scroll_delta = Vec2::ZERO;
        }
        (i.zoom_delta(), notches)
    });
    if zoom != 1.0 || notches != 0.0 {
        *font_size = (*font_size * zoom * WHEEL_ZOOM.powf(notches)).clamp(*FONT_SIZES.start(), *FONT_SIZES.end());
    }
}

/// Text in several colors, wrapped to the width available.
fn colored(ui: &Ui, parts: &[(&str, Color32)], size: f32, italics: bool) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    for &(text, color) in parts {
        job.append(text, 0.0, egui::TextFormat { font_id: FontId::proportional(size), color, italics, ..Default::default() });
    }
    job.wrap.max_width = ui.available_width();
    job
}

/// `readings` in the reading color, between 【】 and separated by ・.
fn readings_parts<'a>(readings: &[&'a str], palette: Palette) -> Vec<(&'a str, Color32)> {
    let mut parts = vec![("【", palette.secondary_text)];
    for (i, &reading) in readings.iter().enumerate() {
        if i > 0 {
            parts.push(("・", palette.secondary_text));
        }
        parts.push((reading, palette.reading));
    }
    parts.push(("】", palette.secondary_text));
    parts
}

/// The word with its readings, then its definitions; returns a kanji of the word that was clicked.
fn word_ui(ui: &mut Ui, word: &ParsedWord, surface: &str, lookup: Option<&Lookup>, body: f32, palette: Palette) -> Option<char> {
    let mut clicked = None;
    let text_color = ui.visuals().text_color();
    let note = |ui: &mut Ui, text: &str| ui.label(RichText::new(text).size(body * 0.8).color(palette.secondary_text));
    ui.spacing_mut().item_spacing.y = body * 0.35;

    let mut entries = lookup.map(|l| l.words.lookup(&word.base_word)).unwrap_or_default();
    // The entry read as in the text first: its readings go on the first line, with the word
    if let Some(i) = entries.iter().position(|e| e.readings.contains(&word.pronunciation.as_str())) {
        entries[..=i].rotate_right(1);
    }
    let mut readings: Vec<&str> = entries.first().map(|e| e.readings.clone()).unwrap_or_default();
    // The reading in the text first; none that only repeats a word written in kana (の【の】)
    if word.pronunciation != word.base_word {
        readings.retain(|r| *r != word.pronunciation);
        readings.insert(0, &word.pronunciation);
    }
    readings.retain(|r| *r != word.base_word);

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
                ui.label(text.color(text_color));
            }
        }
        // Each reading stays whole: a reading that does not fit moves to the next line
        let parts = readings_parts(&readings, palette);
        let mut i = 0;
        while i < parts.len() && !readings.is_empty() {
            // A reading with the bracket or separator before it, and the closing bracket after the last one
            let end = if i + 3 == parts.len() { parts.len() } else { (i + 2).min(parts.len()) };
            let job = colored(ui, &parts[i..end], body * 1.1, false);
            ui.add(egui::Label::new(job).wrap_mode(egui::TextWrapMode::Extend));
            i = end;
        }
    });
    if let Some(first) = entries.first() {
        let others: Vec<&str> = first.spellings.iter().copied().filter(|s| *s != word.base_word).collect();
        if !others.is_empty() {
            note(ui, &format!("Also written {}", others.join("・")));
        }
    }
    if surface != word.base_word {
        note(ui, &format!("Written {surface} in the text"));
    }
    let Some(lookup) = lookup else {
        note(ui, "The dictionary is still loading.");
        return clicked;
    };
    if entries.is_empty() {
        note(ui, "Not in the dictionary.");
    }
    for (i, entry) in entries.iter().take(MAX_ENTRIES).enumerate() {
        ui.separator();
        // The first entry is the word above; the others are other words written the same way
        if i > 0 {
            let spelling = entry.spellings.first().or(entry.readings.first()).copied().unwrap_or_default();
            let mut parts = vec![(spelling, text_color)];
            if !entry.spellings.is_empty() {
                parts.extend(readings_parts(&entry.readings, palette));
            }
            ui.label(colored(ui, &parts, body, false));
            if entry.spellings.len() > 1 {
                note(ui, &format!("Also written {}", entry.spellings[1..].join("・")));
            }
        }
        let numbered = entry.senses.len() > 1;
        let mut previous_tags = None;
        for (n, sense) in entry.senses.iter().enumerate() {
            // Senses often share their tags: shown again only when they change
            if !sense.tags.is_empty() && previous_tags != Some(&sense.tags) {
                let tags: Vec<&str> = sense.tags.iter().map(|t| short_tag(t).unwrap_or_else(|| lookup.words.tag(t))).collect();
                ui.label(colored(ui, &[(&tags.join(", "), palette.grammar)], body * 0.75, true));
            }
            previous_tags = Some(&sense.tags);
            let number = format!("{}. ", n + 1);
            let parts: &[(&str, Color32)] = if numbered { &[(&number, palette.marker), (sense.glosses, text_color)] } else { &[(sense.glosses, text_color)] };
            ui.label(colored(ui, parts, body, false));
        }
        if i + 1 == MAX_ENTRIES && entries.len() > MAX_ENTRIES {
            note(ui, &format!("{} more entries", entries.len() - MAX_ENTRIES));
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
    // Readings, each kept whole (a reading that does not fit moves to the next line), with the separator
    // after it. Kun'yomi mark okurigana with a dot (い.きる), shown dimmed, and affixes with a dash (-うえ).
    let readings = |values: &[&str], color: Color32| -> Vec<Vec<(String, Color32)>> {
        let last = values.len().saturating_sub(1);
        values
            .iter()
            .enumerate()
            .map(|(i, value)| {
                let (stem, okurigana) = value.split_once('.').unwrap_or((value, ""));
                let mut item = vec![(stem.to_string(), color)];
                if !okurigana.is_empty() {
                    item.push((okurigana.to_string(), palette.secondary_text));
                }
                if i < last {
                    item.push(("、".to_string(), palette.secondary_text));
                }
                item
            })
            .collect()
    };
    let label_width = body * 4.2;
    let text_width = (ui.available_width() - label_width - ui.spacing().item_spacing.x).max(body * 6.0);
    let row = |ui: &mut Ui, name: &str, add: &mut dyn FnMut(&mut Ui)| {
        ui.horizontal_top(|ui| {
            let name = egui::Label::new(RichText::new(name).size(body * 0.8).color(palette.secondary_text));
            ui.add_sized([label_width, body * 1.3], name);
            add(ui);
        });
    };
    for (name, items) in [("On", readings(&k.on, palette.marker)), ("Kun", readings(&k.kun, palette.reading)), ("Names", readings(&k.nanori, palette.reading))] {
        if items.is_empty() {
            continue;
        }
        row(ui, name, &mut |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.set_max_width(text_width);
                ui.spacing_mut().item_spacing.x = 0.0;
                for item in &items {
                    let parts: Vec<(&str, Color32)> = item.iter().map(|(t, c)| (t.as_str(), *c)).collect();
                    ui.add(egui::Label::new(colored(ui, &parts, body, false)).wrap_mode(egui::TextWrapMode::Extend));
                }
            });
        });
    }
    if !k.meanings.is_empty() {
        let text_color = ui.visuals().text_color();
        row(ui, "Meanings", &mut |ui| {
            let mut parts = Vec::new();
            for (i, meaning) in k.meanings.iter().enumerate() {
                if i > 0 {
                    parts.push(("; ", palette.secondary_text));
                }
                parts.push((meaning, text_color));
            }
            // Labels in a row do not wrap unless told to
            let mut job = colored(ui, &parts, body, false);
            job.wrap.max_width = text_width;
            ui.add(egui::Label::new(job).wrap_mode(egui::TextWrapMode::Wrap));
        });
    }
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
    fn wraps_lines_without_punctuation_at_their_start() {
        let chars: Vec<char> = "あいう。えお「かき」くけ".chars().collect();
        // 。 and 」 hang at the end of a line rather than start the next one, and 「 moves to the next line
        assert_eq!(wrap(&chars, 3), [0..4, 4..6, 6..10, 10..12]);
        // 「 moves to the next line with what it opens
        assert_eq!(wrap(&chars, 6), [0..6, 6..12]);
        assert_eq!(wrap(&[], 5), vec![0..0]);
    }

    #[test]
    fn shows_half_width_characters_full_width() {
        assert_eq!("40 ab!".chars().map(full_width).collect::<String>(), "４０　ａｂ！");
        assert_eq!(full_width('魔'), '魔');
    }

    #[test]
    fn shift_wheel_and_pinch_change_the_font_size() {
        assert_eq!(zoomed(vec![wheel(Modifiers::NONE)]), 20.0);
        assert!((zoomed(vec![wheel(Modifiers::SHIFT)]) - 20.0 * WHEEL_ZOOM * WHEEL_ZOOM).abs() < 1e-3);
        assert!((zoomed(vec![Event::Zoom(1.5)]) - 30.0).abs() < 1e-3);
        assert_eq!(zoomed(vec![Event::Zoom(10.0)]), *FONT_SIZES.end());
    }
}
