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
    let lines: Vec<&Line> = lines.iter().filter(|l| !is_ruby(l)).collect();
    let mut chapters = Vec::new();
    // Open chapter; None for front matter, which is dropped
    let mut current: Option<OpenChapter> = None;
    let mut found_heading = false;

    for &line in &lines {
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
            chapters.extend(current.take().and_then(OpenChapter::finish));
            if !FRONT_MATTER.contains(&title) {
                current = Some(OpenChapter { title: title.to_string(), text: String::new(), has_body: false });
            }
            continue;
        }
        if let Some(chapter) = &mut current {
            chapter.text.push_str(&line.text);
            chapter.text.push('\n');
            chapter.has_body |= is_body(line) && !trimmed.is_empty();
        }
    }
    chapters.extend(current.and_then(OpenChapter::finish));
    if !found_heading {
        let text: String = lines.iter().map(|l| format!("{}\n", l.text)).collect();
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

struct OpenChapter {
    title: String,
    text: String,
    has_body: bool,
}

impl OpenChapter {
    fn finish(self) -> Option<Chapter> {
        self.has_body.then_some(Chapter { index: 0, title: self.title, text: self.text })
    }
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

    #[test]
    fn keeps_a_book_without_headings_as_one_chapter() {
        let body = style("F5", 1400);
        let chapters = split_chapters(&[line("　本文。", &body)]);
        assert_eq!(titles(&chapters), ["Chapter 01"]);
    }
}
