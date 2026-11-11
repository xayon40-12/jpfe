use std::collections::HashMap;
use anyhow::Result;

use crate::models::{Chapter, WordEntry};
use crate::tokenizer::{JapaneseTokenizer, ParsedWord};

/// Book-wide data of one base word (pronunciation, POS and first chapter come from its first occurrence).
pub struct BookWord {
    pub count: usize,
    pub pronunciation: String,
    pub pos: String,
    /// Position of the first chapter containing the word (titles may repeat, so they are not used as keys).
    pub first_chapter: usize,
    pub first_chapter_title: String,
}

pub struct FrequencyAnalyzer {
    pub words: HashMap<String, BookWord>,
    pub total_words: usize,
}

impl FrequencyAnalyzer {
    pub fn build(tokenizer: &mut JapaneseTokenizer, chapters: &[Chapter]) -> Result<(Self, Vec<Vec<ParsedWord>>)> {
        let mut words: HashMap<String, BookWord> = HashMap::new();
        let mut tokenized = Vec::with_capacity(chapters.len());
        for (chapter_pos, chapter) in chapters.iter().enumerate() {
            let tokens = tokenizer.tokenize(&chapter.text)?;
            for t in &tokens {
                if let Some(w) = words.get_mut(&t.base_word) {
                    w.count += 1;
                } else {
                    words.insert(t.base_word.clone(), BookWord {
                        count: 1,
                        pronunciation: t.pronunciation.clone(),
                        pos: t.pos.clone(),
                        first_chapter: chapter_pos,
                        first_chapter_title: chapter.title.clone(),
                    });
                }
            }
            tokenized.push(tokens);
        }
        let total_words = tokenized.iter().map(Vec::len).sum();
        Ok((Self { words, total_words }, tokenized))
    }

    pub fn analyze_chapter(
        &self,
        chapter_pos: usize,
        chapter_tokens: &[ParsedWord],
        target_percentage: f64,
        min_book_count: usize,
        hide_previous_words: bool,
    ) -> Vec<WordEntry> {
        let chapter_total = chapter_tokens.len() as f64;
        let mut chapter_counts: HashMap<&str, usize> = HashMap::new();
        for token in chapter_tokens {
            *chapter_counts.entry(&token.base_word).or_default() += 1;
        }

        // Sort descending by chapter count, then by whole book count, then alphabetical
        let mut sorted: Vec<(&str, usize, &BookWord)> =
            chapter_counts.into_iter().map(|(w, c)| (w, c, &self.words[w])).collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.count.cmp(&a.2.count)).then(a.0.cmp(b.0)));

        let mut entries = Vec::new();
        let mut cumulative = 0;
        for (i, (word, count, book)) in sorted.into_iter().enumerate() {
            cumulative += count;
            let cum_pct = cumulative as f64 / chapter_total * 100.0;
            // Keep rank holes for filtered words
            if book.count >= min_book_count && (!hide_previous_words || book.first_chapter == chapter_pos) {
                entries.push(WordEntry {
                    rank: i + 1,
                    word: word.to_string(),
                    pronunciation: book.pronunciation.clone(),
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
        ParsedWord { base_word: word.into(), pronunciation: String::new(), pos: "名詞".into() }
    }

    #[test]
    fn new_words_are_tracked_per_chapter_even_with_repeated_titles() {
        let first = BookWord { count: 2, pronunciation: String::new(), pos: "名詞".into(), first_chapter: 0, first_chapter_title: "同じ".into() };
        let words = HashMap::from([("猫".to_string(), first)]);
        let analyzer = FrequencyAnalyzer { words, total_words: 2 };
        let tokens = [token("猫")];
        assert_eq!(analyzer.analyze_chapter(0, &tokens, 100.0, 1, true).len(), 1);
        // Chapter 1 has the same title as chapter 0 but 猫 is no longer new there
        assert!(analyzer.analyze_chapter(1, &tokens, 100.0, 1, true).is_empty());
    }
}
