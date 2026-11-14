use std::collections::HashMap;
use anyhow::Result;

use crate::models::{Chapter, WordEntry};
use crate::tokenizer::{JapaneseTokenizer, ParsedWord};
#[cfg(test)]
use crate::tokenizer::WordKind;

/// Book-wide data of one base word (pronunciation, POS and first chapter come from its first occurrence).
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BookWord {
    pub count: usize,
    /// How the book writes the word, and how often (see [`ParsedWord::spelling`]), in order of appearance
    pub spellings: Vec<Spelling>,
    pub pronunciation: String,
    pub pos: String,
    /// Position of the first chapter containing the word (titles may repeat, so they are not used as keys).
    pub first_chapter: usize,
    pub first_chapter_title: String,
}

/// One way a book writes a word, with its reading.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Spelling {
    pub spelling: String,
    pub pronunciation: String,
    pub count: usize,
}

impl BookWord {
    /// The spelling the book uses most (the first one used, on a tie), and its reading.
    pub fn usual_spelling(&self) -> Option<&Spelling> {
        self.spellings.iter().rev().max_by_key(|s| s.count)
    }
}

/// Occurrences of each base word word lists count in one chapter (see [`ParsedWord::is_listed`]), conjugation
/// endings included (see [`ParsedWord::with_endings`]).
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ChapterCounts {
    pub total: usize,
    pub counts: HashMap<String, usize>,
}

impl ChapterCounts {
    pub fn new(tokens: &[ParsedWord], include_particles: bool) -> Self {
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut total = 0;
        for token in tokens.iter().flat_map(ParsedWord::with_endings).filter(|t| t.is_listed(include_particles)) {
            *counts.entry(token.key().into_owned()).or_default() += 1;
            total += 1;
        }
        Self { total, counts }
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FrequencyAnalyzer {
    pub words: HashMap<String, BookWord>,
    pub total_words: usize,
}

impl FrequencyAnalyzer {
    /// Tokenize the chapters and count their words; particles and auxiliary verbs only with `include_particles`.
    pub fn build(tokenizer: &mut JapaneseTokenizer, chapters: &[Chapter], include_particles: bool) -> Result<(Self, Vec<Vec<ParsedWord>>)> {
        let tokenized = chapters.iter().map(|c| tokenizer.tokenize(&c.text)).collect::<Result<Vec<_>>>()?;
        Ok((Self::from_tokens(chapters, &tokenized, include_particles), tokenized))
    }

    /// Book-wide counts from already tokenized chapters (`tokenized[i]` belongs to `chapters[i]`).
    pub fn from_tokens(chapters: &[Chapter], tokenized: &[Vec<ParsedWord>], include_particles: bool) -> Self {
        let mut words: HashMap<String, BookWord> = HashMap::new();
        for (chapter_pos, (chapter, tokens)) in chapters.iter().zip(tokenized).enumerate() {
            for t in tokens.iter().flat_map(ParsedWord::with_endings).filter(|t| t.is_listed(include_particles)) {
                let word = words.entry(t.key().into_owned()).or_insert_with(|| BookWord {
                    count: 0,
                    spellings: Vec::new(),
                    pronunciation: t.pronunciation.clone(),
                    pos: t.pos.clone(),
                    first_chapter: chapter_pos,
                    first_chapter_title: chapter.title.clone(),
                });
                word.count += 1;
                match word.spellings.iter_mut().find(|s| s.spelling == t.spelling) {
                    Some(spelling) => spelling.count += 1,
                    None => word.spellings.push(Spelling { spelling: t.spelling.clone(), pronunciation: t.pronunciation.clone(), count: 1 }),
                }
            }
        }
        let total_words = words.values().map(|w| w.count).sum();
        Self { words, total_words }
    }

    pub fn analyze_chapter(
        &self,
        chapter_pos: usize,
        chapter_tokens: &[ParsedWord],
        target_percentage: f64,
        min_book_count: usize,
        hide_previous_words: bool,
        include_particles: bool,
    ) -> Vec<WordEntry> {
        let counts = ChapterCounts::new(chapter_tokens, include_particles);
        self.analyze_counts(chapter_pos, &counts, target_percentage, min_book_count, hide_previous_words)
    }

    /// Same as [`Self::analyze_chapter`], from the chapter's word counts.
    pub fn analyze_counts(
        &self,
        chapter_pos: usize,
        chapter: &ChapterCounts,
        target_percentage: f64,
        min_book_count: usize,
        hide_previous_words: bool,
    ) -> Vec<WordEntry> {
        let chapter_total = chapter.total as f64;

        // Sort descending by chapter count, then by whole book count, then alphabetical
        let mut sorted: Vec<(&str, usize, &BookWord)> =
            chapter.counts.iter().map(|(w, &c)| (w.as_str(), c, &self.words[w])).collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.count.cmp(&a.2.count)).then(a.0.cmp(b.0)));

        let mut entries = Vec::new();
        let mut cumulative = 0;
        for (i, (word, count, book)) in sorted.into_iter().enumerate() {
            cumulative += count;
            let cum_pct = cumulative as f64 / chapter_total * 100.0;
            // Keep rank holes for filtered words
            if book.count >= min_book_count && (!hide_previous_words || book.first_chapter == chapter_pos) {
                let usual = book.usual_spelling();
                entries.push(WordEntry {
                    rank: i + 1,
                    key: word.to_string(),
                    word: usual.map_or(word, |s| &s.spelling).to_string(),
                    pronunciation: usual.map_or(&book.pronunciation, |s| &s.pronunciation).clone(),
                    pos: book.pos.clone(),
                    occurrences: count,
                    chapter_percent: format!("{:.4}%", count as f64 / chapter_total * 100.0),
                    cumulative_percent: format!("{:.2}%", cum_pct),
                    book_occurrences: book.count,
                    book_percent: format!("{:.4}%", book.count as f64 / self.total_words as f64 * 100.0),
                    first_appearance_chapter: book.first_chapter_title.clone(),
                });
            }
            // Stop once the cumulative percentage reaches the target (if target < 100)
            if target_percentage < 100.0 && cum_pct >= target_percentage {
                break;
            }
        }
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(word: &str) -> ParsedWord {
        ParsedWord { base_word: word.into(), spelling: word.into(), pronunciation: String::new(), pos: "名詞".into(), span: 0..0, kind: WordKind::Word, endings: Vec::new(), forms: Vec::new(), homograph: None }
    }

    fn spelled(word: &str, spelling: &str) -> ParsedWord {
        ParsedWord { spelling: spelling.into(), pronunciation: spelling.into(), ..token(word) }
    }

    #[test]
    fn lists_each_word_in_the_spelling_its_book_uses_most() {
        let chapters: Vec<Chapter> = (0..2).map(|i| Chapter { index: i, title: format!("{i}"), text: String::new() }).collect();
        let tokens = [
            vec![spelled("出来る", "できる"), spelled("出来る", "出来る"), token("猫")],
            vec![spelled("出来る", "できる")],
        ];
        let analyzer = FrequencyAnalyzer::from_tokens(&chapters, &tokens, false);
        let entries = analyzer.analyze_chapter(0, &tokens[0], 100.0, 1, false, false);
        // One word, counted under its dictionary form and shown as the book writes it most
        assert_eq!(entries.iter().map(|e| (e.key.as_str(), e.word.as_str())).collect::<Vec<_>>(), [("出来る", "できる"), ("猫", "猫")]);
        assert_eq!((entries[0].occurrences, entries[0].book_occurrences), (2, 3));
        assert_eq!(entries[0].pronunciation, "できる");
    }

    #[test]
    fn new_words_are_tracked_per_chapter_even_with_repeated_titles() {
        let first = BookWord { count: 2, spellings: Vec::new(), pronunciation: String::new(), pos: "名詞".into(), first_chapter: 0, first_chapter_title: "同じ".into() };
        let words = HashMap::from([("猫".to_string(), first)]);
        let analyzer = FrequencyAnalyzer { words, total_words: 2 };
        let tokens = [token("猫")];
        assert_eq!(analyzer.analyze_chapter(0, &tokens, 100.0, 1, true, false).len(), 1);
        // Chapter 1 has the same title as chapter 0 but 猫 is no longer new there
        assert!(analyzer.analyze_chapter(1, &tokens, 100.0, 1, true, false).is_empty());
    }
}
