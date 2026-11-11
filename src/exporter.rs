use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use clap::ValueEnum;
use unicode_width::UnicodeWidthStr;

use crate::models::{chapter_file_stem, WordEntry};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExportFormat {
    Tsv,
    Csv,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Tsv => "tsv",
            ExportFormat::Csv => "csv",
        }
    }
}

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
    pad_width: usize,
) -> Result<ChapterExportPaths> {
    let stem = chapter_file_stem(chapter_index, chapter_title, pad_width);
    let full_path = output_dir.join("full").join(format!("{stem}.{}", format.extension()));
    let words_path = output_dir.join("words").join(format!("{stem}.txt"));
    for dir in [&full_path, &words_path].map(|p| p.parent().unwrap()) {
        fs::create_dir_all(dir).with_context(|| format!("Failed to create output directory: {:?}", dir))?;
    }

    let mut header = vec![
        "rank", "word", "pronunciation", "pos", "occurrences",
        "chapter_percent", "cumulative_percent", "book_occurrences", "book_percent",
    ];
    if include_first_appearance_col {
        header.push("first_appearance_chapter");
    }
    let rows: Vec<Vec<String>> = std::iter::once(header.into_iter().map(String::from).collect())
        .chain(entries.iter().map(|e| {
            let mut row = vec![
                e.rank.to_string(), e.word.clone(), e.pronunciation.clone(), e.pos.clone(),
                e.occurrences.to_string(), e.chapter_percent.clone(), e.cumulative_percent.clone(),
                e.book_occurrences.to_string(), e.book_percent.clone(),
            ];
            if include_first_appearance_col {
                row.push(e.first_appearance_chapter.clone());
            }
            row
        }))
        .collect();
    write_table(&full_path, &rows, format)?;

    let words: String = entries.iter().map(|e| format!("{}\n", e.word)).collect();
    fs::write(&words_path, words).with_context(|| format!("Failed to write words file: {:?}", words_path))?;

    Ok(ChapterExportPaths { full_path, words_path })
}

/// Left-align `s` to `width` East Asian display columns.
pub fn pad(s: &str, width: usize) -> String {
    format!("{s}{}", " ".repeat(width.saturating_sub(s.width())))
}

/// Write `rows` (header included) as CSV, or as TSV padded to East Asian display width.
pub fn write_table(path: &Path, rows: &[Vec<String>], format: ExportFormat) -> Result<()> {
    let file = File::create(path).with_context(|| format!("Failed to create output file: {:?}", path))?;
    match format {
        ExportFormat::Csv => {
            let mut writer = csv::WriterBuilder::new().has_headers(false).from_writer(file);
            for row in rows {
                writer.write_record(row)?;
            }
            writer.flush()?;
        }
        ExportFormat::Tsv => {
            let mut widths = vec![0; rows.first().map_or(0, Vec::len)];
            for row in rows {
                for (w, cell) in widths.iter_mut().zip(row) {
                    *w = (*w).max(cell.width());
                }
            }
            let mut file = BufWriter::new(file);
            for row in rows {
                let cells: Vec<String> = row.iter().zip(&widths).map(|(c, &w)| pad(c, w)).collect();
                writeln!(file, "{}", cells.join("\t"))?;
            }
            file.flush()?;
        }
    }
    Ok(())
}
