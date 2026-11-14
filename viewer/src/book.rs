//! Loaded books, their reading progress, and the statistics shown for them.

use std::collections::HashSet;
use std::io::{Cursor, Write};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use jpfe::epub::extract_epub_chapters_from_bytes;
use jpfe::frequency::{ChapterCounts, FrequencyAnalyzer};
use jpfe::models::{chapter_file_stem, Chapter, ChapterStat};
use jpfe::pdf::extract_pdf_chapters_from_bytes;
use jpfe::tokenizer::{is_katakana_word, JapaneseTokenizer, ParsedWord};
#[cfg(test)]
use jpfe::tokenizer::WordKind;
use web_time::Instant;

/// Word-list settings, with the same meaning and defaults as the CLI options.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// `--target-percentage`: each chapter's list stops once its words cover this share of the chapter
    pub target_percentage: f64,
    /// `--min-book-count`: words seen fewer times in the whole book are left out
    pub min_book_count: usize,
    /// Opposite of `--show-previous-words`: only list words new to the book
    pub hide_previous_words: bool,
    /// `--include-particles`; changing it analyzes the books again (which takes them from their text)
    pub include_particles: bool,
    /// Leave out words written only in katakana (mostly loanwords)
    pub hide_katakana_words: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            target_percentage: 99.0,
            min_book_count: 3,
            hide_previous_words: true,
            include_particles: false,
            hide_katakana_words: true,
        }
    }
}

pub struct ChapterInfo {
    pub index: usize,
    pub title: String,
    pub characters: usize,
}

/// What a cached analysis was computed with; it is only reused when all of these match.
pub struct CacheKey<'a> {
    /// Hash of the viewer's and the library's code
    pub build: &'a str,
    pub dictionary: &'a str,
    pub include_particles: bool,
}

/// A book as remembered between sessions (its file and analysis are stored separately).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedBook {
    pub id: u64,
    pub name: String,
    pub file_name: String,
    pub learned: Option<usize>,
}

/// Stored analysis. Field order matters: `CachedAnalysisRef` encodes the same fields with postcard.
#[derive(Deserialize)]
struct CachedAnalysis {
    build: String,
    dictionary: String,
    include_particles: bool,
    chapters: Vec<Chapter>,
    analyzer: FrequencyAnalyzer,
    counts: Vec<ChapterCounts>,
}

#[derive(Serialize)]
struct CachedAnalysisRef<'a> {
    build: &'a str,
    dictionary: &'a str,
    include_particles: bool,
    chapters: &'a [Chapter],
    analyzer: &'a FrequencyAnalyzer,
    counts: &'a [ChapterCounts],
}

pub struct Book {
    /// Key of the book in storage; also its position, as ids grow in import order
    pub id: u64,
    pub file_name: String,
    pub name: String,
    pub chapters: Vec<ChapterInfo>,
    /// The extracted chapters, to tokenize them again when the tokenizer setting changes
    texts: Vec<Chapter>,
    analyzer: FrequencyAnalyzer,
    counts: Vec<ChapterCounts>,
    /// Words to learn in each chapter, as in the CLI's `words/` files, by dictionary form (which matches
    /// them across books however each book spells them; see [`Book::spelling`])
    pub words: Vec<Vec<String>>,
    /// Position of the last chapter whose words the reader has learned; `None` before the first one.
    pub learned: Option<usize>,
}

impl Book {
    fn new(saved: SavedBook, texts: Vec<Chapter>, analyzer: FrequencyAnalyzer, counts: Vec<ChapterCounts>, settings: &Settings) -> Self {
        let words = word_lists(&analyzer, &counts, settings);
        let chapters = texts
            .iter()
            .map(|c| ChapterInfo { index: c.index, characters: c.text.chars().count(), title: c.title.clone() })
            .collect();
        let SavedBook { id, name, file_name, learned } = saved;
        // A new build may find fewer chapters than when the progress was saved
        let learned = learned.map(|pos| pos.min(texts.len().saturating_sub(1)));
        Book { id, file_name, name, chapters, texts, analyzer, counts, words, learned }
    }

    pub fn saved(&self) -> SavedBook {
        SavedBook { id: self.id, name: self.name.clone(), file_name: self.file_name.clone(), learned: self.learned }
    }

    /// The analysis, to store and reuse with the same `key`.
    pub fn analysis_bytes(&self, key: &CacheKey) -> Vec<u8> {
        let cached = CachedAnalysisRef {
            build: key.build,
            dictionary: key.dictionary,
            include_particles: key.include_particles,
            chapters: &self.texts,
            analyzer: &self.analyzer,
            counts: &self.counts,
        };
        postcard::to_allocvec(&cached).unwrap_or_default()
    }

    /// The book from a stored analysis, if it was made with the same `key`.
    pub fn from_analysis(saved: SavedBook, bytes: &[u8], key: &CacheKey, settings: &Settings) -> Option<Self> {
        let cached: CachedAnalysis = postcard::from_bytes(bytes).ok()?;
        let matches = cached.build == key.build
            && cached.dictionary == key.dictionary
            && cached.include_particles == key.include_particles
            && cached.chapters.len() == cached.counts.len()
            && !cached.chapters.is_empty();
        matches.then(|| Self::new(saved, cached.chapters, cached.analyzer, cached.counts, settings))
    }

    /// Words of every chapter up to and including the learned one.
    pub fn learned_words(&self) -> impl Iterator<Item = &str> {
        let count = self.learned.map_or(0, |pos| pos + 1);
        self.words.iter().take(count).flatten().map(String::as_str)
    }

    /// Rebuild the word lists for new settings.
    pub fn apply(&mut self, settings: &Settings) {
        self.words = word_lists(&self.analyzer, &self.counts, settings);
    }

    /// Start over from the chapter texts, keeping the reader's progress.
    pub fn retokenize(self) -> PendingBook {
        let saved = self.saved();
        PendingBook { saved, chapters: self.texts, tokens: Vec::new() }
    }

    /// Words learned in every book but `book`.
    fn known_elsewhere(books: &[Book], book: usize) -> HashSet<&str> {
        books.iter().enumerate().filter(|&(i, _)| i != book).flat_map(|(_, b)| b.learned_words()).collect()
    }

    /// For each book, how many of its words are not learned in the other books.
    pub fn remaining_counts(books: &[Book]) -> Vec<usize> {
        (0..books.len())
            .map(|i| {
                let known = Self::known_elsewhere(books, i);
                books[i].words.iter().flatten().filter(|w| !known.contains(w.as_str())).count()
            })
            .collect()
    }

    /// The text of the chapter at `pos`.
    pub fn chapter_text(&self, pos: usize) -> &str {
        self.texts.get(pos).map_or("", |c| c.text.as_str())
    }

    /// How this book writes a word most often (`word` is its dictionary form).
    pub fn spelling<'a>(&'a self, word: &'a str) -> &'a str {
        self.analyzer.words.get(word).and_then(|w| w.usual_spelling()).map_or(word, |s| s.spelling.as_str())
    }

    pub fn chapter_label(&self, pos: usize) -> String {
        let c = &self.chapters[pos];
        format!("{}: {}", c.index, c.title)
    }

    /// Words of each chapter, minus the words already learned in the other books.
    pub fn stats(books: &[Book], shown: usize) -> BookStats {
        let known = Self::known_elsewhere(books, shown);
        let book = &books[shown];
        let words: Vec<Vec<String>> = book
            .words
            .iter()
            .map(|ws| ws.iter().filter(|w| !known.contains(w.as_str())).cloned().collect())
            .collect();
        let excluded = book.words.iter().map(Vec::len).sum::<usize>() - words.iter().map(Vec::len).sum::<usize>();
        let chapters = book
            .chapters
            .iter()
            .zip(&words)
            // No reference chapter: the viewer computes its own density, per 100 words of the chapter
            .map(|(c, w)| ChapterStat::new(c.index, &c.title, w.len(), c.characters, 0.0))
            .collect();
        // Distinct words, as a chapter's list can repeat earlier ones when previous words are shown
        let mut seen = HashSet::new();
        let learned_so_far = words
            .iter()
            .map(|ws| {
                seen.extend(ws.iter().map(String::as_str));
                seen.len()
            })
            .collect();
        let chapter_words = book.counts.iter().map(|c| c.total).collect();
        BookStats { chapters, chapter_words, learned_so_far, words, excluded }
    }
}

pub struct BookStats {
    pub chapters: Vec<ChapterStat>,
    /// Words each chapter has (counted like the word lists: no particles, unless they are included)
    pub chapter_words: Vec<usize>,
    /// Words to learn from the start of the book up to and including each chapter
    pub learned_so_far: Vec<usize>,
    /// Remaining words of each chapter
    pub words: Vec<Vec<String>>,
    /// How many of the book's words were removed because they are learned in other books
    pub excluded: usize,
}

impl BookStats {
    /// Zip with one word list per chapter, laid out like the CLI's `words/` directory.
    pub fn words_zip(&self, book: &Book) -> Result<Vec<u8>> {
        let pad_width = book.chapters.last().map_or(2, |c| c.index.to_string().len().max(2));
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (chapter, words) in book.chapters.iter().zip(&self.words) {
            let path = format!("words/{}.txt", chapter_file_stem(chapter.index, &chapter.title, pad_width));
            zip.start_file(path, zip::write::SimpleFileOptions::default())?;
            for word in words {
                writeln!(zip, "{}", book.spelling(word))?;
            }
        }
        Ok(zip.finish()?.into_inner())
    }
}

/// A book being tokenized a few chapters per frame, so the window stays responsive
/// (the web version has no threads).
pub struct PendingBook {
    /// Id, names and the reader's progress, kept when a book is tokenized again
    pub saved: SavedBook,
    chapters: Vec<Chapter>,
    tokens: Vec<Vec<ParsedWord>>,
}

impl PendingBook {
    pub fn parse(id: u64, file_name: &str, bytes: Vec<u8>, learned: Option<usize>) -> Result<Self> {
        let lower = file_name.to_lowercase();
        let chapters = if lower.ends_with(".epub") {
            extract_epub_chapters_from_bytes(bytes)?
        } else if lower.ends_with(".pdf") {
            extract_pdf_chapters_from_bytes(&bytes)?
        } else {
            bail!("{file_name}: only .epub and .pdf files are supported");
        };
        if chapters.is_empty() {
            bail!("{file_name}: no chapters found");
        }
        let name = file_name.rsplit_once('.').map_or(file_name, |(stem, _)| stem).to_string();
        let saved = SavedBook { id, name, file_name: file_name.to_string(), learned };
        Ok(Self { saved, chapters, tokens: Vec::new() })
    }

    /// Tokenize again from the first chapter (after a tokenizer setting changed).
    pub fn restart(&mut self) {
        self.tokens.clear();
    }

    pub fn progress(&self) -> f32 {
        self.tokens.len() as f32 / self.chapters.len() as f32
    }

    /// Tokenize chapters until `deadline`; true once every chapter is done.
    pub fn step(&mut self, tokenizer: &mut JapaneseTokenizer, deadline: Instant) -> Result<bool> {
        while let Some(chapter) = self.chapters.get(self.tokens.len()) {
            self.tokens.push(tokenizer.tokenize(&chapter.text)?);
            if Instant::now() >= deadline {
                break;
            }
        }
        Ok(self.tokens.len() == self.chapters.len())
    }

    pub fn finish(self, settings: &Settings) -> Book {
        let particles = settings.include_particles;
        let analyzer = FrequencyAnalyzer::from_tokens(&self.chapters, &self.tokens, particles);
        let counts: Vec<ChapterCounts> = self.tokens.iter().map(|t| ChapterCounts::new(t, particles)).collect();
        Book::new(self.saved, self.chapters, analyzer, counts, settings)
    }
}

fn word_lists(analyzer: &FrequencyAnalyzer, counts: &[ChapterCounts], s: &Settings) -> Vec<Vec<String>> {
    counts
        .iter()
        .enumerate()
        .map(|(i, c)| {
            analyzer
                .analyze_counts(i, c, s.target_percentage, s.min_book_count, s.hide_previous_words)
                .into_iter()
                // Like the CLI's other filters, this leaves the coverage cut-off unchanged
                .filter(|e| !(s.hide_katakana_words && is_katakana_word(&e.word)))
                .map(|e| e.key)
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn book(name: &str, words: &[&[&str]]) -> Book {
        Book {
            id: 0,
            file_name: format!("{name}.epub"),
            name: name.to_string(),
            chapters: (0..words.len())
                .map(|i| ChapterInfo { index: i, title: format!("第{i}話"), characters: 100 })
                .collect(),
            texts: Vec::new(),
            analyzer: FrequencyAnalyzer::from_tokens(&[], &[], false),
            counts: Vec::new(),
            words: words.iter().map(|ws| ws.iter().map(|w| w.to_string()).collect()).collect(),
            learned: None,
        }
    }

    fn tokens(words: &[&str]) -> Vec<ParsedWord> {
        let word = |w: &&str| ParsedWord {
            base_word: w.to_string(),
            spelling: w.to_string(),
            pronunciation: String::new(),
            pos: "名詞".into(),
            span: 0..0,
            kind: WordKind::Word,
            endings: Vec::new(),
            forms: Vec::new(),
            homograph: None,
        };
        words.iter().map(word).collect()
    }

    #[test]
    fn settings_change_the_word_lists() {
        let chapters: Vec<Chapter> = (0..2).map(|i| Chapter { index: i, title: format!("第{i}話"), text: "本文".into() }).collect();
        let pending = PendingBook {
            saved: SavedBook { id: 7, name: "A".into(), file_name: "A.epub".into(), learned: Some(1) },
            chapters,
            tokens: vec![tokens(&["猫", "猫", "犬"]), tokens(&["猫", "鳥", "パン"])],
        };
        let mut book = pending.finish(&Settings { min_book_count: 1, ..Settings::default() });
        assert_eq!(book.words, [vec!["猫", "犬"], vec!["鳥"]]);
        assert_eq!(book.learned, Some(1));
        book.apply(&Settings { min_book_count: 2, ..Settings::default() });
        assert_eq!(book.words, [vec!["猫"], Vec::<&str>::new()]);
        let all = Settings { min_book_count: 1, hide_previous_words: false, hide_katakana_words: false, ..Settings::default() };
        book.apply(&all);
        assert_eq!(book.words[1], ["猫", "パン", "鳥"]);
        book.apply(&Settings { min_book_count: 1, hide_katakana_words: true, ..Settings::default() });
        assert_eq!(book.words[1], ["鳥"]);
        book.apply(&Settings { min_book_count: 1, hide_katakana_words: false, ..Settings::default() });
        assert_eq!(book.words[1], ["パン", "鳥"]);
    }

    #[test]
    fn reuses_analysis_only_with_the_same_key() {
        let chapters: Vec<Chapter> = (0..2).map(|i| Chapter { index: i, title: format!("第{i}話"), text: "本文".into() }).collect();
        let pending = PendingBook {
            saved: SavedBook { id: 7, name: "A".into(), file_name: "A.epub".into(), learned: Some(0) },
            chapters,
            tokens: vec![tokens(&["猫", "猫", "犬"]), tokens(&["猫", "鳥"])],
        };
        let settings = Settings { min_book_count: 1, ..Settings::default() };
        let book = pending.finish(&settings);
        let key = CacheKey { build: "b1", dictionary: "d1", include_particles: false };
        let bytes = book.analysis_bytes(&key);

        let restored = Book::from_analysis(book.saved(), &bytes, &key, &settings).unwrap();
        assert_eq!(restored.words, book.words);
        assert_eq!(restored.saved(), book.saved());
        assert_eq!(restored.chapters.len(), 2);

        for other in [
            CacheKey { build: "b2", ..key },
            CacheKey { dictionary: "d2", ..key },
            CacheKey { include_particles: true, ..key },
        ] {
            assert!(Book::from_analysis(book.saved(), &bytes, &other, &settings).is_none());
        }
        assert!(Book::from_analysis(book.saved(), b"not an analysis", &key, &settings).is_none());
    }

    #[test]
    fn counts_words_left_after_other_books() {
        let mut books = vec![book("A", &[&["猫", "犬"], &["鳥"]]), book("B", &[&["猫", "空"]])];
        assert_eq!(Book::remaining_counts(&books), [3, 2]);
        books[1].learned = Some(0);
        assert_eq!(Book::remaining_counts(&books), [2, 2]);
    }

    #[test]
    fn excludes_words_learned_in_other_books() {
        let mut books = vec![
            book("A", &[&["猫", "犬"], &["鳥", "魚"]]),
            book("B", &[&["猫", "空"], &["魚", "海"], &["山"]]),
        ];
        // Nothing learned yet: nothing excluded
        let stats = Book::stats(&books, 1);
        assert_eq!(stats.excluded, 0);
        assert_eq!(stats.chapters[0].filtered_words, 2);

        books[0].learned = Some(0); // learned A's first chapter, including it
        let stats = Book::stats(&books, 1);
        assert_eq!(stats.words, [vec!["空"], vec!["魚", "海"], vec!["山"]]);
        assert_eq!(stats.excluded, 1);

        books[0].learned = Some(1);
        assert_eq!(Book::stats(&books, 1).words[1], ["海"]);
        // The shown book's own progress does not filter it
        books[1].learned = Some(2);
        assert_eq!(Book::stats(&books, 1).excluded, 2);
    }

    #[test]
    fn keeps_words_by_dictionary_form_and_shows_the_books_spelling() {
        let spelled = |word: &str, spelling: &str| ParsedWord { spelling: spelling.into(), ..tokens(&[word]).remove(0) };
        let pending = PendingBook {
            saved: SavedBook { id: 1, name: "A".into(), file_name: "A.epub".into(), learned: Some(0) },
            chapters: vec![Chapter { index: 1, title: "第1話".into(), text: String::new() }],
            tokens: vec![vec![spelled("出来る", "できる"), spelled("出来る", "できる"), spelled("出来る", "出来る")]],
        };
        let book = pending.finish(&Settings { min_book_count: 1, ..Settings::default() });
        // Matched across books by its dictionary form, however this book spells it
        assert_eq!(book.words, [vec!["出来る"]]);
        assert_eq!(book.spelling("出来る"), "できる");
        let stats = Book::stats(std::slice::from_ref(&book), 0);
        let mut zip = zip::ZipArchive::new(Cursor::new(stats.words_zip(&book).unwrap())).unwrap();
        let mut text = String::new();
        zip.by_index(0).unwrap().read_to_string(&mut text).unwrap();
        assert_eq!(text, "できる\n");
    }

    #[test]
    fn zips_one_word_list_per_chapter() {
        let books = vec![book("A", &[&["猫", "犬"], &["鳥"]])];
        let stats = Book::stats(&books, 0);
        let bytes = stats.words_zip(&books[0]).unwrap();
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(zip.len(), 2);
        let mut text = String::new();
        zip.by_name("words/chapter_00_第0話.txt").unwrap().read_to_string(&mut text).unwrap();
        assert_eq!(text, "猫\n犬\n");
    }
}
