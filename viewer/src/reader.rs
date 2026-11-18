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
use egui::{pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, FontId, Painter, Pos2, Rect, RichText, Sense, Ui, Vec2};
use jpfe::dictionary::{Definitions, Kanjidic, DEFINITIONS_FILE, KANJI_FILE};
use jpfe::tokenizer::{is_cjk_kanji, JapaneseTokenizer, ParsedWord, WordKind};
use serde::{Deserialize, Serialize};

use crate::book::Book;
use crate::theme::{Palette, WordFills};

pub const DEFAULT_FONT_SIZE: f32 = 20.0;
pub const FONT_SIZES: RangeInclusive<f32> = 10.0..=48.0;
/// Vertical text (縦書き) unless the reader picks horizontal text.
pub const DEFAULT_VERTICAL: bool = true;
/// One page at a time, turned by dragging sideways or with the arrow keys, unless the reader picks scrolling.
pub const DEFAULT_PAGINATE: bool = true;
/// Words are not highlighted unless the reader asks for it.
pub const DEFAULT_HIGHLIGHT: bool = false;
/// Rounding of the rectangles under words (highlighted, hovered, selected), in font sizes (and in points, at
/// least and at most).
const WORD_RADIUS: f32 = 0.2;
const WORD_RADIUS_POINTS: RangeInclusive<f32> = 3.0..=8.0;
/// Height of the middle of a Japanese font's em box above the baseline, in font sizes (the em box goes from 0.12
/// below the baseline to 0.88 above it).
const EM_BOX_MIDDLE: f32 = 0.38;
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
/// Characters saved with a place, to find its sentence again if the chapter's text changes.
const PLACE_TEXT: usize = 16;
/// Seconds after scrolling stops before the place is saved.
const SCROLL_SETTLE: f64 = 0.6;
/// A drag turns the page once it went this share of the view's width sideways (in points, at least and at most).
const TURN_DRAG: f32 = 0.06;
const TURN_DRAG_POINTS: RangeInclusive<f32> = 24.0..=64.0;
/// A page a drag shows stays until the drag comes back within this share of the threshold.
const DRAG_HYSTERESIS: f32 = 0.7;

/// What the reader remembers of a book: where it was left. The app saves it by book id in its state, so
/// it is kept when the app restarts; two copies of it (from two devices) merge by keeping the one
/// `modified` last.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BookReaderState {
    /// Chapter, as a position in the book's chapter list
    pub chapter: usize,
    /// Where the sentence the page (or the view, when scrolling) starts in begins, in bytes of the chapter text
    pub offset: usize,
    /// The first characters of the chapter text from `offset`, to find the sentence again if the text
    /// changes (the book analyzed again by a new build)
    pub text: String,
    /// When the place last changed, in milliseconds since 1970-01-01 UTC
    pub modified: u64,
}

impl BookReaderState {
    /// The same place, whenever it was saved.
    pub fn same_place(&self, other: &Self) -> bool {
        (self.chapter, self.offset, &self.text) == (other.chapter, other.offset, &other.text)
    }
}

/// Where a saved place is in `text`: at `offset` if the text there still starts with `start`, or else where
/// `start` is found nearest to it (the chapter changed); at `offset` if it is not found at all.
fn find_place(text: &str, offset: usize, start: &str) -> usize {
    let offset = text.floor_char_boundary(offset.min(text.len()));
    if start.is_empty() || text[offset..].starts_with(start) {
        return offset;
    }
    text.match_indices(start).map(|(i, _)| i).min_by_key(|i| i.abs_diff(offset)).unwrap_or(offset)
}

/// Whether a sentence ends just before character `i`: after 。！？ and the closing brackets that follow
/// (half-width ! and ? are shown, and stored here, full width).
fn sentence_ends_before(chars: &[char], i: usize) -> bool {
    if chars.get(i).is_some_and(|&c| "。！？」』）】〕〉》］｝".contains(c)) {
        return false;
    }
    let mut j = i;
    while j > 0 && "」』）】〕〉》］｝".contains(chars[j - 1]) {
        j -= 1;
    }
    j > 0 && "。！？".contains(chars[j - 1])
}

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
    /// Its parts, in order, as characters of its paragraph covering `chars` (see [`word_parts`])
    parts: Vec<(Range<usize>, Part)>,
}

/// What part of a word some of its characters are, which gives their color when the reader highlights words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Noun,
    /// Names of people, places, organizations (名詞-固有名詞)
    ProperNoun,
    Verb,
    /// い- and な-adjectives
    Adjective,
    Adverb,
    /// A counter, with its number (５冊)
    Counter,
    Pronoun,
    /// Interjections, conjunctions, adnominals, expressions
    Other,
    /// The prefix before a word: polite (お, ご), ordinal (第), or a kanji prefix (大)
    Prefix,
    /// Conjugation endings and auxiliaries after a word (られる, た, て, ない)
    Ending,
    /// Suffixes after a word (さ, ら, たち), and a suffix that is a word of its own (家)
    Suffix,
    /// Particles and auxiliaries that are words of their own
    Grammar,
    /// Numbers and fragments
    Faint,
}

impl Part {
    /// The word itself (its affixes left out), from its kind and part of speech.
    fn of_stem(word: &ParsedWord) -> Self {
        match word.kind {
            WordKind::Grammar => Self::Grammar,
            WordKind::Number | WordKind::Fragment => Self::Faint,
            WordKind::Word if word.written_reading.is_some() => Self::Counter,
            WordKind::Word => match word.pos.as_str() {
                "名詞" if word.proper_noun => Self::ProperNoun,
                "名詞" => Self::Noun,
                "動詞" => Self::Verb,
                "形容詞" | "形状詞" => Self::Adjective,
                "副詞" => Self::Adverb,
                "代名詞" => Self::Pronoun,
                "接尾辞" => Self::Suffix,
                _ => Self::Other,
            },
        }
    }

    /// One of a word's affixes (see [`ParsedWord::affixes`]): prefixes are tagged 接頭辞 and suffixes 接尾辞;
    /// the rest are conjugation endings (auxiliaries, the particles of conjugated forms such as て, the ない of
    /// 美しくない, the stems of そうだ and ようだ), except the adjective づらい (辛い), a suffix like やすい.
    fn of_affix(affix: &ParsedWord) -> Self {
        match affix.pos.as_str() {
            "接頭辞" => Self::Prefix,
            "接尾辞" => Self::Suffix,
            "形容詞" if !matches!(affix.base_word.as_str(), "無い" | "ない") => Self::Suffix,
            _ => Self::Ending,
        }
    }

    fn fill(self, fills: &WordFills) -> Color32 {
        match self {
            Self::Noun => fills.noun,
            Self::ProperNoun => fills.proper_noun,
            Self::Verb => fills.verb,
            Self::Adjective => fills.adjective,
            Self::Adverb => fills.adverb,
            Self::Counter => fills.counter,
            Self::Pronoun => fills.pronoun,
            Self::Other => fills.other,
            Self::Prefix => fills.prefix,
            Self::Ending => fills.ending,
            Self::Suffix => fills.suffix,
            Self::Grammar => fills.grammar,
            Self::Faint => fills.faint,
        }
    }
}

/// The parts of `word`, written `surface` in the text, in order, as characters of the surface that cover it: the
/// word itself (its stem) and each affix. Grammar, numbers and fragments are one part, affixes included.
fn word_parts(word: &ParsedWord, surface: &str) -> Vec<(Range<usize>, Part)> {
    let stem = Part::of_stem(word);
    let char_at = |byte: usize| surface.get(..byte).map_or_else(|| surface.chars().count(), |t| t.chars().count());
    if word.kind != WordKind::Word || word.affixes.is_empty() {
        return vec![(0..char_at(surface.len()), stem)];
    }
    let mut affixes: Vec<(Range<usize>, Part)> = word
        .affixes
        .iter()
        .filter_map(|a| {
            let span = a.span.start.checked_sub(word.span.start)?..a.span.end.checked_sub(word.span.start)?.min(surface.len());
            (span.start < span.end).then(|| (span, Part::of_affix(a)))
        })
        .collect();
    affixes.sort_by_key(|(span, _)| span.start);
    // In bytes, then in characters
    let mut parts = Vec::with_capacity(affixes.len() * 2 + 1);
    let mut at = 0;
    for (span, part) in affixes {
        if at < span.start {
            parts.push((at..span.start, stem));
        }
        if at.max(span.start) < span.end {
            parts.push((at.max(span.start)..span.end, part));
            at = span.end;
        }
    }
    if at < surface.len() {
        parts.push((at..surface.len(), stem));
    }
    parts.into_iter().map(|(span, part)| (char_at(span.start)..char_at(span.end), part)).collect()
}

/// The corners of the rectangle of some characters of a word: rounded at the word's real start and end (`start`,
/// `end`), square where the word goes on, on the next line or in its next part. The start is the left side in
/// horizontal text and the top in vertical text. Rectangles cover their characters' whole cells, so two words'
/// rectangles meet, and their rounded corners show where one word ends and the next starts.
fn word_corners(start: bool, end: bool, vertical: bool, radius: u8) -> CornerRadius {
    let (s, e) = (if start { radius } else { 0 }, if end { radius } else { 0 });
    if vertical { CornerRadius { nw: s, ne: s, sw: e, se: e } } else { CornerRadius { nw: s, sw: s, ne: e, se: e } }
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
    /// At its last page (turning back from the first page of the next chapter)
    pub at_end: bool,
    /// A drag in progress, to go on with in that chapter ([`Reader::continue_drag`])
    pub drag: Option<PageDrag>,
}

/// A drag turning pages: the page it started on, the one other page it can show, and which of them it shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageDrag {
    /// The chapter, first line and anchor of the page the drag started on
    chapter: usize,
    page: usize,
    anchor: usize,
    /// The other page: 1 for the page after that one, -1 for the page before, 0 until the drag first goes past
    /// the threshold, which fixes it for the rest of the drag
    other: i8,
    /// The page shown: 0 for the one the drag started on, or `other`
    shown: i8,
}

/// Which page a drag shows (see [`PageDrag`]), from how far it went toward the next page (negative toward the
/// previous one): the other page past the threshold in its direction, the page it started on anywhere else. Until
/// the other page is fixed (`other` 0), either direction can fix it. A page shown stays until the drag comes back a
/// little within the threshold, so that it does not flicker there.
fn drag_page(shown: i8, other: i8, forward: f32, threshold: f32) -> i8 {
    let back = threshold * DRAG_HYSTERESIS;
    let toward = |direction: i8| {
        let along = forward * f32::from(direction);
        along >= threshold || (shown == direction && along > back)
    };
    match other {
        0 if toward(1) => 1,
        0 if toward(-1) => -1,
        0 => 0,
        other if toward(other) => other,
        _ => 0,
    }
}

/// Colors of the text and of the highlighted words.
#[derive(Clone, Copy)]
struct Colors {
    text: Color32,
    secondary: Color32,
    hover: Color32,
    selected: Color32,
    /// Fills under every word, by part, when the reader highlights words
    words: Option<WordFills>,
}

/// The characters of a run (a word, or some of it) on one line, and its rectangle there.
#[derive(Clone, Debug, PartialEq)]
struct Piece {
    rect: Rect,
    chars: Range<usize>,
    /// Whether the run starts on this line, and ends on it (otherwise the line cuts it)
    start: bool,
    end: bool,
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

    /// The paragraph and character at `pos`, if it is on one of `lines`.
    fn char_at(&self, pos: Pos2, origin: Pos2, lines: Range<usize>) -> Option<(usize, usize)> {
        let (across, along) = if self.key.vertical { (origin.x - pos.x, pos.y - origin.y) } else { (pos.y - origin.y, pos.x - origin.x) };
        let index = self.lines.partition_point(|l| l.offset + l.pitch <= across).min(self.lines.len().checked_sub(1)?);
        let line = &self.lines[index];
        if !lines.contains(&index) || across < line.offset || along < 0.0 {
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

    /// The rectangle covering characters `chars` of `line` (all on it), in the glyphs' band.
    fn chars_rect(&self, line: &Line, chars: Range<usize>, origin: Pos2) -> Rect {
        let cell = |i: usize| self.cell(line, line.first_cell + i - line.chars.start, origin);
        let rect = cell(chars.start).union(cell(chars.end - 1));
        // The glyphs' band, not the space between lines
        let gap = (line.pitch - self.key.size * line.scale * 1.3) / 2.0;
        if self.key.vertical { rect.shrink2(vec2(gap, 0.0)) } else { rect.shrink2(vec2(0.0, gap)) }
    }

    /// The characters `chars` of a paragraph that are on `line`, if any.
    fn piece(&self, line: &Line, chars: &Range<usize>, origin: Pos2) -> Option<Piece> {
        let (start, end) = (chars.start.max(line.chars.start), chars.end.min(line.chars.end));
        (start < end).then(|| Piece { rect: self.chars_rect(line, start..end, origin), chars: start..end, start: start == chars.start, end: end == chars.end })
    }

    /// The characters `chars` of a paragraph, one piece for each line they are on among `lines`.
    fn rects(&self, paragraph: usize, chars: &Range<usize>, origin: Pos2, lines: Range<usize>) -> Vec<Piece> {
        let own = self.paragraph_lines.get(paragraph).cloned().unwrap_or_default();
        let lines = own.start.max(lines.start)..own.end.min(lines.end);
        if lines.is_empty() {
            return Vec::new();
        }
        self.lines[lines].iter().filter_map(|line| self.piece(line, chars, origin)).collect()
    }

    /// Rounding of the corners of the rectangles under words.
    fn word_radius(&self) -> u8 {
        (self.key.size * WORD_RADIUS).clamp(*WORD_RADIUS_POINTS.start(), *WORD_RADIUS_POINTS.end()).round() as u8
    }

    /// Pages are made of items: the lines, then, when `end` is not zero, room of that thickness after the
    /// text for the next chapter button.
    fn items(&self, end: f32) -> usize {
        self.lines.len() + usize::from(end > 0.0)
    }

    /// Where an item starts (its distance from the first line), and its thickness.
    fn item(&self, item: usize, end: f32) -> (f32, f32) {
        self.lines.get(item).map_or((self.extent, end), |l| (l.offset, l.pitch))
    }

    /// End of the page starting at item `start`: as many whole items as fit in `room` (at least one).
    fn page_end(&self, start: usize, room: f32, end: f32) -> usize {
        let items = self.items(end);
        let from = self.item(start, end).0;
        let mut stop = (start + 1).min(items);
        while stop < items {
            let (offset, pitch) = self.item(stop, end);
            if offset + pitch - from > room {
                break;
            }
            stop += 1;
        }
        stop
    }

    /// Start of the page after the one starting at `start` (`None` after the last one), not starting with
    /// the empty lines between paragraphs.
    fn next_page(&self, start: usize, room: f32, end: f32) -> Option<usize> {
        let mut next = self.page_end(start, room, end);
        while next + 1 < self.lines.len() && self.lines[next].paragraph.is_some() && self.lines[next].chars.is_empty() {
            next += 1;
        }
        (next < self.items(end)).then_some(next)
    }

    /// Start of the page before item `start`. Pages follow each other from the chapter's start, so turning
    /// back shows the pages turning forward showed, the first one with the chapter title: the last of them
    /// that starts before `start` (it shows lines of the current page too when `start` is not one of them,
    /// after the text was laid out again).
    fn page_before(&self, start: usize, room: f32, end: f32) -> usize {
        let mut page = 0;
        while let Some(next) = self.next_page(page, room, end).filter(|&next| next < start) {
            page = next;
        }
        page
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
    // Its em box in the middle of the cell, rather than its line box, whose room for accents above makes the glyph
    // sit low: a Japanese font's em box goes from 0.12 em below the baseline to 0.88 em above it
    let baseline = galley.rows.first().and_then(|row| row.row.glyphs.first().map(|g| row.pos.y + g.pos.y));
    let top = baseline.map_or(center.y - size.y / 2.0, |baseline| center.y - baseline + EM_BOX_MIDDLE * font.size);
    painter.galley(pos2(center.x - size.x / 2.0, top), galley, color);
}

pub struct Reader {
    pub book: u64,
    pub chapter: usize,
    book_name: String,
    title: Vec<char>,
    chapters: usize,
    text: String,
    paragraphs: Vec<Paragraph>,
    words: Vec<Word>,
    /// Words are found once the dictionary is ready
    tokenized: bool,
    layout: Option<Layout>,
    /// Where the reader is: the start of the sentence the page (or the view, when scrolling) starts in, in
    /// bytes of the chapter text. It moves when the reader turns a page or scrolls, not when the text is laid
    /// out again, which shows it at the start of the page (zoom, window size, direction, pagination).
    anchor: usize,
    /// Show the chapter's end once it is laid out (turning back from the next chapter)
    to_end: bool,
    /// When the anchor last moved, until its place is saved (`-∞` to save it right away)
    anchor_moved: Option<f64>,
    /// First line of the page shown (paginated)
    page: usize,
    /// The drag in progress (paginated)
    drag: Option<PageDrag>,
    /// Show this line's page once laid out, when a drag comes back to the chapter it started in
    start_line: Option<usize>,
    /// Scroll offset set or seen in the last frame (scrolling)
    scroll_offset: Option<f32>,
    /// Layout, visible size and pagination the anchor was last shown for
    placed_for: Option<(LayoutKey, Vec2, bool)>,
    word_popup: Option<WordPopup>,
    kanji_popup: Option<KanjiPopup>,
    /// Where the word window was in the last frame, to open the kanji window next to it
    word_popup_rect: Option<Rect>,
    popups: u64,
}

impl Reader {
    pub fn new(book: &Book, chapter: usize) -> Self {
        let pdf = book.file_name.to_lowercase().ends_with(".pdf");
        let mut reader = Self::from_text(book.id, &book.name, &book.chapter_label(chapter), book.chapter_text(chapter), pdf, book.chapters.len());
        reader.chapter = chapter;
        reader
    }

    fn from_text(book: u64, book_name: &str, title: &str, text: &str, pdf: bool, chapters: usize) -> Self {
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
            book,
            chapter: 0,
            book_name: book_name.to_string(),
            title: title.chars().map(full_width).collect(),
            chapters,
            text: text.to_string(),
            paragraphs,
            words: Vec::new(),
            tokenized: false,
            layout: None,
            anchor: 0,
            to_end: false,
            // A chapter opened at its start is a place to save
            anchor_moved: Some(f64::NEG_INFINITY),
            page: 0,
            drag: None,
            start_line: None,
            scroll_offset: None,
            placed_for: None,
            word_popup: None,
            kanji_popup: None,
            word_popup_rect: None,
            popups: 0,
        }
    }

    /// Open at a saved place (see [`BookReaderState`]).
    pub fn go_to(&mut self, offset: usize, start: &str) {
        self.anchor = find_place(&self.text, offset, start);
        self.anchor_moved = None;
    }

    /// Open at the chapter's last page (or its end, when scrolling).
    pub fn go_to_end(&mut self) {
        self.to_end = true;
        self.anchor_moved = None;
    }

    /// Where the reader is, to save (with `modified` left for the app to set).
    pub fn place(&self) -> BookReaderState {
        let text = self.text.get(self.anchor..).unwrap_or_default().chars().take(PLACE_TEXT).collect();
        BookReaderState { chapter: self.chapter, offset: self.anchor, text, modified: 0 }
    }

    /// The place, once it has moved and settled: right after a page turn, or a moment after scrolling stops.
    pub fn settled_place(&mut self, ctx: &egui::Context) -> Option<BookReaderState> {
        let moved = self.anchor_moved?;
        let (now, down) = ctx.input(|i| (i.time, i.pointer.any_down()));
        let wait = SCROLL_SETTLE - (now - moved);
        if down {
            return None;
        }
        if wait > 0.0 {
            ctx.request_repaint_after_secs(wait as f32);
            return None;
        }
        self.anchor_moved = None;
        Some(self.place())
    }

    /// Move the anchor at `now`; a page turn (`settled`) is saved right away, scrolling once it stops.
    fn move_anchor(&mut self, anchor: usize, now: f64, settled: bool) {
        self.anchor = anchor;
        self.anchor_moved = Some(if settled { f64::NEG_INFINITY } else { now });
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
                let parts = word_parts(&word, &text[start..end]).into_iter().map(|(c, part)| (c.start + chars.start..c.end + chars.start, part)).collect();
                self.words.push(Word { parsed: word, paragraph: index, chars, parts });
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

    /// The line showing byte `offset` of the chapter text (the title for the chapter's start).
    fn line_at_offset(&self, layout: &Layout, offset: usize) -> usize {
        if offset == 0 {
            return 0;
        }
        let p = self.paragraphs.partition_point(|p| p.span.start <= offset).saturating_sub(1);
        let (Some(paragraph), Some(lines)) = (self.paragraphs.get(p), layout.paragraph_lines.get(p)) else { return 0 };
        let char = self.text.get(paragraph.span.start..offset.min(paragraph.span.end)).map_or(0, |t| t.chars().count());
        lines.clone().find(|&l| char < layout.lines[l].chars.end).unwrap_or(lines.end.saturating_sub(1))
    }

    /// Where the sentence that line `line` starts in begins, in bytes of the chapter text (the chapter's start
    /// for its title).
    fn sentence_start(&self, layout: &Layout, line: usize) -> usize {
        let Some(line) = layout.lines.get(line.min(layout.lines.len().saturating_sub(1))) else { return 0 };
        let Some(paragraph) = line.paragraph.and_then(|p| self.paragraphs.get(p)) else { return 0 };
        let mut start = line.chars.start.min(paragraph.chars.len());
        while start > 0 && !sentence_ends_before(&paragraph.chars, start) {
            start -= 1;
        }
        let text = &self.text[paragraph.span.clone()];
        paragraph.span.start + text.char_indices().nth(start).map_or(text.len(), |(i, _)| i)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn ui(&mut self, ui: &mut Ui, lookup: Option<&Lookup>, font_size: &mut f32, vertical: bool, paginate: bool, highlight: bool, palette: Palette) -> ReaderResponse {
        let mut response = ReaderResponse::default();
        zoom(ui, font_size);
        let size = *font_size;
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) && self.kanji_popup.take().is_none() {
            self.word_popup = None;
        }

        let view = ui.available_size();
        let margin = size * MARGIN;
        // A cell of room for punctuation hanging past the end of a line
        let length = if vertical { view.y - 2.0 * margin - size } else { (view.x - 2.0 * margin - size).min(size * MAX_LINE) };
        let key = LayoutKey { size, vertical, length: length.max(size * 4.0) };
        let layout = match self.layout.take() {
            Some(layout) if layout.key == key => layout,
            _ => self.lay_out(key),
        };
        // Show the anchor again when the text is laid out again, or the view changes
        let replace = self.placed_for != Some((key, view, paginate));
        self.placed_for = Some((key, view, paginate));
        let colors = Colors {
            text: ui.visuals().text_color(),
            secondary: palette.secondary_text,
            hover: palette.accent.gamma_multiply(0.12),
            selected: palette.accent.gamma_multiply(0.28),
            words: highlight.then_some(palette.words),
        };
        let clicked = if paginate {
            self.paged_ui(ui, &layout, view, replace, colors, &mut response)
        } else {
            self.drag = None;
            self.scrolled_ui(ui, &layout, view, replace, colors, &mut response)
        };
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

    /// The whole chapter, scrolling (sideways for vertical text); returns a click on the text, with the word
    /// clicked if any.
    fn scrolled_ui(&mut self, ui: &mut Ui, layout: &Layout, view: Vec2, replace: bool, colors: Colors, response: &mut ReaderResponse) -> Option<Option<(usize, Pos2, Align2)>> {
        let (size, vertical) = (layout.key.size, layout.key.vertical);
        let content = layout.content_size(view);
        let scrollable = if vertical { content.x - view.x } else { content.y - view.y }.max(0.0);
        let mut area = if vertical { egui::ScrollArea::horizontal() } else { egui::ScrollArea::vertical() }
            .id_salt(("reader", self.book, self.chapter, vertical))
            .auto_shrink(false);
        if replace {
            // The anchor's line at the start of the view (vertical text starts on the right), or the chapter's end
            let target = if self.to_end {
                if vertical { 0.0 } else { scrollable }
            } else {
                let line = &layout.lines[self.line_at_offset(layout, self.anchor)];
                if vertical { content.x - line.offset - view.x } else { line.offset }.clamp(0.0, scrollable)
            };
            // Coming to the end moves the anchor, found below from where the view lands
            self.scroll_offset = (!std::mem::take(&mut self.to_end)).then_some(target);
            area = if vertical { area.horizontal_scroll_offset(target) } else { area.vertical_scroll_offset(target) };
        }
        // The mouse wheel moves through vertical text: down goes on to the next columns, on the left
        let view_rect = Rect::from_min_size(ui.cursor().min, view);
        if vertical && ui.rect_contains_pointer(view_rect) {
            ui.ctx().input_mut(|i| {
                let delta = i.smooth_scroll_delta;
                i.smooth_scroll_delta = vec2(delta.x - delta.y, 0.0);
            });
        }

        let selected = self.word_popup.as_ref().map(|p| p.word);
        let mut clicked = None;
        let shown = area.show(ui, |ui| {
            let (rect, background) = ui.allocate_exact_size(content, Sense::click());
            let origin = layout.origin(rect);
            let all = 0..layout.lines.len();
            let hovered = background.hover_pos().and_then(|pos| self.word_at_pos(layout, origin, all.clone(), pos));
            if hovered.is_some() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            let drawn = layout.visible(ui.clip_rect(), origin);
            self.paint(ui.painter(), layout, origin, drawn, all.clone(), [(hovered, colors.hover), (selected, colors.selected)], colors);
            if self.chapter + 1 < self.chapters && self.next_chapter_button(ui, layout, origin) {
                response.chapter = Some(self.chapter + 1);
            }
            if background.clicked() {
                clicked = Some(background.interact_pointer_pos().and_then(|pos| self.popup_for(ui.ctx(), layout, origin, all.clone(), pos)));
            }
        });
        let offset = shown.state.clamped_offset();
        let along = if vertical { offset.x } else { offset.y };
        if self.scroll_offset.is_none_or(|o| (o - along).abs() > 0.5) {
            self.scroll_offset = Some(along);
            // Scrolled: the anchor is the sentence of the first line whose start is in view
            let margin = size * MARGIN;
            let edge = if vertical { content.x - margin - along - view.x } else { along - margin };
            let first = layout.lines.partition_point(|l| l.offset < edge - 0.5);
            let now = ui.input(|i| i.time);
            self.move_anchor(self.sentence_start(layout, first), now, false);
        }
        clicked
    }

    /// One page of the chapter: whole columns (or lines) from `self.page`, turned by dragging sideways or
    /// with the arrow keys, in the reading direction. Returns a click on the text, with the word clicked if any.
    fn paged_ui(&mut self, ui: &mut Ui, layout: &Layout, view: Vec2, replace: bool, colors: Colors, response: &mut ReaderResponse) -> Option<Option<(usize, Pos2, Align2)>> {
        let (size, vertical) = (layout.key.size, layout.key.vertical);
        let next_chapter = self.chapter + 1 < self.chapters;
        // Room for the next chapter button after the text, as for scrolling
        let end = if next_chapter { size * if vertical { END_SPACE_VERTICAL } else { END_SPACE } } else { 0.0 };
        let room = (if vertical { view.x } else { view.y } - 2.0 * size * MARGIN).max(size);
        let items = layout.items(end);
        let now = ui.input(|i| i.time);
        if replace {
            if std::mem::take(&mut self.to_end) {
                // A full last page
                self.page = layout.page_before(items, room, end);
                self.move_anchor(self.sentence_start(layout, self.page), now, true);
            } else if let Some(line) = self.start_line.take() {
                // Back to the page a drag started on, in this chapter
                self.page = line;
            } else {
                // The page starts with the anchor's line
                self.page = self.line_at_offset(layout, self.anchor);
            }
        }
        self.page = self.page.min(items.saturating_sub(1));

        let (rect, background) = ui.allocate_exact_size(view, Sense::click_and_drag());
        // Dragging: while the pointer is down, the page shown follows how far it went from where it was pressed,
        // in the reading direction (a short drag is a click: egui tells them apart). Vertical text reads to the
        // left, so its next page is pulled in from the left, toward the right; horizontal text reads to the
        // right, so its next page comes from the right, toward the left. Whole pages switch at once.
        // A drag starts on the page, and lasts until the button (or finger) is up, measured from where it was
        // pressed: even if egui stops calling it a drag of the page for a frame, it never starts again from the
        // page it shows, which would reach a third page
        let (down, moved) = ui.input(|i| (i.pointer.primary_down(), i.pointer.press_origin().zip(i.pointer.latest_pos()).map(|(from, to)| to - from)));
        if self.drag.is_none() && background.dragged() {
            self.drag = Some(PageDrag { chapter: self.chapter, page: self.page, anchor: self.anchor, other: 0, shown: 0 });
        }
        if let Some(drag) = self.drag.filter(|_| down) {
            let threshold = (rect.width() * TURN_DRAG).clamp(*TURN_DRAG_POINTS.start(), *TURN_DRAG_POINTS.end());
            let forward = moved.map_or(0.0, |d| if vertical { d.x } else { -d.x });
            let wanted = drag_page(drag.shown, drag.other, forward, threshold);
            if wanted != drag.shown {
                self.show_drag_page(layout, room, end, wanted, response);
                ui.ctx().request_repaint();
            }
        } else if let Some(drag) = self.drag.take() {
            // Released: the page shown stays, and its place is saved
            if drag.shown != 0 {
                self.move_anchor(self.anchor, now, true);
            }
        }
        // An arrow key turns the page in the reading direction, though not while a widget has the keyboard (a
        // text field, or keyboard navigation)
        let mut turn = None;
        if self.drag.is_none() && !ui.ctx().egui_wants_keyboard_input() {
            let (left, right) = ui.input(|i| (i.key_pressed(egui::Key::ArrowLeft), i.key_pressed(egui::Key::ArrowRight)));
            if left != right {
                turn = Some(left == vertical);
            }
        }
        // Past the last page, the next chapter; before the first one, the previous chapter's last page
        let turned = match turn {
            Some(true) => match layout.next_page(self.page, room, end) {
                Some(next) => {
                    self.page = next;
                    true
                }
                None => {
                    response.chapter = next_chapter.then_some(self.chapter + 1);
                    false
                }
            },
            Some(false) if self.page > 0 => {
                self.page = layout.page_before(self.page, room, end);
                true
            }
            Some(false) if self.chapter > 0 => {
                response.chapter = Some(self.chapter - 1);
                response.at_end = true;
                false
            }
            _ => false,
        };
        if turned {
            self.move_anchor(self.sentence_start(layout, self.page), now, true);
            self.close_popups();
        }

        let page = self.page..layout.page_end(self.page, room, end);
        let lines = page.start.min(layout.lines.len())..page.end.min(layout.lines.len());
        // The page's first line at the start of the view
        let mut origin = layout.origin(rect);
        let first = layout.item(page.start, end).0;
        if vertical {
            origin.x += first;
        } else {
            origin.y -= first;
        }
        let hovered = background.hover_pos().filter(|_| self.drag.is_none() && !background.dragged()).and_then(|pos| self.word_at_pos(layout, origin, lines.clone(), pos));
        if hovered.is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let selected = self.word_popup.as_ref().map(|p| p.word);
        let highlights = [(hovered, colors.hover), (selected, colors.selected)];
        self.paint(&ui.painter_at(rect), layout, origin, lines.clone(), lines.clone(), highlights, colors);
        if next_chapter && page.end == items && self.next_chapter_button(ui, layout, origin) {
            response.chapter = Some(self.chapter + 1);
        }
        background
            .clicked()
            .then(|| background.interact_pointer_pos().and_then(|pos| self.popup_for(ui.ctx(), layout, origin, lines.clone(), pos)))
    }

    /// While a drag is in progress, show the page it started on (`wanted` 0), the next one (1) or the previous
    /// one (-1). The next or previous page may be in another chapter: the app then opens it, and the drag goes
    /// on there (see [`ReaderResponse::drag`]).
    fn show_drag_page(&mut self, layout: &Layout, room: f32, end: f32, wanted: i8, response: &mut ReaderResponse) {
        let Some(mut drag) = self.drag else { return };
        // The first page past the threshold is the only other page this drag shows
        if wanted != 0 {
            drag.other = wanted;
        }
        if drag.chapter != self.chapter {
            // Shown from the next or previous chapter: back to the chapter the drag started in, which shows
            // the page wanted from there
            drag.shown = 0;
            response.chapter = Some(drag.chapter);
            response.drag = Some(drag);
            return;
        }
        let target = match wanted {
            0 => Some(drag.page),
            1 => layout.next_page(drag.page, room, end),
            _ => (drag.page > 0).then(|| layout.page_before(drag.page, room, end)),
        };
        match target {
            Some(page) => {
                self.page = page;
                // The page the drag started on keeps its place as it was
                self.anchor = if wanted == 0 { drag.anchor } else { self.sentence_start(layout, page) };
            }
            None => {
                let chapter = if wanted > 0 { (self.chapter + 1 < self.chapters).then_some(self.chapter + 1) } else { self.chapter.checked_sub(1) };
                // Nothing before the first page of the book, or after its last one: the page stays
                let Some(chapter) = chapter else {
                    self.drag = Some(drag);
                    return;
                };
                response.chapter = Some(chapter);
                response.at_end = wanted < 0;
                response.drag = Some(PageDrag { shown: wanted, ..drag });
            }
        }
        drag.shown = wanted;
        self.drag = Some(drag);
        if wanted != 0 {
            self.close_popups();
        }
    }

    /// Go on with a drag that started in another chapter, or come back to the page it started on.
    pub fn continue_drag(&mut self, drag: PageDrag) {
        if drag.chapter == self.chapter {
            self.anchor = drag.anchor;
            self.start_line = Some(drag.page);
            self.anchor_moved = None;
        }
        self.drag = Some(drag);
    }

    /// The word is no longer in view.
    fn close_popups(&mut self) {
        self.word_popup = None;
        self.kanji_popup = None;
    }

    /// Draws lines `drawn`, with the words of `highlights` filled where they are on lines `lines` (over the
    /// fills of every word, when words are highlighted).
    #[allow(clippy::too_many_arguments)]
    fn paint(&self, painter: &Painter, layout: &Layout, origin: Pos2, drawn: Range<usize>, lines: Range<usize>, highlights: [(Option<usize>, Color32); 2], colors: Colors) {
        let (size, vertical) = (layout.key.size, layout.key.vertical);
        let radius = layout.word_radius();
        if let Some(fills) = &colors.words {
            for line in &layout.lines[drawn.clone()] {
                self.paint_word_fills(painter, layout, line, origin, fills);
            }
        }
        for (word, fill) in highlights {
            if let Some(word) = word.and_then(|w| self.words.get(w)) {
                for piece in layout.rects(word.paragraph, &word.chars, origin, lines.clone()) {
                    painter.rect_filled(piece.rect, word_corners(piece.start, piece.end, vertical, radius), fill);
                }
            }
        }
        for line in &layout.lines[drawn] {
            let (chars, color) = match line.paragraph {
                None => (&self.title, colors.text),
                Some(p) => (&self.paragraphs[p].chars, if self.paragraphs[p].page_number { colors.secondary } else { colors.text }),
            };
            let font = FontId::proportional(size * line.scale);
            for (i, &c) in chars[line.chars.clone()].iter().enumerate() {
                if c != '　' {
                    draw_char(painter, c, layout.cell(line, line.first_cell + i, origin), &font, color, vertical);
                }
            }
        }
    }

    /// The fills under the words of `line`, colored by part: the word's piece on the line in the color of its
    /// stem, then the regions of its other parts over it, square where they meet so the word reads as one
    /// rectangle. PDF page numbers are not words, and are left as they are.
    fn paint_word_fills(&self, painter: &Painter, layout: &Layout, line: &Line, origin: Pos2, fills: &WordFills) {
        let Some(paragraph) = line.paragraph.and_then(|p| self.paragraphs.get(p)) else { return };
        if paragraph.page_number || line.chars.is_empty() {
            return;
        }
        let (vertical, radius) = (layout.key.vertical, layout.word_radius());
        let words = &self.words[paragraph.words.clone()];
        let first = words.partition_point(|w| w.chars.end <= line.chars.start);
        for word in words[first..].iter().take_while(|w| w.chars.start < line.chars.end) {
            let Some(piece) = layout.piece(line, &word.chars, origin) else { continue };
            let stem = Part::of_stem(&word.parsed);
            painter.rect_filled(piece.rect, word_corners(piece.start, piece.end, vertical, radius), stem.fill(fills));
            for (chars, part) in word.parts.iter().filter(|(_, part)| *part != stem) {
                let (start, end) = (chars.start.max(piece.chars.start), chars.end.min(piece.chars.end));
                if start < end {
                    let rect = layout.chars_rect(line, start..end, origin);
                    painter.rect_filled(rect, word_corners(start == word.chars.start, end == word.chars.end, vertical, radius), part.fill(fills));
                }
            }
        }
    }

    /// The next chapter button, after the text: below it, or on its left for vertical text. True when clicked.
    fn next_chapter_button(&self, ui: &mut Ui, layout: &Layout, origin: Pos2) -> bool {
        let size = layout.key.size;
        let (area, align) = if layout.key.vertical {
            let right = origin.x - layout.extent - size * 0.5;
            (Rect::from_min_max(pos2(right - size * (END_SPACE_VERTICAL - 1.0), origin.y), pos2(right, origin.y + size * 2.0)), egui::Align::Max)
        } else {
            let top = origin.y + layout.extent + size;
            (Rect::from_min_size(pos2(origin.x, top), vec2(layout.line_length, size * 2.0)), egui::Align::Center)
        };
        let builder = egui::UiBuilder::new().max_rect(area).layout(egui::Layout::top_down(align));
        ui.scope_builder(builder, |ui| ui.add(egui::Button::new(RichText::new("Next chapter ›").size(size * 0.8))).clicked()).inner
    }

    /// The word at `pos`, if it is on one of `lines`.
    fn word_at_pos(&self, layout: &Layout, origin: Pos2, lines: Range<usize>, pos: Pos2) -> Option<usize> {
        layout.char_at(pos, origin, lines).and_then(|(p, c)| self.word_at(p, c))
    }

    /// The word at `pos` (on one of `lines`), and where its window opens: beside it, below it or on its left
    /// for vertical text.
    fn popup_for(&self, ctx: &egui::Context, layout: &Layout, origin: Pos2, lines: Range<usize>, pos: Pos2) -> Option<(usize, Pos2, Align2)> {
        let w = self.word_at_pos(layout, origin, lines.clone(), pos)?;
        let word = &self.words[w];
        let first = layout.rects(word.paragraph, &word.chars, origin, lines).first()?.rect;
        let width = popup_width(ctx, layout.key.size, WORD_POPUP_WIDTH);
        Some(if layout.key.vertical && first.left() - 6.0 - width >= ctx.content_rect().left() {
            (w, first.left_top() - vec2(6.0, 0.0), Align2::RIGHT_TOP)
        } else {
            (w, first.left_bottom() + vec2(0.0, 4.0), Align2::LEFT_TOP)
        })
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
            // Titled with the word as the text writes it (数えられるだろう)
            let shown = egui::Window::new(RichText::new(surface).size(body * 1.2))
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

/// `word` at `size`, with each kanji KANJIDIC has as a link; returns the kanji that was clicked.
fn kanji_links(ui: &mut Ui, word: &str, size: f32, lookup: Option<&Lookup>, palette: Palette) -> Option<char> {
    let mut clicked = None;
    let text_color = ui.visuals().text_color();
    for c in word.chars() {
        let text = RichText::new(c.to_string()).size(size);
        if !(is_cjk_kanji(c) && lookup.is_some_and(|l| l.kanji.lookup(c).is_some())) {
            ui.label(text.color(text_color));
            continue;
        }
        let label = ui.add(egui::Label::new(text.color(palette.accent)).sense(Sense::click()));
        if label.hovered() {
            ui.painter().hline(label.rect.x_range(), label.rect.bottom() - 1.0, egui::Stroke::new(1.0, palette.accent));
        }
        if label.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Show this kanji").clicked() {
            clicked = Some(c);
        }
    }
    clicked
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

/// The word with its other spellings and its readings, the number it counts with its reading (一つ【ひとつ】),
/// what its conjugation means, then its definitions; returns a kanji of the word that was clicked.
fn word_ui(ui: &mut Ui, word: &ParsedWord, surface: &str, lookup: Option<&Lookup>, body: f32, palette: Palette) -> Option<char> {
    let mut clicked = None;
    let text_color = ui.visuals().text_color();
    let note = |ui: &mut Ui, text: &str| ui.label(RichText::new(text).size(body * 0.8).color(palette.secondary_text));
    ui.spacing_mut().item_spacing.y = body * 0.35;

    // Those of the word's part of speech first (the particle は, not the noun 歯 read the same)
    let mut entries = lookup.map(|l| l.words.lookup_as(&word.base_word, &word.pos)).unwrap_or_default();
    // The entry read as in the text first, of the word's part of speech if one is: its readings go on the first
    // line, with the word
    let read_so = |e: &jpfe::dictionary::Entry| e.readings.contains(&word.pronunciation.as_str());
    let first = entries.iter().position(|e| read_so(e) && e.is_part_of_speech(&word.pos)).or_else(|| entries.iter().position(read_so));
    if let Some(i) = first {
        entries[..=i].rotate_right(1);
    }
    let mut readings: Vec<&str> = entries.first().map(|e| e.readings.clone()).unwrap_or_default();
    // The reading in the text first; none that only repeats a word written in kana (の【の】)
    if word.pronunciation != word.base_word {
        readings.retain(|r| *r != word.pronunciation);
        readings.insert(0, &word.pronunciation);
    }
    readings.retain(|r| *r != word.base_word);

    let others: Vec<&str> = entries.first().map_or(Vec::new(), |e| e.spellings.iter().copied().filter(|s| *s != word.base_word).collect());
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        clicked = kanji_links(ui, &word.base_word, body * 1.5, lookup, palette);
        // The word's other spellings, smaller
        for other in &others {
            ui.label(RichText::new(", ").size(body * 1.1).color(palette.secondary_text));
            clicked = kanji_links(ui, other, body * 1.1, lookup, palette).or(clicked);
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
    // A counter: the number it counts as the text writes it, read as a whole (一つ【ひとつ】)
    if let Some(reading) = &word.written_reading {
        let mut parts = vec![(surface, ui.visuals().text_color())];
        parts.extend(readings_parts(&[reading.as_str()], palette));
        ui.label(colored(ui, &parts, body * 1.1, false));
    }
    // What the conjugation means (passive or potential (られる), past (た)), in the color of grammar
    if !word.forms.is_empty() {
        ui.label(colored(ui, &[(&word.forms.join(", "), palette.grammar)], body * 0.9, false));
    }
    let Some(lookup) = lookup else {
        note(ui, "The dictionary is still loading.");
        return clicked;
    };
    if entries.is_empty() {
        note(ui, if word.kind == WordKind::Number { "A number (not in word lists)." } else { "Not in the dictionary." });
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
    use egui::{Event, Modifiers, MouseWheelUnit, RawInput, TouchPhase};

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
    fn finds_a_saved_place_again_when_the_text_moved() {
        let text = "前の文。探す文はここ。後の文。";
        let at = text.find("探す").unwrap();
        assert_eq!(find_place(text, at, "探す文"), at);
        // Text added before it: found again by its first characters
        let moved = format!("新しい段落。\n{text}");
        assert_eq!(find_place(&moved, at, "探す文"), moved.find("探す").unwrap());
        // Not found: the offset, on a character boundary
        assert_eq!(find_place(text, at + 1, "ない"), at);
        assert_eq!(find_place(text, 1000, "ない"), text.len());
    }

    #[test]
    fn a_page_starts_in_the_sentence_its_first_line_is_in() {
        let chars: Vec<char> = "あ。「い！」う".chars().collect();
        assert!(sentence_ends_before(&chars, 2));
        // Not between a period and the bracket closing after it
        assert!(!sentence_ends_before(&chars, 5));
        assert!(sentence_ends_before(&chars, 6));
        assert!(!sentence_ends_before(&chars, 4));

        let reader = Reader::from_text(0, "Book", "T", "一二三四五六七。八九十一二三四五\n次", false, 2);
        let layout = reader.lay_out(LayoutKey { size: 10.0, vertical: true, length: 40.0 });
        // Title, a half line, then four characters per column
        let line = |text: &str| reader.line_at_offset(&layout, reader.text.find(text).unwrap());
        assert_eq!(line("五六"), 3);
        assert_eq!(reader.sentence_start(&layout, line("五六")), 0);
        assert_eq!(reader.sentence_start(&layout, line("九十")), reader.text.find("八").unwrap());
        assert_eq!(reader.sentence_start(&layout, line("次")), reader.text.find("次").unwrap());
        assert_eq!(reader.sentence_start(&layout, 0), 0);
        assert_eq!(reader.line_at_offset(&layout, 0), 0);
    }

    #[test]
    fn pages_hold_whole_lines() {
        let reader = Reader::from_text(0, "Book", "T", &"あいうえおかきくけこ\n".repeat(5), false, 1);
        let layout = reader.lay_out(LayoutKey { size: 10.0, vertical: true, length: 40.0 });
        // Every line but the half one after the title is 17.5 thick
        let room = 60.0;
        assert_eq!(layout.page_end(2, room, 0.0), 5);
        // Turning back shows the pages turning forward showed, from the chapter's start
        let mut pages = vec![0];
        while let Some(next) = layout.next_page(*pages.last().unwrap(), room, 0.0) {
            pages.push(next);
        }
        assert!(pages.len() > 2);
        for pair in pages.windows(2) {
            assert_eq!(layout.page_before(pair[1], room, 0.0), pair[0]);
        }
        // From a line that is not a page's start: the page it is in
        let inside = pages[2] - 1;
        assert!(inside > pages[1]);
        assert_eq!(layout.page_before(inside, room, 0.0), pages[1]);
        // The room for the next chapter button is the last item, so the last page ends with it
        let with_end = layout.items(30.0);
        assert_eq!(with_end, layout.lines.len() + 1);
        assert_eq!(layout.page_end(with_end - 1, room, 30.0), with_end);
        let last = layout.page_before(with_end, room, 30.0);
        assert_eq!(layout.page_end(last, room, 30.0), with_end);
    }

    #[test]
    fn a_word_cut_by_a_line_end_is_square_at_the_cut() {
        let reader = Reader::from_text(0, "Book", "T", "あいうえおかきくけこ", false, 1);
        let r = 3;
        for vertical in [true, false] {
            // Four characters per line
            let layout = reader.lay_out(LayoutKey { size: 10.0, vertical, length: 40.0 });
            let origin = pos2(500.0, 0.0);
            let all = 0..layout.lines.len();
            let pieces = layout.rects(0, &(2..6), origin, all.clone());
            let cut: Vec<_> = pieces.iter().map(|p| (p.chars.clone(), p.start, p.end)).collect();
            assert_eq!(cut, [(2..4, true, false), (4..6, false, true)]);
            let [first, second] = [0, 1].map(|i| word_corners(pieces[i].start, pieces[i].end, vertical, r));
            // The cells the pieces cover, along the line: whole cells, from the cut to the word's end
            let along = |rect: Rect| if vertical { (rect.top() - origin.y, rect.bottom() - origin.y) } else { (rect.left() - origin.x, rect.right() - origin.x) };
            assert_eq!([along(pieces[0].rect), along(pieces[1].rect)], [(20.0, 40.0), (0.0, 20.0)]);
            if vertical {
                // Rounded at the word's top, square at the bottom of the column where it is cut, and the other way round
                // at the top of the next column
                assert_eq!(first, CornerRadius { nw: r, ne: r, sw: 0, se: 0 });
                assert_eq!(second, CornerRadius { nw: 0, ne: 0, sw: r, se: r });
            } else {
                assert_eq!(first, CornerRadius { nw: r, sw: r, ne: 0, se: 0 });
                assert_eq!(second, CornerRadius { nw: 0, sw: 0, ne: r, se: r });
            }
            // A word on one line is rounded all around, and meets the word before it with no gap
            let whole = layout.rects(0, &(6..8), origin, all.clone());
            assert_eq!(whole.len(), 1);
            assert_eq!(word_corners(whole[0].start, whole[0].end, vertical, r), CornerRadius::same(r));
            assert_eq!(along(whole[0].rect).0, along(pieces[1].rect).1);
            // One on a line not shown has no piece there
            let first_line = layout.paragraph_lines[0].start;
            assert_eq!(layout.rects(0, &(2..6), origin, first_line..first_line + 1).len(), 1);
        }
    }

    fn parsed(base: &str, pos: &str, span: Range<usize>, kind: WordKind) -> ParsedWord {
        ParsedWord {
            base_word: base.to_string(),
            spelling: base.to_string(),
            pronunciation: String::new(),
            pos: pos.to_string(),
            span,
            kind,
            affixes: Vec::new(),
            forms: Vec::new(),
            homograph: None,
            written_reading: None,
            proper_noun: false,
        }
    }

    #[test]
    fn words_are_colored_by_their_parts() {
        use Part::*;
        use WordKind::{Fragment, Grammar as G, Number, Word as W};
        let parts = |word: &ParsedWord, text: &str| word_parts(word, &text[word.span.clone()]);

        // 数えられた: the verb, then two endings
        let mut word = parsed("数える", "動詞", 0..15, W);
        word.affixes = vec![parsed("られる", "助動詞", 6..12, G), parsed("た", "助動詞", 12..15, G)];
        assert_eq!(parts(&word, "数えられた"), [(0..2, Verb), (2..4, Ending), (4..5, Ending)]);

        // お二人: the polite prefix, then the counter with its number
        let mut word = parsed("人", "接尾辞", 0..9, W);
        word.written_reading = Some("おふたり".to_string());
        word.affixes = vec![parsed("御", "接頭辞", 0..3, G)];
        assert_eq!(parts(&word, "お二人"), [(0..1, Prefix), (1..3, Counter)]);

        // 静かさ, 使いづらい: suffixes; 美しくない: an ending
        let mut word = parsed("静か", "形状詞", 0..9, W);
        word.affixes = vec![parsed("さ", "接尾辞", 6..9, G)];
        assert_eq!(parts(&word, "静かさ"), [(0..2, Adjective), (2..3, Suffix)]);
        let mut word = parsed("使う", "動詞", 0..15, W);
        word.affixes = vec![parsed("辛い", "形容詞", 6..15, G)];
        assert_eq!(parts(&word, "使いづらい"), [(0..2, Verb), (2..5, Suffix)]);
        let mut word = parsed("美しい", "形容詞", 0..15, W);
        word.affixes = vec![parsed("無い", "形容詞", 9..15, G)];
        assert_eq!(parts(&word, "美しくない"), [(0..3, Adjective), (3..5, Ending)]);

        // Grammar is one quiet part, its endings included (だった); numbers and fragments are grey
        let mut word = parsed("だ", "助動詞", 0..9, G);
        word.affixes = vec![parsed("た", "助動詞", 6..9, G)];
        assert_eq!(parts(&word, "だった"), [(0..3, Grammar)]);
        assert_eq!(parts(&parsed("二十三", "名詞", 0..9, Number), "二十三"), [(0..3, Faint)]);
        assert_eq!(parts(&parsed("あ", "感動詞", 0..3, Fragment), "あ"), [(0..1, Faint)]);

        // Stems by part of speech
        let stem = |pos: &str| Part::of_stem(&parsed("語", pos, 0..3, W));
        assert_eq!(["名詞", "副詞", "代名詞", "接尾辞", "連体詞"].map(stem), [Noun, Adverb, Pronoun, Suffix, Other]);
        assert_eq!(Part::of_stem(&ParsedWord { proper_noun: true, ..parsed("東京", "名詞", 0..6, W) }), ProperNoun);
    }

    /// One frame of the reader, paginated, in a 400 × 300 window.
    fn frame(ctx: &egui::Context, reader: &mut Reader, vertical: bool, time: f64, events: Vec<Event>) -> ReaderResponse {
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0));
        let input = RawInput { time: Some(time), screen_rect: Some(screen), events, ..Default::default() };
        let mut response = ReaderResponse::default();
        let mut output = ctx.run_ui(input, |ui| {
            let mut size = 20.0;
            response = reader.ui(ui, None, &mut size, vertical, true, false, Palette::of(ui.visuals()));
        });
        output.textures_delta.clear();
        response
    }

    /// Press at `from`, move by `dx` in steps, release; returns the time after it, and the chapter asked for.
    /// The page must have turned, if it does, before the pointer is released.
    fn drag(ctx: &egui::Context, reader: &mut Reader, vertical: bool, mut time: f64, from: Pos2, dx: f32) -> (f64, ReaderResponse) {
        let button = |pos, pressed| Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        frame(ctx, reader, vertical, time, vec![Event::PointerMoved(from), button(from, true)]);
        let mut asked = ReaderResponse::default();
        for step in 1..=5 {
            time += 0.02;
            let response = frame(ctx, reader, vertical, time, vec![Event::PointerMoved(from + vec2(dx * step as f32 / 5.0, 0.0))]);
            if response.chapter.is_some() {
                asked = response;
            }
        }
        let before_release = reader.page;
        time += 0.02;
        let response = frame(ctx, reader, vertical, time, vec![button(from + vec2(dx, 0.0), false)]);
        assert_eq!(reader.page, before_release);
        assert!(response.chapter.is_none() || asked.chapter.is_none(), "turned twice");
        (time + 0.02, if response.chapter.is_some() { response } else { asked })
    }

    fn key(key: egui::Key) -> Vec<Event> {
        vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }]
    }

    #[test]
    fn dragging_and_arrow_keys_turn_pages_in_the_reading_direction() {
        let text = "これは長い文の一つです。".repeat(3) + "\n";
        for vertical in [true, false] {
            let ctx = egui::Context::default();
            let mut reader = Reader::from_text(0, "Book", "Title", &text.repeat(40), false, 2);
            reader.chapter = 1;
            let mut time = 1.0;
            frame(&ctx, &mut reader, vertical, time, vec![]);
            assert_eq!(reader.page, 0);
            let next = if vertical { 120.0 } else { -120.0 };
            // A drag in the reading direction turns to the next page, and saves the place
            let (t, _) = drag(&ctx, &mut reader, vertical, time, pos2(200.0, 150.0), next);
            time = t;
            let second = reader.page;
            assert!(second > 0, "vertical {vertical}");
            assert!(reader.settled_place(&ctx).is_some_and(|p| p.offset > 0 && p.chapter == 1));
            // A short drag, or a click, does not
            let (t, _) = drag(&ctx, &mut reader, vertical, time, pos2(200.0, 150.0), next / 10.0);
            time = t;
            assert_eq!(reader.page, second);
            // Back
            let (t, _) = drag(&ctx, &mut reader, vertical, time, pos2(200.0, 150.0), -next);
            time = t;
            assert_eq!(reader.page, 0);
            // Before the first page: the previous chapter's last page
            let (t, response) = drag(&ctx, &mut reader, vertical, time, pos2(200.0, 150.0), -next);
            time = t;
            assert_eq!((response.chapter, response.at_end), (Some(0), true));
            // Arrow keys: ← is the next page in vertical text, → in horizontal text
            let forward = if vertical { egui::Key::ArrowLeft } else { egui::Key::ArrowRight };
            let backward = if vertical { egui::Key::ArrowRight } else { egui::Key::ArrowLeft };
            frame(&ctx, &mut reader, vertical, time, key(forward));
            frame(&ctx, &mut reader, vertical, time + 0.02, vec![]);
            assert_eq!(reader.page, second);
            frame(&ctx, &mut reader, vertical, time + 0.04, key(backward));
            assert_eq!(reader.page, 0);
            // The wheel does nothing
            let wheel = Event::MouseWheel { unit: MouseWheelUnit::Line, delta: vec2(0.0, -5.0), phase: TouchPhase::Move, source: egui::MouseWheelSource::Wheel, modifiers: Modifiers::NONE };
            frame(&ctx, &mut reader, vertical, time + 0.06, vec![Event::PointerMoved(pos2(200.0, 150.0)), wheel]);
            frame(&ctx, &mut reader, vertical, time + 0.2, vec![]);
            assert_eq!(reader.page, 0);
        }
    }

    #[test]
    fn a_drag_shows_the_page_its_net_movement_points_to() {
        let threshold = 30.0;
        // Past the threshold either way while no other page is fixed, and back a little within it before the page
        // shown changes back
        assert_eq!(drag_page(0, 0, 29.0, threshold), 0);
        assert_eq!(drag_page(0, 0, 30.0, threshold), 1);
        assert_eq!(drag_page(0, 0, -30.0, threshold), -1);
        assert_eq!(drag_page(1, 1, 25.0, threshold), 1);
        assert_eq!(drag_page(1, 1, 20.0, threshold), 0);
        assert_eq!(drag_page(-1, -1, -25.0, threshold), -1);
        // Once the other page is fixed, the opposite way shows the page the drag started on
        assert_eq!(drag_page(1, 1, -40.0, threshold), 0);
        assert_eq!(drag_page(0, 1, -400.0, threshold), 0);
        assert_eq!(drag_page(0, -1, 400.0, threshold), 0);
        assert_eq!(drag_page(0, -1, -30.0, threshold), -1);

        let button = |pos, pressed| Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        let text = ("これは長い文の一つです。".repeat(3) + "\n").repeat(40);
        for vertical in [true, false] {
            let ctx = egui::Context::default();
            let mut reader = Reader::from_text(0, "Book", "Title", &text, false, 2);
            reader.chapter = 1;
            let mut time = 1.0;
            let mut step = |reader: &mut Reader, events| {
                time += 0.02;
                frame(&ctx, reader, vertical, time, events)
            };
            step(&mut reader, vec![]);
            step(&mut reader, key(if vertical { egui::Key::ArrowLeft } else { egui::Key::ArrowRight }));
            let (start, anchor) = (reader.page, reader.anchor);
            assert!(start > 0);
            reader.settled_place(&ctx);
            // Held down: forward shows the next page, back the page it started on; that next page is the only other
            // page of this drag, so far back stays on the page it started on, and far forward never goes further
            let next = if vertical { 1.0 } else { -1.0 };
            let at = |x: f32| pos2(200.0 + next * x, 150.0);
            step(&mut reader, vec![Event::PointerMoved(at(0.0)), button(at(0.0), true)]);
            let mut shown = Vec::new();
            for x in [10.0, 40.0, 80.0, 40.0, 10.0, -40.0, -80.0, -150.0, 0.0, 80.0, 190.0] {
                let response = step(&mut reader, vec![Event::PointerMoved(at(x))]);
                assert!(response.chapter.is_none());
                shown.push(reader.page);
            }
            let forward = shown[2];
            assert!(forward > start, "vertical {vertical}: {shown:?}");
            assert_eq!(shown, [start, forward, forward, forward, start, start, start, start, start, forward, forward]);
            // Nothing saved while held, and the page it started on kept its place
            assert!(reader.settled_place(&ctx).is_none());
            step(&mut reader, vec![Event::PointerMoved(at(10.0))]);
            assert_eq!((reader.page, reader.anchor), (start, anchor));
            // Released: the page shown stays, and is saved
            step(&mut reader, vec![Event::PointerMoved(at(80.0))]);
            step(&mut reader, vec![button(at(80.0), false)]);
            assert_eq!(reader.page, forward);
            assert!(reader.settled_place(&ctx).is_some_and(|p| p.offset > anchor));

            // From the chapter's first page, back into the previous chapter, and back again
            for _ in 0..2 {
                step(&mut reader, key(if vertical { egui::Key::ArrowRight } else { egui::Key::ArrowLeft }));
            }
            assert_eq!(reader.page, 0);
            step(&mut reader, vec![Event::PointerMoved(at(0.0)), button(at(0.0), true)]);
            step(&mut reader, vec![Event::PointerMoved(at(10.0))]);
            let response = step(&mut reader, vec![Event::PointerMoved(at(-80.0))]);
            assert_eq!((response.chapter, response.at_end), (Some(0), true));
            // The app opens the previous chapter at its end, and the drag goes on there
            let mut previous = Reader::from_text(0, "Book", "Previous", &text, false, 2);
            previous.go_to_end();
            previous.continue_drag(response.drag.unwrap());
            step(&mut previous, vec![Event::PointerMoved(at(-85.0))]);
            assert!(previous.page > 0);
            let response = step(&mut previous, vec![Event::PointerMoved(at(0.0))]);
            assert_eq!((response.chapter, response.at_end), (Some(1), false));
            // Back in the chapter it started in, on its first page, which a release keeps: far forward too, since
            // the previous page is this drag's other page
            let mut back = Reader::from_text(0, "Book", "Title", &text, false, 2);
            back.chapter = 1;
            back.continue_drag(response.drag.unwrap());
            for x in [5.0, 80.0, 150.0, 5.0] {
                let response = step(&mut back, vec![Event::PointerMoved(at(x))]);
                assert!(response.chapter.is_none());
                assert_eq!(back.page, 0);
            }
            step(&mut back, vec![button(at(5.0), false)]);
            assert_eq!(back.page, 0);
            assert!(back.drag.is_none());
        }
    }

    #[test]
    fn the_page_keeps_its_place_when_laid_out_again() {
        let text = ("これは長い文の一つです。".repeat(3) + "\n").repeat(40);
        let ctx = egui::Context::default();
        let mut reader = Reader::from_text(0, "Book", "Title", &text, false, 1);
        frame(&ctx, &mut reader, true, 1.0, vec![]);
        for (i, t) in (0..3).zip([1.1, 1.2, 1.3]) {
            frame(&ctx, &mut reader, true, t, key(egui::Key::ArrowLeft));
            assert!(reader.page > i);
        }
        let anchor = reader.anchor;
        let layout = reader.layout.as_ref().unwrap();
        let line = reader.line_at_offset(layout, anchor);
        assert!(line <= reader.page);
        // Horizontal text now: the page starts with the anchor's line, and the anchor stays
        frame(&ctx, &mut reader, false, 1.4, vec![]);
        assert_eq!(reader.anchor, anchor);
        assert_eq!(reader.page, reader.line_at_offset(reader.layout.as_ref().unwrap(), anchor));
        // Scrolling, and back
        frame(&ctx, &mut reader, true, 1.5, vec![]);
        assert_eq!(reader.anchor, anchor);
        let mut size = 20.0;
        let input = RawInput { time: Some(1.6), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0))), ..Default::default() };
        let mut output = ctx.run_ui(input, |ui| {
            reader.ui(ui, None, &mut size, true, false, false, Palette::of(ui.visuals()));
        });
        output.textures_delta.clear();
        assert_eq!(reader.anchor, anchor);
        frame(&ctx, &mut reader, true, 1.7, vec![]);
        assert_eq!(reader.anchor, anchor);
    }

    #[test]
    fn shift_wheel_and_pinch_change_the_font_size() {
        assert_eq!(zoomed(vec![wheel(Modifiers::NONE)]), 20.0);
        assert!((zoomed(vec![wheel(Modifiers::SHIFT)]) - 20.0 * WHEEL_ZOOM * WHEEL_ZOOM).abs() < 1e-3);
        assert!((zoomed(vec![Event::Zoom(1.5)]) - 30.0).abs() < 1e-3);
        assert_eq!(zoomed(vec![Event::Zoom(10.0)]), *FONT_SIZES.end());
    }
}
