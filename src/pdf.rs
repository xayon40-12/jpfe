use std::path::Path;
use anyhow::{Context, Result};
use lopdf::content::Content;
use lopdf::{Document, Object};
use rayon::prelude::*;
use regex::Regex;

use crate::models::Chapter;

pub fn extract_pdf_chapters(path: &Path) -> Result<Vec<Chapter>> {
    let doc = Document::load(path)
        .with_context(|| format!("Failed to load PDF file: {:?}", path))?;
    
    let pages = doc.get_pages();
    let total_pages = pages.len();
    if total_pages == 0 {
        return Ok(Vec::new());
    }

    // Extract pages in order (1..=total_pages)
    // Use parallel mapping for efficiency across multiple cores
    let page_numbers: Vec<u32> = (1..=total_pages as u32).collect();
    let page_texts: Vec<(u32, String)> = page_numbers
        .par_iter()
        .map(|&p_num| {
            if let Some(&page_id) = pages.get(&p_num) {
                let text = extract_text_from_page(&doc, page_id).unwrap_or_default();
                (p_num, text)
            } else {
                (p_num, String::new())
            }
        })
        .collect();

    // Regex to match Japanese chapter headings at the start of a page
    let re_chapter = Regex::new(
        r"^(プロローグ|エピローグ|終章|序章|間話[「『].*?[」』]|第[0-9一二三四五六七八九十百千]+[話章][「『].*?[」』]|第[0-9一二三四五六七八九十百千]+[話章])"
    ).unwrap();

    let mut chapters: Vec<Chapter> = Vec::new();
    let mut current_title = String::new();
    let mut current_text = String::new();
    let mut chapter_idx = 0;

    for (_p_num, page_content) in page_texts {
        let lines: Vec<&str> = page_content.lines().collect();
        for (line_idx, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            // In novel PDFs, chapter headings typically appear near the top of the page (within the first 5 lines)
            if line_idx <= 5 && trimmed.chars().count() < 50 && re_chapter.is_match(trimmed) {
                if !current_text.trim().is_empty() {
                    if !current_title.is_empty() {
                        chapter_idx += 1;
                        chapters.push(Chapter {
                            index: chapter_idx,
                            title: current_title.clone(),
                            text: current_text.clone(),
                        });
                    }
                    current_text.clear();
                }
                current_title = trimmed.to_string();
                continue;
            }
            current_text.push_str(line);
            current_text.push('\n');
        }
    }

    if !current_text.trim().is_empty() {
        chapter_idx += 1;
        let title = if current_title.is_empty() {
            format!("Chapter {:02}", chapter_idx)
        } else {
            current_title
        };
        chapters.push(Chapter {
            index: chapter_idx,
            title,
            text: current_text.clone(),
        });
    }

    // If no chapter headers were matched at all, treat each block of pages as chapters or whole book as 1 chapter
    if chapters.is_empty() && !current_text.is_empty() {
        chapters.push(Chapter {
            index: 1,
            title: "Chapter 01".to_string(),
            text: current_text,
        });
    }

    crate::models::adjust_chapter_indices(&mut chapters);

    Ok(chapters)
}

fn extract_text_from_page(doc: &Document, page_id: (u32, u16)) -> Result<String> {
    let content_bytes = doc.get_page_content(page_id)?;
    let content = Content::decode(&content_bytes)?;
    let mut page_text = String::new();
    let mut current_font_is_cjk = true;

    for op in content.operations {
        if op.operator == "Tf" {
            if let Some(font_name) = op.operands.first().and_then(|o| o.as_name().ok()) {
                let name = String::from_utf8_lossy(font_name);
                // Standard ASCII fonts vs CJK (UniJIS-UTF16-H)
                current_font_is_cjk = !name.contains("F9") && !name.contains("Helvetica") && !name.contains("Times");
            }
        } else if op.operator == "Tj" || op.operator == "TJ" || op.operator == "'" || op.operator == "\"" {
            let mut text_parts = Vec::new();
            for operand in &op.operands {
                match operand {
                    Object::String(bytes, _) => {
                        decode_pdf_string(bytes, current_font_is_cjk, &mut text_parts);
                    }
                    Object::Array(arr) => {
                        for item in arr {
                            if let Object::String(bytes, _) = item {
                                decode_pdf_string(bytes, current_font_is_cjk, &mut text_parts);
                            }
                        }
                    }
                    _ => {}
                }
            }
            if !text_parts.is_empty() {
                page_text.push_str(&text_parts.join(""));
                page_text.push('\n');
            }
        }
    }
    Ok(page_text)
}

fn decode_pdf_string(bytes: &[u8], current_font_is_cjk: bool, text_parts: &mut Vec<String>) {
    if current_font_is_cjk && bytes.len() % 2 == 0 {
        let u16_chars: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        text_parts.push(String::from_utf16_lossy(&u16_chars));
    } else {
        text_parts.push(String::from_utf8_lossy(bytes).to_string());
    }
}
