use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;
use anyhow::{bail, Result};
use clap::{Parser, ValueEnum};

use jpfe::epub::extract_epub_chapters;
use jpfe::exporter::{export_chapter_entries, pad, ExportFormat};
use jpfe::frequency::FrequencyAnalyzer;
use jpfe::models::{vocabulary_ratio, ChapterStat};
use jpfe::pdf::extract_pdf_chapters;
use jpfe::statistics::export_statistics;
use jpfe::tokenizer::JapaneseTokenizer;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ReadingFormat {
    Hiragana,
    Katakana,
}

#[derive(Parser, Debug)]
#[command(
    name = "jpfe",
    author = "Japanese Novel Frequency Analyzer",
    version,
    about = "Extract word frequencies per chapter from Japanese EPUB or PDF books with base-form lemmatization."
)]
struct Args {
    /// Path to input EPUB or PDF file
    #[arg(value_name = "INPUT_FILE")]
    input: PathBuf,

    /// Output directory for per-chapter column files (default: adjacent to input file, named '<input>_frequency')
    #[arg(short = 'o', long = "output-dir")]
    output_dir: Option<PathBuf>,

    /// Target cumulative percentage of vocabulary to learn per chapter (e.g. 99.0)
    #[arg(short = 'p', long = "target-percentage", default_value_t = 99.0)]
    target_percentage: f64,

    /// Number of chapters to output (e.g. 3 for first 3 chapters)
    #[arg(short = 'n', long = "num-chapters", conflicts_with = "chapters")]
    num_chapters: Option<usize>,

    /// Specific chapter numbers or ranges to output (e.g. '1,2,3' or '1-3')
    #[arg(short = 'c', long = "chapters")]
    chapters: Option<String>,

    /// Output file format
    #[arg(long = "format", value_enum, default_value_t = ExportFormat::Tsv)]
    format: ExportFormat,

    /// Pronunciation format
    #[arg(long = "reading", value_enum, default_value_t = ReadingFormat::Hiragana)]
    reading: ReadingFormat,

    /// Include grammatical particles (助詞) and auxiliary verbs (助動詞) (default: false, clean vocabulary only)
    #[arg(long = "include-particles", visible_alias = "include-grammar", default_value_t = false)]
    include_particles: bool,

    /// Minimum occurrences across the entire novel to be included (default: 3)
    #[arg(short = 'm', long = "min-book-count", visible_alias = "min-count", default_value_t = 3)]
    min_book_count: usize,

    /// Show all words including those that appeared in previous chapters (default is to hide previous words with rank holes)
    #[arg(long = "show-previous-words", visible_alias = "show-seen", visible_alias = "show-all", default_value_t = false)]
    show_previous_words: bool,

    /// Explicitly include first_appearance_chapter column even when hiding previous words
    #[arg(long = "show-first-appearance", default_value_t = false)]
    show_first_appearance: bool,

    /// Path to custom external MeCab/Lindera dictionary directory
    #[arg(long = "dict-path")]
    dict_path: Option<PathBuf>,
}

fn parse_chapter_selection(spec: &str, valid: std::ops::RangeInclusive<usize>) -> Result<HashSet<usize>> {
    let mut selected = HashSet::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (start, end) = part.split_once('-').unwrap_or((part, part));
        let (start, end): (usize, usize) = (start.trim().parse()?, end.trim().parse()?);
        if start > end {
            bail!("Invalid chapter range '{part}': start is after end");
        }
        selected.extend((start..=end).filter(|i| valid.contains(i)));
    }
    Ok(selected)
}

const RULE: &str = "------------------------------------------------------------";
const BANNER: &str = "============================================================";

fn main() -> Result<()> {
    let args = Args::parse();
    if !args.input.exists() {
        bail!("Input file does not exist: {:?}", args.input);
    }
    let output_dir = args.output_dir.clone().unwrap_or_else(|| {
        let name = args.input.file_name().map_or("output".into(), |s| s.to_string_lossy());
        args.input.parent().unwrap_or(Path::new("")).join(format!("{name}_frequency"))
    });
    let hide_previous_words = !args.show_previous_words;

    println!("{BANNER}\n  Japanese Novel Word Frequency & Vocabulary Extractor\n{BANNER}");
    println!("Input: {:?}", args.input);
    println!("Output directory: {:?}", output_dir);
    println!("Target cumulative percentage: {:.1}%", args.target_percentage);
    println!("Min occurrences in whole novel: {}", args.min_book_count);
    println!("Pronunciation: {:?}", args.reading);
    println!("Grammar filtering: {}", if args.include_particles { "Particles/auxiliaries included" } else { "Particles, auxiliaries and contractions excluded" });
    println!("Hide previously seen words: {hide_previous_words}\n{RULE}");

    let t0 = Instant::now();
    let ext = args.input.extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
    let chapters = match ext.as_str() {
        "epub" => {
            println!("Reading EPUB structure and chapters...");
            extract_epub_chapters(&args.input)?
        }
        "pdf" => {
            println!("Reading PDF and detecting chapter boundaries...");
            extract_pdf_chapters(&args.input)?
        }
        _ => bail!("Unsupported file format: '.{}'. Supported formats: epub, pdf", ext),
    };
    // Indices are consecutive (see adjust_chapter_indices)
    let (Some(first), Some(last)) = (chapters.first(), chapters.last()) else {
        bail!("No chapters could be extracted from {:?}", args.input);
    };
    let pad_width = last.index.to_string().len().max(2);
    let char_counts: Vec<usize> = chapters.iter().map(|ch| ch.text.chars().count()).collect();

    println!("Extracted {} chapters from book in {:?}", chapters.len(), t0.elapsed());
    for (ch, chars) in chapters.iter().zip(&char_counts).take(5) {
        println!("  [{:>pad_width$}] {} ({} chars)", ch.index, ch.title, chars);
    }
    if chapters.len() > 5 {
        println!("  ... and {} more chapters", chapters.len() - 5);
    }
    println!("{RULE}");

    let selected: HashSet<usize> = match (&args.chapters, args.num_chapters) {
        (Some(spec), _) => parse_chapter_selection(spec, first.index..=last.index)?,
        (None, Some(n)) => chapters.iter().take(n).map(|ch| ch.index).collect(),
        (None, None) => chapters.iter().map(|ch| ch.index).collect(),
    };
    println!("Selected {} chapter(s) for export:", selected.len());
    for ch in chapters.iter().filter(|ch| selected.contains(&ch.index)) {
        println!("  -> Chapter {:0pad_width$}: {}", ch.index, ch.title);
    }
    println!("{RULE}");

    println!("Initializing Japanese morphological tokenizer (Lindera + UniDic)...");
    let reading_in_hiragana = matches!(args.reading, ReadingFormat::Hiragana);
    let mut tokenizer = JapaneseTokenizer::new(args.dict_path.as_deref(), reading_in_hiragana)?;

    let t_tokenize = Instant::now();
    println!("Tokenizing and calculating book-wide vocabulary statistics across {} chapters...", chapters.len());
    let (analyzer, tokenized_chapters) = FrequencyAnalyzer::build(&mut tokenizer, &chapters, args.include_particles)?;
    println!(
        "Processed {} total words ({} unique base vocabulary) across entire book in {:?}\n{RULE}",
        analyzer.total_words,
        analyzer.words.len(),
        t_tokenize.elapsed()
    );

    let show_first_seen = args.show_previous_words || args.show_first_appearance;
    let analyze = |i: usize| {
        let chapter = &tokenized_chapters[i];
        analyzer.analyze_chapter(i, chapter, args.target_percentage, args.min_book_count, hide_previous_words, args.include_particles)
    };
    // Vocabulary-ratio baseline: the first chapter (the prologue if there is one, otherwise chapter 1)
    let baseline_ratio = vocabulary_ratio(analyze(0).len(), char_counts[0]);
    let filter_mode_desc = if hide_previous_words {
        "New words exported (seen words hidden, ranks preserved with holes)"
    } else {
        "Words exported"
    };
    let mut chapter_stats = Vec::new();

    for (i, chapter) in chapters.iter().enumerate().filter(|(_, ch)| selected.contains(&ch.index)) {
        let tokens = &tokenized_chapters[i];
        let entries = analyze(i);
        let paths = export_chapter_entries(&output_dir, chapter.index, &chapter.title, &entries, args.format, show_first_seen, pad_width)?;
        chapter_stats.push(ChapterStat::new(chapter.index, &chapter.title, entries.len(), char_counts[i], baseline_ratio));
        let unique = tokens.iter().map(|t| t.key()).collect::<HashSet<_>>().len();

        println!("Chapter {:0pad_width$}: \"{}\"", chapter.index, chapter.title);
        println!("  Tokens: {} | Total unique words: {} | {}: {}", tokens.len(), unique, filter_mode_desc, entries.len());
        println!("  Full stats: {}", paths.full_path.display());
        println!("  Words list: {}", paths.words_path.display());
        println!("  Vocabulary preview:");
        let header = ["Rank", "Word", "Reading", "POS", "Count", "Chapter%", "Cumul%", "First Seen"].map(String::from);
        let rows = entries.iter().take(8).map(|e| {
            [
                e.rank.to_string(), e.word.clone(), e.pronunciation.clone(), e.pos.clone(), e.occurrences.to_string(),
                e.chapter_percent.clone(), e.cumulative_percent.clone(), e.first_appearance_chapter.chars().take(10).collect(),
            ]
        });
        let columns = if show_first_seen { 8 } else { 7 };
        for row in std::iter::once(header).chain(rows) {
            let cells: Vec<String> = row.iter().zip([5, 12, 14, 8, 6, 9, 8, 12]).take(columns).map(|(c, w)| pad(c, w)).collect();
            println!("    {}", cells.join(" "));
        }
        println!();
    }

    if !chapter_stats.is_empty() {
        let table_path = export_statistics(&output_dir, &chapter_stats, args.format)?;
        println!("{RULE}\nVocabulary Density Statistics:");
        println!("  Table:     {}", table_path.display());
        println!("  Baseline:  Ch {:0pad_width$} \"{}\" (raw ratio: {:.6})\n", first.index, first.title, baseline_ratio);
    }

    println!("{BANNER}");
    println!("Successfully exported {} chapter files in {:?}", chapter_stats.len(), t0.elapsed());
    println!("Output directory: {}", std::fs::canonicalize(&output_dir)?.display());
    println!("{BANNER}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chapter_selection() {
        let selected = parse_chapter_selection("1, 3-5,9-12", 0..=10).unwrap();
        assert_eq!(selected, HashSet::from([1, 3, 4, 5, 9, 10]));
        assert!(parse_chapter_selection("5-3", 0..=10).is_err());
    }

    #[test]
    fn chapters_and_num_chapters_conflict() {
        assert!(Args::try_parse_from(["jpfe", "book.epub", "-c", "1", "-n", "2"]).is_err());
    }
}
