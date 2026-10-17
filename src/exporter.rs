use std::fs::{create_dir_all, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use csv::WriterBuilder;
use unicode_width::UnicodeWidthStr;

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

    // 1. Build table rows (omitting redundant frequency decimals, keeping percentages)
    let mut header = vec![
        "rank".to_string(),
        "word".to_string(),
        "pronunciation".to_string(),
        "pos".to_string(),
        "occurrences".to_string(),
        "chapter_percent".to_string(),
        "cumulative_percent".to_string(),
        "book_occurrences".to_string(),
        "book_percent".to_string(),
    ];
    if include_first_appearance_col {
        header.push("first_appearance_chapter".to_string());
    }

    let mut rows: Vec<Vec<String>> = Vec::with_capacity(entries.len() + 1);
    rows.push(header);

    for entry in entries {
        let mut row = vec![
            entry.rank.to_string(),
            entry.word.clone(),
            entry.pronunciation.clone(),
            entry.pos.clone(),
            entry.occurrences.to_string(),
            entry.chapter_percent.clone(),
            entry.cumulative_percent.clone(),
            entry.book_occurrences.to_string(),
            entry.book_percent.clone(),
        ];
        if include_first_appearance_col {
            row.push(entry.first_appearance_chapter.clone());
        }
        rows.push(row);
    }

    // 2. Export full statistics table
    let full_filename = format!("chapter_{:02}_{}.{}", chapter_index, sanitized_title, extension);
    let full_path = full_dir.join(&full_filename);

    match format {
        ExportFormat::Csv => {
            let file = File::create(&full_path)
                .with_context(|| format!("Failed to create output file: {:?}", full_path))?;
            let mut writer = WriterBuilder::new()
                .delimiter(b',')
                .has_headers(false)
                .from_writer(file);
            for row in &rows {
                writer.write_record(row)?;
            }
            writer.flush()?;
        }
        ExportFormat::Tsv => {
            // Calculate column widths based on East Asian Display Width
            let num_cols = rows[0].len();
            let mut col_widths = vec![0; num_cols];
            for row in &rows {
                for (i, cell) in row.iter().enumerate() {
                    col_widths[i] = col_widths[i].max(cell.width());
                }
            }

            let mut file = File::create(&full_path)
                .with_context(|| format!("Failed to create output file: {:?}", full_path))?;

            for row in &rows {
                let mut padded_parts = Vec::with_capacity(num_cols);
                for (i, cell) in row.iter().enumerate() {
                    let cell_w = cell.width();
                    let pad = col_widths[i].saturating_sub(cell_w);
                    padded_parts.push(format!("{}{}", cell, " ".repeat(pad)));
                }
                writeln!(file, "{}", padded_parts.join("\t"))?;
            }
            file.flush()?;
        }
    }

    // 3. Export single-column words text file
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
