//! Loaded books, their reading progress, and the statistics shown for them.

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Write};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use jpfe::epub::extract_epub_chapters_from_bytes;
use jpfe::frequency::{BookWord, ChapterCounts, FrequencyAnalyzer, ListOptions, Spelling};
use jpfe::models::{chapter_file_stem, Chapter, ChapterStat};
use jpfe::pdf::extract_pdf_chapters_from_bytes;
use jpfe::tokenizer::{is_katakana_word, JapaneseTokenizer, ParsedWord};
#[cfg(test)]
use jpfe::tokenizer::WordKind;
use web_time::Instant;

use crate::timeline;

/// Word-list settings, with the same meaning and defaults as the CLI options.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// `--target-percentage`: each chapter's list stops once its words cover this share of the chapter
    pub target_percentage: f64,
    /// `--min-book-count`: words seen fewer times in the whole book are left out
    pub min_book_count: usize,
    /// `--min-chapters`: words found in fewer chapters of the book are left out
    pub min_chapters: usize,
    /// Opposite of `--show-previous-words`: only list words new to the book
    pub hide_previous_words: bool,
    /// `--include-particles`; changing it analyzes the books again (which takes them from their text)
    pub include_particles: bool,
    /// Leave out words written only in katakana (mostly loanwords)
    pub hide_katakana_words: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let ListOptions { target_percentage, min_book_count, min_chapters, hide_previous_words } = ListOptions::default();
        Self { target_percentage, min_book_count, min_chapters, hide_previous_words, include_particles: false, hide_katakana_words: true }
    }
}

impl Settings {
    fn list_options(&self) -> ListOptions {
        ListOptions {
            target_percentage: self.target_percentage,
            min_book_count: self.min_book_count,
            min_chapters: self.min_chapters,
            hide_previous_words: self.hide_previous_words,
        }
    }
}

pub struct ChapterInfo {
    pub index: usize,
    pub title: String,
    pub characters: usize,
}

impl ChapterInfo {
    fn of(chapter: &Chapter) -> Self {
        Self { index: chapter.index, characters: chapter.text.chars().count(), title: chapter.title.clone() }
    }
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

/// A stored analysis is a sequence of postcard-encoded parts, so that it can be decoded a few parts per frame:
/// decoding a 56 MB analysis in one call froze Safari for 2.4 s. First a [`StoredHeader`], then the book's
/// words in parts of [`WORDS_PER_PART`] (`Vec<StoredWord>`), then a [`StoredChapter`] for each chapter. The
/// encoder writes the same fields as tuples of references, in this order.
#[derive(Deserialize)]
struct StoredHeader {
    build: String,
    dictionary: String,
    include_particles: bool,
    total_words: usize,
    /// Number of words, to reserve their table at once
    words: usize,
    /// Chapter titles, also each word's first chapter title (not stored with every word)
    titles: Vec<String>,
}

#[derive(Deserialize)]
struct StoredWord {
    key: String,
    count: usize,
    spellings: Vec<Spelling>,
    pronunciation: String,
    pos: String,
    chapters: usize,
    first_chapter: usize,
}

#[derive(Deserialize)]
struct StoredChapter {
    index: usize,
    text: String,
    total: usize,
    /// Occurrences of each word, by its position among the stored words (rather than its text)
    counts: Vec<(u32, u32)>,
}

/// Words in each part of a stored analysis.
const WORDS_PER_PART: usize = 2000;

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
        let chapters = texts.iter().map(ChapterInfo::of).collect();
        Self::with_words(saved, chapters, texts, analyzer, counts, words)
    }

    fn with_words(
        saved: SavedBook,
        chapters: Vec<ChapterInfo>,
        texts: Vec<Chapter>,
        analyzer: FrequencyAnalyzer,
        counts: Vec<ChapterCounts>,
        words: Vec<Vec<String>>,
    ) -> Self {
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
        let mut out = Vec::new();
        let mut part = |bytes: postcard::Result<Vec<u8>>| out.extend(bytes.unwrap_or_default());
        let titles: Vec<&str> = self.texts.iter().map(|c| c.title.as_str()).collect();
        let words: Vec<(&String, &BookWord)> = self.analyzer.words.iter().collect();
        part(postcard::to_allocvec(&(key.build, key.dictionary, key.include_particles, self.analyzer.total_words, words.len(), &titles)));
        for chunk in words.chunks(WORDS_PER_PART) {
            let chunk: Vec<_> =
                chunk.iter().map(|(k, w)| (k.as_str(), w.count, &w.spellings, w.pronunciation.as_str(), w.pos.as_str(), w.chapters, w.first_chapter)).collect();
            part(postcard::to_allocvec(&chunk));
        }
        let ids: HashMap<&str, u32> = words.iter().enumerate().map(|(i, (k, _))| (k.as_str(), i as u32)).collect();
        for (chapter, counts) in self.texts.iter().zip(&self.counts) {
            let by_id: Vec<(u32, u32)> = counts.counts.iter().filter_map(|(k, &n)| Some((*ids.get(k.as_str())?, n as u32))).collect();
            part(postcard::to_allocvec(&(chapter.index, chapter.text.as_str(), counts.total, &by_id)));
        }
        out
    }

    /// The book from a stored analysis, if it was made with the same `key`: decoded and given its word lists a
    /// few parts at a time ([`RestoringBook::step`]).
    pub fn from_analysis(saved: SavedBook, bytes: Vec<u8>, key: &CacheKey, settings: &Settings) -> Option<RestoringBook> {
        let (header, rest) = postcard::take_from_bytes::<StoredHeader>(&bytes).ok()?;
        if header.build != key.build || header.dictionary != key.dictionary || header.include_particles != key.include_particles || header.titles.is_empty() {
            return None;
        }
        let at = bytes.len() - rest.len();
        let chapters = header.titles.len();
        let analyzer = FrequencyAnalyzer { words: HashMap::with_capacity(header.words), total_words: header.total_words, chapters };
        Some(RestoringBook {
            saved,
            settings: *settings,
            size: bytes.len(),
            bytes,
            at,
            words_left: header.words,
            keys: Vec::with_capacity(header.words),
            titles: header.titles,
            analyzer,
            texts: Vec::with_capacity(chapters),
            infos: Vec::with_capacity(chapters),
            counts: Vec::with_capacity(chapters),
            words: Vec::with_capacity(chapters),
            started: Instant::now(),
            work_ms: 0.0,
            longest_ms: 0.0,
            frames: 0,
        })
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
        PendingBook::new(saved, self.texts)
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
/// (the web version has no threads). Each chapter's words are counted as soon as it is tokenized and its
/// tokens dropped: kept, the tokens of a long book (Re:ゼロ, 4 million) took more memory than an iPhone gives
/// a page.
pub struct PendingBook {
    /// Id, names and the reader's progress, kept when a book is tokenized again
    pub saved: SavedBook,
    chapters: Vec<Chapter>,
    /// Words of the chapters tokenized so far
    analyzer: FrequencyAnalyzer,
    counts: Vec<ChapterCounts>,
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
        Ok(Self::new(saved, chapters))
    }

    fn new(saved: SavedBook, chapters: Vec<Chapter>) -> Self {
        Self { saved, chapters, analyzer: FrequencyAnalyzer::default(), counts: Vec::new() }
    }

    /// Tokenize again from the first chapter (after a tokenizer setting changed).
    pub fn restart(&mut self) {
        self.analyzer = FrequencyAnalyzer::default();
        self.counts.clear();
    }

    pub fn progress(&self) -> f32 {
        self.counts.len() as f32 / self.chapters.len() as f32
    }

    /// Tokenize and count chapters until `deadline`; true once every chapter is done.
    pub fn step(&mut self, tokenizer: &mut JapaneseTokenizer, include_particles: bool, deadline: Instant) -> Result<bool> {
        while let Some(chapter) = self.chapters.get(self.counts.len()) {
            let tokens = tokenizer.tokenize(&chapter.text)?;
            self.add_chapter(&tokens, include_particles);
            if Instant::now() >= deadline {
                break;
            }
        }
        Ok(self.counts.len() == self.chapters.len())
    }

    /// Count the words of the next chapter from its tokens.
    fn add_chapter(&mut self, tokens: &[ParsedWord], include_particles: bool) {
        let title = &self.chapters[self.counts.len()].title;
        self.analyzer.add_chapter(title, tokens, include_particles);
        self.counts.push(ChapterCounts::new(tokens, include_particles));
    }

    pub fn finish(self, settings: &Settings) -> Book {
        Book::new(self.saved, self.chapters, self.analyzer, self.counts, settings)
    }
}

/// A book being restored from its stored analysis: its parts decoded, then its word lists built, a few at a
/// time, so that the window keeps redrawing (decoding a large analysis in one call froze Safari for seconds).
pub struct RestoringBook {
    saved: SavedBook,
    /// The settings the lists are built with
    settings: Settings,
    /// The stored analysis, and where its next part starts
    bytes: Vec<u8>,
    at: usize,
    size: usize,
    /// Words still to decode, and the decoded words' keys by their position
    words_left: usize,
    keys: Vec<String>,
    titles: Vec<String>,
    analyzer: FrequencyAnalyzer,
    texts: Vec<Chapter>,
    /// Titles and lengths of the decoded chapters (counting the characters of a long book's text takes long too)
    infos: Vec<ChapterInfo>,
    counts: Vec<ChapterCounts>,
    words: Vec<Vec<String>>,
    /// For the timeline: when it started, the time spent on it (ms), in how many frames, at most in one
    started: Instant,
    work_ms: f64,
    longest_ms: f64,
    frames: u32,
}

impl RestoringBook {
    pub fn id(&self) -> u64 {
        self.saved.id
    }

    /// Decode parts, then build word lists, until `deadline` (one part or list at least): `Some(true)` once the
    /// book is complete, `None` if the stored analysis is damaged.
    pub fn step(&mut self, deadline: Instant) -> Option<bool> {
        let start = Instant::now();
        let done = loop {
            let done = self.next()?;
            if done || Instant::now() >= deadline {
                break done;
            }
        };
        let took = timeline::since(start);
        self.work_ms += took;
        self.longest_ms = self.longest_ms.max(took);
        self.frames += 1;
        Some(done)
    }

    /// One part decoded, or one word list built; true once there is nothing left.
    fn next(&mut self) -> Option<bool> {
        let options = self.settings.list_options();
        if self.words_left > 0 {
            let (part, rest) = postcard::take_from_bytes::<Vec<StoredWord>>(&self.bytes[self.at..]).ok()?;
            self.at = self.bytes.len() - rest.len();
            if part.is_empty() || part.len() > self.words_left {
                return None;
            }
            self.words_left -= part.len();
            for w in part {
                let first_chapter_title = self.titles.get(w.first_chapter).cloned().unwrap_or_default();
                self.keys.push(w.key.clone());
                let word = BookWord {
                    count: w.count,
                    spellings: w.spellings,
                    pronunciation: w.pronunciation,
                    pos: w.pos,
                    chapters: w.chapters,
                    first_chapter: w.first_chapter,
                    first_chapter_title,
                };
                self.analyzer.words.insert(w.key, word);
            }
        } else if self.texts.len() < self.titles.len() {
            let (stored, rest) = postcard::take_from_bytes::<StoredChapter>(&self.bytes[self.at..]).ok()?;
            self.at = self.bytes.len() - rest.len();
            let mut counts = HashMap::with_capacity(stored.counts.len());
            for (id, n) in stored.counts {
                counts.insert(self.keys.get(id as usize)?.clone(), n as usize);
            }
            let title = self.titles[self.texts.len()].clone();
            let chapter = Chapter { index: stored.index, title, text: stored.text };
            self.infos.push(ChapterInfo::of(&chapter));
            self.texts.push(chapter);
            self.counts.push(ChapterCounts { total: stored.total, counts });
            if self.texts.len() == self.titles.len() {
                // Every part is decoded: the stored analysis is no longer needed
                self.bytes = Vec::new();
            }
        } else if let Some(counts) = self.counts.get(self.words.len()) {
            self.words.push(word_list(&self.analyzer, self.words.len(), counts, &self.settings, &options));
        }
        Some(self.words_left == 0 && self.words.len() == self.titles.len())
    }

    /// Start over from the chapter texts (the tokenizer setting changed while it was restored), if they are
    /// decoded; `None` otherwise, to analyze it from its file.
    pub fn retokenize(self) -> Option<PendingBook> {
        (self.texts.len() == self.titles.len()).then(|| PendingBook::new(self.saved, self.texts))
    }

    /// The book, with its lists for `settings` (built again if they changed meanwhile).
    pub fn finish(self, settings: &Settings) -> Book {
        timeline::note(format!(
            "{}: restored from {} in {:.0} ms of work over {:.1} s ({} frames, at most {:.0} ms in one)",
            self.saved.name,
            timeline::size(self.size),
            self.work_ms,
            self.started.elapsed().as_secs_f64(),
            self.frames,
            self.longest_ms
        ));
        let mut book = Book::with_words(self.saved, self.infos, self.texts, self.analyzer, self.counts, self.words);
        if self.settings != *settings {
            book.apply(settings);
        }
        book
    }
}

fn word_lists(analyzer: &FrequencyAnalyzer, counts: &[ChapterCounts], s: &Settings) -> Vec<Vec<String>> {
    let options = s.list_options();
    counts.iter().enumerate().map(|(i, c)| word_list(analyzer, i, c, s, &options)).collect()
}

/// The words to learn in chapter `i`.
fn word_list(analyzer: &FrequencyAnalyzer, i: usize, counts: &ChapterCounts, s: &Settings, options: &ListOptions) -> Vec<String> {
    analyzer
        .analyze_counts(i, counts, options)
        .into_iter()
        // Like the CLI's other filters, this leaves the coverage cut-off unchanged
        .filter(|e| !(s.hide_katakana_words && is_katakana_word(&e.word)))
        .map(|e| e.key)
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

    /// A book whose chapters are already tokenized into `tokens`.
    fn pending(saved: SavedBook, chapters: Vec<Chapter>, tokens: Vec<Vec<ParsedWord>>) -> PendingBook {
        let mut book = PendingBook::new(saved, chapters);
        for t in &tokens {
            book.add_chapter(t, false);
        }
        book
    }

    fn tokens(words: &[&str]) -> Vec<ParsedWord> {
        let word = |w: &&str| ParsedWord {
            base_word: w.to_string(),
            spelling: w.to_string(),
            pronunciation: String::new(),
            pos: "名詞".into(),
            span: 0..0,
            kind: WordKind::Word,
            affixes: Vec::new(),
            forms: Vec::new(),
            homograph: None,
            written_reading: None,
            proper_noun: false,
        };
        words.iter().map(word).collect()
    }

    #[test]
    fn settings_change_the_word_lists() {
        let chapters: Vec<Chapter> = (0..2).map(|i| Chapter { index: i, title: format!("第{i}話"), text: "本文".into() }).collect();
        let pending = pending(
            SavedBook { id: 7, name: "A".into(), file_name: "A.epub".into(), learned: Some(1) },
            chapters,
            vec![tokens(&["猫", "猫", "犬"]), tokens(&["猫", "鳥", "パン"])],
        );
        let mut book = pending.finish(&Settings { min_book_count: 1, min_chapters: 1, ..Settings::default() });
        assert_eq!(book.words, [vec!["猫", "犬"], vec!["鳥"]]);
        assert_eq!(book.learned, Some(1));
        book.apply(&Settings { min_book_count: 2, min_chapters: 1, ..Settings::default() });
        assert_eq!(book.words, [vec!["猫"], Vec::<&str>::new()]);
        let all = Settings { min_book_count: 1, min_chapters: 1, hide_previous_words: false, hide_katakana_words: false, ..Settings::default() };
        book.apply(&all);
        assert_eq!(book.words[1], ["猫", "パン", "鳥"]);
        book.apply(&Settings { min_book_count: 1, min_chapters: 1, hide_katakana_words: true, ..Settings::default() });
        assert_eq!(book.words[1], ["鳥"]);
        book.apply(&Settings { min_book_count: 1, min_chapters: 1, hide_katakana_words: false, ..Settings::default() });
        assert_eq!(book.words[1], ["パン", "鳥"]);
    }

    #[test]
    fn reuses_analysis_only_with_the_same_key() {
        let chapters: Vec<Chapter> = (0..2).map(|i| Chapter { index: i, title: format!("第{i}話"), text: "本文".into() }).collect();
        let pending = pending(
            SavedBook { id: 7, name: "A".into(), file_name: "A.epub".into(), learned: Some(0) },
            chapters,
            vec![tokens(&["猫", "猫", "犬"]), tokens(&["猫", "鳥"])],
        );
        let settings = Settings { min_book_count: 1, min_chapters: 1, ..Settings::default() };
        let book = pending.finish(&settings);
        let key = CacheKey { build: "b1", dictionary: "d1", include_particles: false };
        let bytes = book.analysis_bytes(&key);

        // Decoded a part at a time, then given its word lists a chapter at a time, here with a deadline already
        // past: the words, the two chapters, then the two lists
        let mut restoring = Book::from_analysis(book.saved(), bytes.clone(), &key, &settings).unwrap();
        let mut steps = 1;
        while !restoring.step(Instant::now()).unwrap() {
            steps += 1;
        }
        assert_eq!(steps, 5);
        let restored = restoring.finish(&settings);
        assert_eq!(restored.words, book.words);
        assert_eq!(restored.saved(), book.saved());
        assert_eq!(restored.chapters.len(), 2);
        // Settings changed while it was restored: the lists follow them
        let stricter = Settings { min_book_count: 2, ..settings };
        let mut restoring = Book::from_analysis(book.saved(), bytes.clone(), &key, &settings).unwrap();
        while !restoring.step(Instant::now()).unwrap() {}
        let mut expected = book;
        let restored = restoring.finish(&stricter);
        expected.apply(&stricter);
        assert_eq!(restored.words, expected.words);
        let book = expected;

        for other in [
            CacheKey { build: "b2", ..key },
            CacheKey { dictionary: "d2", ..key },
            CacheKey { include_particles: true, ..key },
        ] {
            assert!(Book::from_analysis(book.saved(), bytes.clone(), &other, &settings).is_none());
        }
        assert!(Book::from_analysis(book.saved(), b"not an analysis".to_vec(), &key, &settings).is_none());
        // Cut short: the header is fine, a later part is not
        let mut cut = Book::from_analysis(book.saved(), bytes[..bytes.len() - 3].to_vec(), &key, &settings).unwrap();
        assert_eq!((0..10).find_map(|_| match cut.step(Instant::now()) { Some(false) => None, other => Some(other) }), Some(None));
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
        let pending = pending(
            SavedBook { id: 1, name: "A".into(), file_name: "A.epub".into(), learned: Some(0) },
            vec![Chapter { index: 1, title: "第1話".into(), text: String::new() }],
            vec![vec![spelled("出来る", "できる"), spelled("出来る", "できる"), spelled("出来る", "出来る")]],
        );
        let book = pending.finish(&Settings { min_book_count: 1, min_chapters: 1, ..Settings::default() });
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
