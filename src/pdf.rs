use std::path::Path;
use anyhow::{Context, Result};
use lopdf::content::Content;
use lopdf::{Document, Object};
use rayon::prelude::*;
use regex::Regex;

use crate::models::Chapter;

pub fn extract_pdf_chapters(path: &Path) -> Result<Vec<Chapter>> {
    let doc = Document::load(path).with_context(|| format!("Failed to load PDF file: {:?}", path))?;
    // Pages are keyed 1..=n in order; extract them in parallel
    let page_ids: Vec<_> = doc.get_pages().into_values().collect();
    let page_texts: Vec<String> =
        page_ids.par_iter().map(|&id| extract_text_from_page(&doc, id).unwrap_or_default()).collect();

    // Japanese chapter headings at the start of a page
    let re_chapter = Regex::new(
        r"^(プロローグ|エピローグ|終章|序章|間話[「『].*?[」』]|第[0-9一二三四五六七八九十百千]+[話章][「『].*?[」』]|第[0-9一二三四五六七八九十百千]+[話章])"
    ).unwrap();

    let mut chapters = Vec::new();
    let mut title = String::new();
    let mut text = String::new();
    for page in &page_texts {
        for (line_idx, line) in page.lines().enumerate() {
            let trimmed = line.trim();
            // In novel PDFs, chapter headings appear near the top of the page
            if line_idx <= 5 && trimmed.chars().count() < 50 && re_chapter.is_match(trimmed) {
                if !text.trim().is_empty() {
                    // Text before the first heading (front matter) is dropped
                    if !title.is_empty() {
                        chapters.push(Chapter { index: 0, title: std::mem::take(&mut title), text: std::mem::take(&mut text) });
                    }
                    text.clear();
                }
                title = trimmed.to_string();
                continue;
            }
            text.push_str(line);
            text.push('\n');
        }
    }
    if !text.trim().is_empty() {
        if title.is_empty() {
            title = format!("Chapter {:02}", chapters.len() + 1);
        }
        chapters.push(Chapter { index: 0, title, text });
    }

    crate::models::adjust_chapter_indices(&mut chapters);
    Ok(chapters)
}

fn extract_text_from_page(doc: &Document, page_id: (u32, u16)) -> Result<String> {
    let content = Content::decode(&doc.get_page_content(page_id)?)?;
    // Fonts whose CMap encodes text as UTF-16BE (e.g. UniJIS-UTF16-H, UniJIS-UCS2-H)
    let utf16_fonts: Vec<Vec<u8>> = doc
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
    let mut page_text = String::new();
    let mut cjk_font = true;
    for op in content.operations {
        match op.operator.as_str() {
            "Tf" => {
                if let Some(name) = op.operands.first().and_then(|o| o.as_name().ok()) {
                    cjk_font = utf16_fonts.iter().any(|f| f == name);
                }
            }
            "Tj" | "TJ" | "'" | "\"" => {
                let mut any = false;
                for operand in &op.operands {
                    let items = match operand {
                        Object::Array(arr) => arr.as_slice(),
                        other => std::slice::from_ref(other),
                    };
                    for item in items {
                        if let Object::String(bytes, _) = item {
                            any = true;
                            decode_pdf_string(bytes, cjk_font, &mut page_text);
                        }
                    }
                }
                if any {
                    page_text.push('\n');
                }
            }
            _ => {}
        }
    }
    Ok(page_text)
}

fn decode_pdf_string(bytes: &[u8], cjk_font: bool, out: &mut String) {
    if cjk_font && bytes.len().is_multiple_of(2) {
        let units = bytes.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]]));
        out.extend(char::decode_utf16(units).map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)));
    } else {
        out.push_str(&String::from_utf8_lossy(bytes));
    }
}
