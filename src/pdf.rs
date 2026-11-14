use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;
use anyhow::{Context, Result};
use lopdf::content::Content;
use lopdf::{Document, Object};
#[cfg(feature = "parallel")]
use rayon::prelude::*;
use regex::Regex;

use crate::models::Chapter;

/// Font resource and size a text run is drawn with.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Style {
    font: Vec<u8>,
    /// In hundredths of a point
    size: i64,
    /// Japanese font with a UTF-16 CMap (page numbers and URLs use Latin fonts)
    utf16: bool,
}

/// Text drawn by one show-text operation, which is one line in these PDFs.
struct Line {
    text: String,
    style: Option<Style>,
}

/// Headings whose section is not part of the story.
const FRONT_MATTER: [&str; 2] = ["注意事項", "目次"];

pub fn extract_pdf_chapters(path: &Path) -> Result<Vec<Chapter>> {
    let doc = Document::load(path).with_context(|| format!("Failed to load PDF file: {:?}", path))?;
    Ok(extract_chapters(&doc))
}

/// Same as [`extract_pdf_chapters`], from the file's contents (for the web viewer).
pub fn extract_pdf_chapters_from_bytes(bytes: &[u8]) -> Result<Vec<Chapter>> {
    Ok(extract_chapters(&Document::load_mem(bytes).context("Failed to load PDF file")?))
}

fn extract_chapters(doc: &Document) -> Vec<Chapter> {
    // Pages are keyed 1..=n in order
    let page_ids: Vec<_> = doc.get_pages().into_values().collect();
    #[cfg(feature = "parallel")]
    let page_ids = page_ids.par_iter();
    #[cfg(not(feature = "parallel"))]
    let page_ids = page_ids.iter();
    let pages: Vec<Vec<Line>> = page_ids.map(|&id| extract_page_lines(doc, id).unwrap_or_default()).collect();
    let lines: Vec<Line> = pages.into_iter().flatten().collect();
    split_chapters(&lines)
}

/// Split the book at headings, which may appear anywhere on a page.
fn split_chapters(lines: &[Line]) -> Vec<Chapter> {
    let (body, heading_styles) = classify_styles(lines);
    let is_heading = |line: &Line| {
        let text = line.text.trim();
        let short = !text.is_empty() && text.chars().count() <= 50 && !text.ends_with(['。', '、']);
        if heading_styles.is_empty() {
            short && RE_HEADING.is_match(text)
        } else {
            short && line.style.as_ref().is_some_and(|s| heading_styles.contains(s))
        }
    };
    // A section without body text (such as the cover page) is not a chapter
    let is_body = |line: &Line| body.is_none() || line.style.as_ref() == body;
    // Furigana (ruby) is drawn at about half the body size; drop it, like <rt> in EPUBs
    let is_ruby = |line: &Line| matches!((&line.style, body), (Some(s), Some(b)) if s.size * 10 <= b.size * 6);
    // Each line remembers whether furigana was dropped just before it, which may have cut a drawn line in two
    let mut after_ruby = false;
    let lines: Vec<(&Line, bool)> = lines
        .iter()
        .filter_map(|l| {
            let ruby = is_ruby(l);
            let kept = (!ruby).then_some((l, after_ruby));
            after_ruby = ruby;
            kept
        })
        .collect();
    let full = full_line_length(lines.iter().map(|(l, _)| l.text.as_str()));
    let mut chapters = Vec::new();
    // Open chapter; None for front matter, which is dropped
    let mut current: Option<OpenChapter> = None;
    let mut found_heading = false;

    for &(line, after_ruby) in &lines {
        let trimmed = line.text.trim();
        if is_heading(line) {
            found_heading = true;
            // Narou PDFs give an episode's author foreword and afterword their own headings
            if trimmed.ends_with("（後書き）") {
                continue;
            }
            let title = trimmed.strip_suffix("（前書き）").unwrap_or(trimmed);
            if current.as_ref().is_some_and(|c| c.title == title) {
                continue;
            }
            chapters.extend(current.take().and_then(|c| c.finish(full)));
            if !FRONT_MATTER.contains(&title) {
                current = Some(OpenChapter { title: title.to_string(), lines: Vec::new(), has_body: false });
            }
            continue;
        }
        if let Some(chapter) = &mut current {
            chapter.lines.push(DrawnLine { text: &line.text, after_ruby });
            chapter.has_body |= is_body(line) && !trimmed.is_empty();
        }
    }
    chapters.extend(current.and_then(|c| c.finish(full)));
    if !found_heading {
        let drawn: Vec<DrawnLine> = lines.iter().map(|&(l, after_ruby)| DrawnLine { text: &l.text, after_ruby }).collect();
        let text = join_lines(&drawn, full);
        if !text.trim().is_empty() {
            chapters.push(Chapter { index: 0, title: "Chapter 01".to_string(), text });
        }
    }

    crate::models::adjust_chapter_indices(&mut chapters);
    chapters
}

/// Chapter heading names, used when the PDF has no distinct heading font.
static RE_HEADING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:プロローグ|エピローグ|序章|終章|最終話|間章|幕間|間話|閑話|番外編?|あとがき|第[0-9０-９一二三四五六七八九十百千]+[話章部幕])(?:$|[\s　]*[「『【（(:：]|[\s　]+\S)",
    )
    .unwrap()
});

/// The body text style (the one with the most characters) and the heading styles: Japanese fonts
/// other than the body's (for example a bold font) at the body size or larger. Without heading
/// styles, headings are recognized by name instead.
fn classify_styles(lines: &[Line]) -> (Option<&Style>, HashSet<&Style>) {
    let mut chars_per_style: HashMap<&Style, usize> = HashMap::new();
    for line in lines {
        if let Some(style) = &line.style {
            *chars_per_style.entry(style).or_default() += line.text.chars().count();
        }
    }
    let body = chars_per_style.iter().max_by_key(|&(_, &chars)| chars).map(|(&style, _)| style);
    let headings = chars_per_style
        .into_keys()
        .filter(|s| body.is_some_and(|b| *s != b && s.utf16 && s.size >= b.size))
        .collect();
    (body, headings)
}

struct OpenChapter<'a> {
    title: String,
    lines: Vec<DrawnLine<'a>>,
    has_body: bool,
}

impl OpenChapter<'_> {
    fn finish(self, full: Option<usize>) -> Option<Chapter> {
        self.has_body.then(|| Chapter { index: 0, title: self.title, text: join_lines(&self.lines, full) })
    }
}

/// A chapter's line as the PDF draws it.
struct DrawnLine<'a> {
    text: &'a str,
    /// Furigana was dropped just before it
    after_ruby: bool,
}

/// Characters that end a sentence: a line ending with one is not continued on the next line.
const SENTENCE_ENDS: [char; 10] = ['。', '」', '』', '！', '？', '）', '】', '!', '?', '…'];

/// Page numbers are drawn in a Latin font at the bottom of each page.
fn is_page_number(text: &str) -> bool {
    let text = text.trim();
    (1..=4).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_digit())
}

/// Number of characters of a full line, when the book's text is cut at one width (as it usually is).
fn full_line_length<'a>(lines: impl Iterator<Item = &'a str>) -> Option<usize> {
    let mut counts: HashMap<usize, usize> = HashMap::new();
    for line in lines {
        let length = line.trim_end().chars().count();
        if length >= 10 {
            *counts.entry(length).or_default() += 1;
        }
    }
    let (&length, &count) = counts.iter().max_by_key(|&(&length, &count)| (count, length))?;
    (length >= 15 && count >= 20).then_some(length)
}

/// Lines joined back into paragraphs, one per line of the result. The PDF cuts paragraphs at the line width
/// and pages (which would also split words such as 興味 for the tokenizer): a line continues the previous one
/// when that one was full and did not end a sentence, unless it is indented (a new paragraph), or when it is
/// the rest of a line cut in two by furigana. Page numbers stay on lines of their own, after the paragraph
/// where the page ends.
fn join_lines(lines: &[DrawnLine], full: Option<usize>) -> String {
    let mut out = String::new();
    let mut paragraph: Option<String> = None;
    // Characters of the paragraph's last drawn line
    let mut last_length = 0;
    // Page numbers met inside the paragraph
    let mut pages: Vec<&str> = Vec::new();
    let flush = |out: &mut String, paragraph: &mut Option<String>, pages: &mut Vec<&str>| {
        if let Some(text) = paragraph.take() {
            out.push_str(&text);
            out.push('\n');
        }
        for page in pages.drain(..) {
            out.push_str(page);
            out.push('\n');
        }
    };
    for (i, line) in lines.iter().enumerate() {
        let text = line.text.trim_end();
        // A page ends with an empty line, then its number
        if text.is_empty() && lines.get(i + 1).is_some_and(|l| is_page_number(l.text)) {
            continue;
        }
        if is_page_number(text) {
            if paragraph.is_some() {
                pages.push(text.trim());
            } else {
                out.push_str(text.trim());
                out.push('\n');
            }
            continue;
        }
        let length = text.chars().count();
        let (same_line, next_line) = match (&paragraph, full) {
            (Some(p), Some(full)) if !text.is_empty() && !text.starts_with('　') => (
                line.after_ruby && last_length + length <= full + 1,
                last_length + 2 >= full && !p.ends_with(SENTENCE_ENDS),
            ),
            _ => (false, false),
        };
        match &mut paragraph {
            Some(p) if same_line || next_line => {
                p.push_str(text);
                last_length = if same_line { last_length + length } else { length };
            }
            _ => {
                flush(&mut out, &mut paragraph, &mut pages);
                if text.is_empty() {
                    out.push('\n');
                } else {
                    paragraph = Some(text.to_string());
                    last_length = length;
                }
            }
        }
    }
    flush(&mut out, &mut paragraph, &mut pages);
    out
}

fn extract_page_lines(doc: &Document, page_id: (u32, u16)) -> Result<Vec<Line>> {
    let content = Content::decode(&doc.get_page_content(page_id))?;
    // Fonts whose CMap encodes text as UTF-16BE (e.g. UniJIS-UTF16-H, UniJIS-UCS2-H)
    let utf16_fonts: HashSet<Vec<u8>> = doc
        .get_page_fonts(page_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, font)| {
            font.get(b"Encoding").and_then(Object::as_name).is_ok_and(|enc| {
                let enc = String::from_utf8_lossy(enc);
                enc.contains("UTF16") || enc.contains("UCS2")
            })
        })
        .map(|(name, _)| name)
        .collect();

    let mut lines = Vec::new();
    let mut style: Option<Style> = None;
    for op in content.operations {
        match op.operator.as_str() {
            "Tf" => {
                if let [Object::Name(font), size, ..] = op.operands.as_slice() {
                    style = Some(Style {
                        utf16: utf16_fonts.contains(font),
                        font: font.clone(),
                        size: size.as_float().map_or(0, |s| (s * 100.0).round() as i64),
                    });
                }
            }
            "Tj" | "TJ" | "'" | "\"" => {
                let utf16 = style.as_ref().is_none_or(|s| s.utf16);
                let mut text = String::new();
                let mut any = false;
                for operand in &op.operands {
                    let items = match operand {
                        Object::Array(arr) => arr.as_slice(),
                        other => std::slice::from_ref(other),
                    };
                    for item in items {
                        if let Object::String(bytes, _) = item {
                            any = true;
                            decode_pdf_string(bytes, utf16, &mut text);
                        }
                    }
                }
                if any {
                    lines.push(Line { text, style: style.clone() });
                }
            }
            _ => {}
        }
    }
    Ok(lines)
}

fn decode_pdf_string(bytes: &[u8], utf16: bool, out: &mut String) {
    if utf16 && bytes.len().is_multiple_of(2) {
        let units = bytes.as_chunks::<2>().0.iter().map(|&c| u16::from_be_bytes(c));
        out.extend(char::decode_utf16(units).map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)));
    } else {
        out.push_str(&String::from_utf8_lossy(bytes));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(font: &str, size: i64) -> Option<Style> {
        Some(Style { font: font.as_bytes().to_vec(), size, utf16: true })
    }

    fn line(text: &str, style: &Option<Style>) -> Line {
        Line { text: text.to_string(), style: style.clone() }
    }

    fn titles(chapters: &[Chapter]) -> Vec<&str> {
        chapters.iter().map(|c| c.title.as_str()).collect()
    }

    #[test]
    fn splits_on_heading_font_anywhere_on_a_page() {
        let (body, bold) = (style("F5", 1400), style("F6", 1400));
        let lines = [
            line("注意事項", &bold),
            line("　このＰＤＦファイルは…", &body),
            line("１", &bold),
            line("　本文。", &body),
            line("　まだ本文。", &body),
            line("２", &bold), // mid-page
            line("　次の話。", &body),
            line("地図（前書き）", &bold),
            line("　前書き。", &body),
            line("地図", &bold),
            line("　地図の話。", &body),
            line("地図（後書き）", &bold),
            line("　後書き。", &body),
        ];
        let chapters = split_chapters(&lines);
        assert_eq!(titles(&chapters), ["１", "２", "地図"]);
        assert_eq!(chapters[2].text, "　前書き。\n　地図の話。\n　後書き。\n");
        // A cover page has the title in a large font but no body text
        let cover = [line("書名", &style("F2", 2000)), line("作者", &None), line("１", &bold), line("　本文。", &body)];
        assert_eq!(titles(&split_chapters(&cover)), ["１"]);
    }

    #[test]
    fn drops_furigana() {
        let (body, ruby, bold) = (style("F5", 1400), style("F5", 700), style("F6", 1400));
        let lines = [line("１", &bold), line("　《落", &body), line("ポーン", &ruby), line("とし子》", &body)];
        assert_eq!(split_chapters(&lines)[0].text, "　《落\nとし子》\n");
        let single_font = [line("　本文、", &body), line("ほんぶん", &ruby), line("続き。", &body)];
        assert_eq!(split_chapters(&single_font)[0].text, "　本文、\n続き。\n");
    }

    #[test]
    fn falls_back_to_heading_names_with_a_single_font() {
        let body = style("F5", 1400);
        let lines = [
            line("プロローグ", &body),
            line("　閑話休題。", &body),
            line("第一話　「始まり」", &body),
            line("　本文。", &body),
            line("最終話「終わり」", &body),
            line("　おしまい。", &body),
        ];
        assert_eq!(titles(&split_chapters(&lines)), ["プロローグ", "第一話　「始まり」", "最終話「終わり」"]);
    }

    fn joined(lines: &[(&str, bool)], full: usize) -> String {
        let lines: Vec<DrawnLine> = lines.iter().map(|&(text, after_ruby)| DrawnLine { text, after_ruby }).collect();
        join_lines(&lines, Some(full))
    }

    #[test]
    fn joins_lines_cut_at_the_line_width_into_paragraphs() {
        let lines = [
            ("　魔術に関する記述は実に興", false), // full (12 characters)
            ("味深いものである。", false),
            ("　読み進めていくと、わかった。", false), // full, but ends a sentence
            ("「そういう問題じゃねぇ」", false),
            ("１．まず、魔術は三種類しか", false),
            ("　ないらしい。", false), // indented: a new paragraph
        ];
        assert_eq!(
            joined(&lines, 12),
            "　魔術に関する記述は実に興味深いものである。\n　読み進めていくと、わかった。\n「そういう問題じゃねぇ」\n１．まず、魔術は三種類しか\n　ないらしい。\n"
        );
    }

    #[test]
    fn keeps_page_numbers_after_the_paragraph_across_a_page() {
        let lines = [("　骸骨は壁に背を預けて興味なさげなそ", false), ("", false), ("38", false), ("ぶりだ。", false), ("", false), ("　次。", false)];
        assert_eq!(joined(&lines, 18), "　骸骨は壁に背を預けて興味なさげなそぶりだ。\n38\n\n　次。\n");
        let lines = [("　一頁目。", false), ("", false), ("39", false), ("　二頁目。", false)];
        assert_eq!(joined(&lines, 18), "　一頁目。\n39\n　二頁目。\n");
    }

    #[test]
    fn joins_a_line_cut_in_two_by_furigana() {
        let lines = [("　《落", false), ("とし子》が来た。", true), ("　本文、", false), ("続き。", true)];
        assert_eq!(joined(&lines, 10), "　《落とし子》が来た。\n　本文、続き。\n");
        // Too long for one line: the furigana ended a line, and the next line is a new paragraph
        let lines = [("　本文、", false), ("「続きの文章です」", true)];
        assert_eq!(joined(&lines, 10), "　本文、\n「続きの文章です」\n");
    }

    #[test]
    fn finds_the_full_line_length() {
        let mut lines = vec!["　あいうえおかきくけこさしすせそたち"; 25]; // 18 characters
        lines.extend(["短い。"; 40]);
        assert_eq!(full_line_length(lines.iter().copied()), Some(18));
        assert_eq!(full_line_length(["　本文。"; 50].into_iter()), None);
    }

    #[test]
    fn keeps_a_book_without_headings_as_one_chapter() {
        let body = style("F5", 1400);
        let chapters = split_chapters(&[line("　本文。", &body)]);
        assert_eq!(titles(&chapters), ["Chapter 01"]);
    }
}
