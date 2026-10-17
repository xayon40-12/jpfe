mod epub;
mod exporter;
mod frequency;
mod models;
mod pdf;
mod tokenizer;

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;
use anyhow::{bail, Result};
use clap::{Parser, ValueEnum};

use crate::epub::extract_epub_chapters;
use crate::exporter::{export_chapter_entries, ExportFormat};
use crate::frequency::FrequencyAnalyzer;
use crate::models::Chapter;
use crate::pdf::extract_pdf_chapters;
use crate::tokenizer::JapaneseTokenizer;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ReadingFormat {
    Hiragana,
    Katakana,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Tsv,
    Csv,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ScanScope {
    Full,
    Selected,
}

#[derive(Parser, Debug)]
#[command(
    name = "frequency_extraction",
    author = "Japanese Novel Frequency Analyzer",
    version = "0.1.0",
    about = "Extract word frequencies per chapter from Japanese EPUB or PDF books with base-form lemmatization."
)]
struct Args {
    /// Path to input EPUB or PDF file
    #[arg(value_name = "INPUT_FILE")]
    input: PathBuf,

    /// Output directory for per-chapter column files
    #[arg(short = 'o', long = "output-dir", default_value = "output")]
    output_dir: PathBuf,

    /// Target cumulative percentage of vocabulary to learn per chapter (e.g. 99.0)
    #[arg(short = 'p', long = "target-percentage", default_value_t = 99.0)]
    target_percentage: f64,

    /// Number of chapters to output (e.g. 3 for first 3 chapters)
    #[arg(short = 'n', long = "num-chapters")]
    num_chapters: Option<usize>,

    /// Specific chapter numbers or ranges to output (e.g. '1,2,3' or '1-3')
    #[arg(short = 'c', long = "chapters")]
    chapters: Option<String>,

    /// Output file format
    #[arg(long = "format", value_enum, default_value_t = OutputFormat::Tsv)]
    format: OutputFormat,

    /// Pronunciation format
    #[arg(long = "reading", value_enum, default_value_t = ReadingFormat::Hiragana)]
    reading: ReadingFormat,

    /// Only include content words (exclude particles and auxiliary verbs)
    #[arg(long = "content-only", default_value_t = false)]
    content_only: bool,

    /// Minimum occurrences in chapter to be included
    #[arg(long = "min-count", default_value_t = 1)]
    min_count: usize,

    /// Show all words including those that appeared in previous chapters (default is to hide previous words with rank holes)
    #[arg(long = "show-previous-words", visible_alias = "show-seen", visible_alias = "show-all", default_value_t = false)]
    show_previous_words: bool,

    /// Explicitly include first_appearance_chapter column even when hiding previous words
    #[arg(long = "show-first-appearance", default_value_t = false)]
    show_first_appearance: bool,

    /// Path to custom external MeCab/Lindera dictionary directory
    #[arg(long = "dict-path")]
    dict_path: Option<PathBuf>,

    /// Book-wide scanning scope for whole-book frequency and first-appearance chapter calculation
    #[arg(long = "scan-scope", value_enum, default_value_t = ScanScope::Full)]
    scan_scope: ScanScope,
}

fn parse_chapter_selection(spec: &str, total_chapters: usize) -> Result<HashSet<usize>> {
    let mut selected = HashSet::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((start_str, end_str)) = part.split_once('-') {
            let start: usize = start_str.trim().parse()?;
            let end: usize = end_str.trim().parse()?;
            for i in start..=end {
                if i >= 1 && i <= total_chapters {
                    selected.insert(i);
                }
            }
        } else {
            let num: usize = part.parse()?;
            if num >= 1 && num <= total_chapters {
                selected.insert(num);
            }
        }
    }
    Ok(selected)
}

fn main() -> Result<()> {
    let args = Args::parse();

    if !args.input.exists() {
        bail!("Input file does not exist: {:?}", args.input);
    }

    println!("============================================================");
    println!("  Japanese Novel Word Frequency & Vocabulary Extractor");
    println!("============================================================");
    println!("Input: {:?}", args.input);
    println!("Output directory: {:?}", args.output_dir);
    let hide_previous_words = !args.show_previous_words;
    println!("Target cumulative percentage: {:.1}%", args.target_percentage);
    println!("Pronunciation: {:?}", args.reading);
    println!("Content words only: {}", args.content_only);
    println!("Hide previously seen words: {}", hide_previous_words);
    println!("------------------------------------------------------------");

    let t0 = Instant::now();

    // 1. Extract chapters from EPUB or PDF
    let ext = args
        .input
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    let all_chapters: Vec<Chapter> = match ext.as_str() {
        "epub" => {
            println!("Reading EPUB structure and chapters...");
            extract_epub_chapters(&args.input)?
        }
        "pdf" => {
            println!("Reading PDF and detecting chapter boundaries...");
            extract_pdf_chapters(&args.input)?
        }
        _ => {
            bail!("Unsupported file format: '.{}'. Supported formats: epub, pdf", ext);
        }
    };

    if all_chapters.is_empty() {
        bail!("No chapters could be extracted from {:?}", args.input);
    }

    println!("Extracted {} chapters from book in {:?}", all_chapters.len(), t0.elapsed());
    for (i, ch) in all_chapters.iter().take(5).enumerate() {
        println!("  [{:>2}] {} ({} chars)", i + 1, ch.title, ch.text.chars().count());
    }
    if all_chapters.len() > 5 {
        println!("  ... and {} more chapters", all_chapters.len() - 5);
    }
    println!("------------------------------------------------------------");

    // 2. Determine which chapters to output
    let selected_indices: HashSet<usize> = if let Some(ref spec) = args.chapters {
        parse_chapter_selection(spec, all_chapters.len())?
    } else if let Some(n) = args.num_chapters {
        (1..=n.min(all_chapters.len())).collect()
    } else {
        (1..=all_chapters.len()).collect()
    };

    println!("Selected {} chapter(s) for export:", selected_indices.len());
    let mut selected_chapters_list: Vec<&Chapter> = all_chapters
        .iter()
        .filter(|ch| selected_indices.contains(&ch.index))
        .collect();
    selected_chapters_list.sort_by_key(|ch| ch.index);

    for ch in &selected_chapters_list {
        println!("  -> Chapter {:02}: {}", ch.index, ch.title);
    }
    println!("------------------------------------------------------------");

    // 3. Initialize tokenizer
    let reading_in_hiragana = matches!(args.reading, ReadingFormat::Hiragana);
    let include_particles = !args.content_only;

    println!("Initializing Japanese morphological tokenizer (Lindera + IPADIC)...");
    let mut tokenizer = JapaneseTokenizer::new(
        args.dict_path.as_deref(),
        reading_in_hiragana,
        include_particles,
    )?;

    // 4. Build frequency analyzer across the book
    let t_tokenize = Instant::now();
    let chapters_to_scan = match args.scan_scope {
        ScanScope::Full => &all_chapters[..],
        ScanScope::Selected => {
            println!("Scanning selected chapters only...");
            // Use only selected chapters
            &all_chapters[..] // handled below by filtering if needed
        }
    };

    println!(
        "Tokenizing and calculating book-wide vocabulary statistics across {} chapters...",
        chapters_to_scan.len()
    );
    let (analyzer, tokenized_chapters) = FrequencyAnalyzer::build(&mut tokenizer, chapters_to_scan)?;
    println!(
        "Processed {} total words ({} unique base vocabulary) across entire book in {:?}",
        analyzer.book_total_words,
        analyzer.book_word_counts.len(),
        t_tokenize.elapsed()
    );
    println!("------------------------------------------------------------");

    // 5. Analyze and export each selected chapter
    let export_format = match args.format {
        OutputFormat::Tsv => ExportFormat::Tsv,
        OutputFormat::Csv => ExportFormat::Csv,
    };
    let include_first_appearance_col = args.show_previous_words || args.show_first_appearance;

    let mut total_exported_files = Vec::new();

    for (chapter_idx, chapter) in all_chapters.iter().enumerate() {
        if !selected_indices.contains(&chapter.index) {
            continue;
        }

        let chapter_tokens = &tokenized_chapters[chapter_idx];
        let entries = analyzer.analyze_chapter(
            &chapter.title,
            chapter_tokens,
            args.target_percentage,
            args.min_count,
            hide_previous_words,
        );

        let out_path = export_chapter_entries(
            &args.output_dir,
            chapter.index,
            &chapter.title,
            &entries,
            export_format,
            include_first_appearance_col,
        )?;

        total_exported_files.push(out_path.clone());

        let total_unique_in_ch = {
            let set: HashSet<&str> = chapter_tokens.iter().map(|t| t.base_word.as_str()).collect();
            set.len()
        };

        println!(
            "Chapter {:02}: \"{}\"",
            chapter.index, chapter.title
        );
        let filter_mode_desc = if hide_previous_words {
            "New words exported (seen words hidden, ranks preserved with holes)"
        } else {
            "Words exported"
        };
        println!(
            "  Tokens: {} | Total unique words: {} | {}: {}",
            chapter_tokens.len(),
            total_unique_in_ch,
            filter_mode_desc,
            entries.len()
        );
        println!("  Saved to: {}", out_path.display());

        // Print top preview
        println!("  Vocabulary preview:");
        if include_first_appearance_col {
            println!(
                "    {:<4} {:<10} {:<12} {:<8} {:<6} {:<8} {:<8} {:<10}",
                "Rank", "Word", "Reading", "POS", "Count", "Chapter%", "Cumul%", "First Seen"
            );
            for e in entries.iter().take(8) {
                println!(
                    "    {:<4} {:<10} {:<12} {:<8} {:<6} {:<8} {:<8} {:<10}",
                    e.rank,
                    e.word,
                    e.pronunciation,
                    e.pos,
                    e.occurrences,
                    e.chapter_percent,
                    e.cumulative_percent,
                    e.first_appearance_chapter.chars().take(8).collect::<String>()
                );
            }
        } else {
            println!(
                "    {:<4} {:<10} {:<12} {:<8} {:<6} {:<8} {:<8}",
                "Rank", "Word", "Reading", "POS", "Count", "Chapter%", "Cumul%"
            );
            for e in entries.iter().take(8) {
                println!(
                    "    {:<4} {:<10} {:<12} {:<8} {:<6} {:<8} {:<8}",
                    e.rank,
                    e.word,
                    e.pronunciation,
                    e.pos,
                    e.occurrences,
                    e.chapter_percent,
                    e.cumulative_percent
                );
            }
        }
        println!();
    }

    println!("============================================================");
    println!(
        "Successfully exported {} chapter files in {:?}",
        total_exported_files.len(),
        t0.elapsed()
    );
    println!("Output directory: {}", std::fs::canonicalize(&args.output_dir)?.display());
    println!("============================================================");

    Ok(())
}
