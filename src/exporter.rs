use std::fs::{create_dir_all, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use csv::WriterBuilder;

use crate::models::WordEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Tsv,
    Csv,
}

#[derive(Debug, Clone)]
pub struct ChapterExportPaths {
    pub full_path: PathBuf,
    pub words_path: PathBuf,
}

pub fn export_chapter_entries(
    output_dir: &Path,
    chapter_index: usize,
    chapter_title: &str,
    entries: &[WordEntry],
    format: ExportFormat,
    include_first_appearance_col: bool,
) -> Result<ChapterExportPaths> {
    let full_dir = output_dir.join("full");
    let words_dir = output_dir.join("words");

    create_dir_all(&full_dir)
        .with_context(|| format!("Failed to create full output directory: {:?}", full_dir))?;
    create_dir_all(&words_dir)
        .with_context(|| format!("Failed to create words output directory: {:?}", words_dir))?;

    let sanitized_title = sanitize_filename(chapter_title);
    let extension = match format {
        ExportFormat::Tsv => "tsv",
        ExportFormat::Csv => "csv",
    };

    // 1. Export full statistics table
    let full_filename = format!("chapter_{:02}_{}.{}", chapter_index, sanitized_title, extension);
    let full_path = full_dir.join(&full_filename);

    let file = File::create(&full_path)
        .with_context(|| format!("Failed to create output file: {:?}", full_path))?;

    let delimiter = match format {
        ExportFormat::Tsv => b'\t',
        ExportFormat::Csv => b',',
    };

    let mut writer = WriterBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .from_writer(file);

    // Write header
    let mut header = vec![
        "rank",
        "word",
        "pronunciation",
        "pos",
        "occurrences",
        "chapter_frequency",
        "chapter_percent",
        "cumulative_percent",
        "book_occurrences",
        "book_frequency",
        "book_percent",
    ];
    if include_first_appearance_col {
        header.push("first_appearance_chapter");
    }
    writer.write_record(&header)?;

    // Write rows
    for entry in entries {
        let mut row = vec![
            entry.rank.to_string(),
            entry.word.clone(),
            entry.pronunciation.clone(),
            entry.pos.clone(),
            entry.occurrences.to_string(),
            format!("{:.6}", entry.chapter_frequency),
            entry.chapter_percent.clone(),
            entry.cumulative_percent.clone(),
            entry.book_occurrences.to_string(),
            format!("{:.6}", entry.book_frequency),
            entry.book_percent.clone(),
        ];
        if include_first_appearance_col {
            row.push(entry.first_appearance_chapter.clone());
        }
        writer.write_record(&row)?;
    }
    writer.flush()?;

    // 2. Export single-column words text file
    let words_filename = format!("chapter_{:02}_{}.txt", chapter_index, sanitized_title);
    let words_path = words_dir.join(&words_filename);
    let mut words_file = File::create(&words_path)
        .with_context(|| format!("Failed to create words file: {:?}", words_path))?;

    for entry in entries {
        writeln!(words_file, "{}", entry.word)?;
    }
    words_file.flush()?;

    Ok(ChapterExportPaths {
        full_path,
        words_path,
    })
}

fn sanitize_filename(name: &str) -> String {
    let mut clean = String::new();
    for c in name.chars() {
        match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '：' => clean.push('_'),
            ' ' | '\t' | '　' => clean.push('_'),
            _ => clean.push(c),
        }
    }
    // Collapse multiple underscores
    let mut collapsed = String::new();
    let mut prev_underscore = false;
    for c in clean.chars() {
        if c == '_' {
            if !prev_underscore {
                collapsed.push(c);
            }
            prev_underscore = true;
        } else {
            collapsed.push(c);
            prev_underscore = false;
        }
    }
    let trimmed = collapsed.trim_matches('_');
    if trimmed.is_empty() {
        "chapter".to_string()
    } else {
        trimmed.to_string()
    }
}
