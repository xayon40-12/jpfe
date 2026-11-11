use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use anyhow::{Context, Result};
use epub::doc::{EpubDoc, NavPoint};
use regex::Regex;

use crate::models::Chapter;

pub fn extract_epub_chapters(path: &Path) -> Result<Vec<Chapter>> {
    let mut doc = EpubDoc::new(path).with_context(|| format!("Failed to open EPUB file: {:?}", path))?;

    // TOC titles by content path (fragment stripped); nested entries override their parents
    fn collect(points: &[NavPoint], titles: &mut HashMap<PathBuf, String>) {
        for p in points {
            let src = p.content.to_string_lossy();
            titles.insert(PathBuf::from(src.split('#').next().unwrap_or_default()), p.label.trim().to_string());
            collect(&p.children, titles);
        }
    }
    let mut toc_titles = HashMap::new();
    collect(&doc.toc, &mut toc_titles);

    let spine: Vec<PathBuf> = doc.spine.iter().filter_map(|s| doc.resources.get(&s.idref)).map(|r| r.path.clone()).collect();
    let mut chapters = Vec::new();
    for file in spine {
        // Skip obvious non-content files
        let name = file.to_string_lossy().to_lowercase();
        if name.contains("cover") || name.contains("information") {
            continue;
        }
        let Some(content) = doc.get_resource_str_by_path(&file) else { continue };
        let (html_title, text) = clean_xhtml(&content);
        if text.trim().is_empty() {
            continue;
        }
        let title = toc_titles.get(&file).cloned().or(html_title)
            .unwrap_or_else(|| format!("Chapter {:02}", chapters.len() + 1));
        chapters.push(Chapter { index: 0, title, text });
    }
    crate::models::adjust_chapter_indices(&mut chapters);
    Ok(chapters)
}

macro_rules! re {
    ($name:ident, $pat:literal) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat).unwrap());
    };
}
re!(RE_TITLE, r"(?is)<title\b[^>]*>(.*?)</title>");
re!(RE_HEAD, r"(?is)<head\b[^>]*>.*?</head>");
re!(RE_RT, r"(?is)<rt\b[^>]*>.*?</rt>");
re!(RE_RP, r"(?is)<rp\b[^>]*>.*?</rp>");
re!(RE_BREAK, r"(?i)<(?:br|/p|/div|/h[1-6])\b[^>]*>");
re!(RE_TAG, r"<[^>]+>");
re!(RE_ENTITY, r"&(#[0-9]+|#[xX][0-9a-fA-F]+|nbsp|quot|apos|lt|gt|amp);");

/// Returns the `<title>` (if non-empty) and the plain text, with ruby readings removed.
fn clean_xhtml(xhtml: &str) -> (Option<String>, String) {
    let title = RE_TITLE.captures(xhtml).map(|c| c[1].trim().to_string()).filter(|t| !t.is_empty() && t != "Untitled");
    let text = RE_HEAD.replace_all(xhtml, "");
    // Crucial for Japanese text: drop ruby annotations (<rt>, <rp>) with their contents
    let text = RE_RT.replace_all(&text, "");
    let text = RE_RP.replace_all(&text, "");
    let text = RE_BREAK.replace_all(&text, "\n");
    let text = RE_TAG.replace_all(&text, "");
    // Single pass, so an escaped "&amp;lt;" stays "&lt;"
    let text = RE_ENTITY.replace_all(&text, |c: &regex::Captures| {
        let entity = &c[1];
        let code = match entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X")) {
            Some(hex) => u32::from_str_radix(hex, 16).ok(),
            None => entity.strip_prefix('#').and_then(|dec| dec.parse().ok()),
        };
        match (entity, code) {
            (_, Some(code)) => char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER).to_string(),
            ("nbsp", _) => " ".into(),
            ("quot", _) => "\"".into(),
            ("apos", _) => "'".into(),
            ("lt", _) => "<".into(),
            ("gt", _) => ">".into(),
            _ => "&".into(),
        }
    });
    (title, text.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_named_and_numeric_entities_once() {
        let (_, text) = clean_xhtml("<p>a&#8212;b&#x2014;c&nbsp;&lt;d&gt; &amp;lt; &quot;</p>");
        assert_eq!(text, "a—b—c <d> &lt; \"\n");
    }
}
