use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use anyhow::{Context, Result};
use regex::Regex;
use zip::ZipArchive;

use crate::models::Chapter;

pub fn extract_epub_chapters(path: &Path) -> Result<Vec<Chapter>> {
    let file = File::open(path)
        .with_context(|| format!("Failed to open EPUB file: {:?}", path))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| "Failed to read EPUB as zip archive")?;

    // 1. Locate OPF file via META-INF/container.xml
    let mut container_xml = String::new();
    let opf_path = if let Ok(mut container_file) = archive.by_name("META-INF/container.xml") {
        container_file.read_to_string(&mut container_xml)?;
        let doc = roxmltree::Document::parse(&container_xml)?;
        doc.descendants()
            .find(|n| n.has_tag_name("rootfile"))
            .and_then(|n| n.attribute("full-path"))
            .unwrap_or("OEBPS/content.opf")
            .to_string()
    } else {
        "OEBPS/content.opf".to_string()
    };

    let opf_dir = Path::new(&opf_path).parent().unwrap_or(Path::new(""));

    // 2. Read and parse OPF file
    let mut opf_xml = String::new();
    archive
        .by_name(&opf_path)
        .with_context(|| format!("Failed to find OPF file: {}", opf_path))?
        .read_to_string(&mut opf_xml)?;
    let opf_doc = roxmltree::Document::parse(&opf_xml)?;

    // Manifest: id -> href
    let mut manifest = HashMap::new();
    let mut ncx_href = None;
    for node in opf_doc.descendants().filter(|n| n.has_tag_name("item")) {
        if let (Some(id), Some(href)) = (node.attribute("id"), node.attribute("href")) {
            manifest.insert(id.to_string(), href.to_string());
            if node.attribute("media-type") == Some("application/x-dtbncx+xml") {
                ncx_href = Some(href.to_string());
            }
        }
    }

    // Spine: ordered list of itemref idrefs
    let mut spine_files = Vec::new();
    for node in opf_doc.descendants().filter(|n| n.has_tag_name("itemref")) {
        if let Some(idref) = node.attribute("idref") {
            if let Some(href) = manifest.get(idref) {
                let full_href = if opf_dir.as_os_str().is_empty() {
                    href.clone()
                } else {
                    format!("{}/{}", opf_dir.display(), href)
                };
                spine_files.push((idref.to_string(), href.clone(), full_href));
            }
        }
    }

    // 3. Try reading TOC (toc.ncx) for chapter titles
    let mut toc_titles: HashMap<String, String> = HashMap::new();
    let ncx_full_path = ncx_href.map(|h| {
        if opf_dir.as_os_str().is_empty() {
            h
        } else {
            format!("{}/{}", opf_dir.display(), h)
        }
    }).unwrap_or_else(|| {
        if opf_dir.as_os_str().is_empty() {
            "toc.ncx".to_string()
        } else {
            format!("{}/toc.ncx", opf_dir.display())
        }
    });

    if let Ok(mut ncx_file) = archive.by_name(&ncx_full_path) {
        let mut ncx_xml = String::new();
        if ncx_file.read_to_string(&mut ncx_xml).is_ok() {
            if let Ok(ncx_doc) = roxmltree::Document::parse(&ncx_xml) {
                for node in ncx_doc.descendants().filter(|n| n.has_tag_name("navPoint")) {
                    let text = node
                        .descendants()
                        .find(|n| n.has_tag_name("text"))
                        .and_then(|n| n.text());
                    let src = node
                        .descendants()
                        .find(|n| n.has_tag_name("content"))
                        .and_then(|n| n.attribute("src"));
                    if let (Some(t), Some(s)) = (text, src) {
                        let clean_src = s.split('#').next().unwrap_or(s);
                        toc_titles.insert(clean_src.to_string(), t.trim().to_string());
                    }
                }
            }
        }
    }

    // 4. Extract chapters
    let mut chapters = Vec::new();
    let mut chapter_idx = 0;

    for (_id, rel_href, full_path) in spine_files {
        // Skip obvious non-content files
        let lower = rel_href.to_lowercase();
        if lower.contains("cover") || lower.contains("information") {
            continue;
        }

        let mut content = String::new();
        let read_res = match archive.by_name(&full_path) {
            Ok(mut f) => f.read_to_string(&mut content),
            Err(_) => continue,
        };
        if read_res.is_err() {
            continue;
        }

        let (title_from_html, cleaned_text) = clean_xhtml(&content);
        if cleaned_text.trim().is_empty() {
            continue;
        }

        chapter_idx += 1;
        let title = toc_titles
            .get(&rel_href)
            .cloned()
            .unwrap_or_else(|| {
                if !title_from_html.is_empty() && title_from_html != "Untitled" {
                    title_from_html
                } else {
                    format!("Chapter {:02}", chapter_idx)
                }
            });

        chapters.push(Chapter {
            index: chapter_idx,
            title,
            text: cleaned_text,
        });
    }

    crate::models::adjust_chapter_indices(&mut chapters);

    Ok(chapters)
}

fn clean_xhtml(xhtml: &str) -> (String, String) {
    let re_title = Regex::new(r"(?is)<title\b[^>]*>(.*?)</title>").unwrap();
    let title = re_title
        .captures(xhtml)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
        .unwrap_or_else(|| "Untitled".to_string());

    let re_head = Regex::new(r"(?is)<head\b[^>]*>.*?</head>").unwrap();
    let body_only = re_head.replace_all(xhtml, "");

    // Crucial for Japanese text: Remove ruby annotations (<rt> and <rp>) including contents
    let re_rt = Regex::new(r"(?is)<rt\b[^>]*>.*?</rt>").unwrap();
    let re_rp = Regex::new(r"(?is)<rp\b[^>]*>.*?</rp>").unwrap();
    let no_rt = re_rt.replace_all(&body_only, "");
    let no_ruby = re_rp.replace_all(&no_rt, "");

    // Convert paragraph, break, and header tags to newlines
    let re_br = Regex::new(r"(?i)<(?:br|/p|/div|/h[1-6])\b[^>]*>").unwrap();
    let with_newlines = re_br.replace_all(&no_ruby, "\n");

    // Remove remaining XML/HTML tags
    let re_tags = Regex::new(r"<[^>]+>").unwrap();
    let plain_text = re_tags.replace_all(&with_newlines, "");

    // Decode HTML entities
    let decoded = plain_text
        .replace("&nbsp;", " ")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");

    (title, decoded.to_string())
}
