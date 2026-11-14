//! The statistics table: new vocabulary per chapter, its density, and the density relative to the first chapter.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::exporter::{write_table, ExportFormat};
use crate::models::ChapterStat;

/// Export the statistics table (TSV/CSV) into `<output_dir>/statistics/`, and return its path.
pub fn export_statistics(output_dir: &Path, stats: &[ChapterStat], format: ExportFormat) -> Result<PathBuf> {
    let stats_dir = output_dir.join("statistics");
    fs::create_dir_all(&stats_dir)
        .with_context(|| format!("Failed to create statistics directory: {:?}", stats_dir))?;

    let table_path = stats_dir.join(format!("vocabulary_ratio.{}", format.extension()));
    let header = ["chapter", "title", "filtered_words", "characters", "raw_ratio", "normalized_ratio"];
    let rows: Vec<Vec<String>> = std::iter::once(header.iter().map(|h| h.to_string()).collect())
        .chain(stats.iter().map(|s| {
            vec![
                s.chapter_index.to_string(),
                s.title.clone(),
                s.filtered_words.to_string(),
                s.characters.to_string(),
                format!("{:.6}", s.raw_ratio),
                format!("{:.6}", s.normalized_ratio),
            ]
        }))
        .collect();
    write_table(&table_path, &rows, format)?;
    Ok(table_path)
}

#[cfg(test)]
mod tests {
    use crate::models::{vocabulary_ratio, ChapterStat};

    fn sample() -> Vec<ChapterStat> {
        let baseline = vocabulary_ratio(500, 5000);
        vec![
            ChapterStat::new(0, "プロローグ <Intro>", 500, 5000, baseline),
            ChapterStat::new(1, "第一話 & 冒険", 300, 5000, baseline),
            ChapterStat::new(2, "第二話", 0, 12000, baseline),
        ]
    }

    #[test]
    fn normalizes_against_baseline() {
        let stats = sample();
        assert!((stats[0].raw_ratio - 0.10).abs() < 1e-9);
        assert!((stats[0].normalized_ratio - 1.0).abs() < 1e-9);
        assert!((stats[1].normalized_ratio - 0.60).abs() < 1e-9);
        assert_eq!(stats[2].normalized_ratio, 0.0);
        assert_eq!(ChapterStat::new(1, "x", 10, 0, 0.1).raw_ratio, 0.0);
    }
}
