use std::collections::HashMap;
use anyhow::Result;

use crate::models::{Chapter, WordEntry};
use crate::tokenizer::JapaneseTokenizer;

pub struct FrequencyAnalyzer {
    pub book_word_counts: HashMap<String, usize>,
    pub book_total_words: usize,
    pub word_pronunciations: HashMap<String, String>,
    pub word_pos: HashMap<String, String>,
    pub word_first_chapter: HashMap<String, String>,
}

impl FrequencyAnalyzer {
    pub fn build(tokenizer: &mut JapaneseTokenizer, all_chapters: &[Chapter]) -> Result<(Self, Vec<Vec<crate::tokenizer::ParsedWord>>)> {
        let mut book_word_counts: HashMap<String, usize> = HashMap::new();
        let mut book_total_words: usize = 0;
        let mut word_pronunciations: HashMap<String, String> = HashMap::new();
        let mut word_pos: HashMap<String, String> = HashMap::new();
        let mut word_first_chapter: HashMap<String, String> = HashMap::new();

        let mut tokenized_chapters = Vec::with_capacity(all_chapters.len());

        for chapter in all_chapters {
            let tokens = tokenizer.tokenize(&chapter.text)?;
            for token in &tokens {
                book_total_words += 1;
                *book_word_counts.entry(token.base_word.clone()).or_insert(0) += 1;

                word_pronunciations
                    .entry(token.base_word.clone())
                    .or_insert_with(|| token.pronunciation.clone());

                word_pos
                    .entry(token.base_word.clone())
                    .or_insert_with(|| token.pos.clone());

                word_first_chapter
                    .entry(token.base_word.clone())
                    .or_insert_with(|| chapter.title.clone());
            }
            tokenized_chapters.push(tokens);
        }

        Ok((
            Self {
                book_word_counts,
                book_total_words,
                word_pronunciations,
                word_pos,
                word_first_chapter,
            },
            tokenized_chapters,
        ))
    }

    pub fn analyze_chapter(
        &self,
        chapter_title: &str,
        chapter_tokens: &[crate::tokenizer::ParsedWord],
        target_percentage: f64,
        min_book_count: usize,
        hide_previous_words: bool,
    ) -> Vec<WordEntry> {
        let chapter_total = chapter_tokens.len();
        if chapter_total == 0 {
            return Vec::new();
        }

        // Aggregate words in this chapter
        let mut chapter_counts: HashMap<&str, usize> = HashMap::new();
        for token in chapter_tokens {
            *chapter_counts.entry(token.base_word.as_str()).or_insert(0) += 1;
        }

        // Convert to sorted vector
        let mut sorted_words: Vec<(&str, usize)> = chapter_counts.into_iter().collect();
        // Sort descending by chapter count, then by whole book count, then alphabetical
        sorted_words.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| {
                    let book_a = self.book_word_counts.get(a.0).copied().unwrap_or(0);
                    let book_b = self.book_word_counts.get(b.0).copied().unwrap_or(0);
                    book_b.cmp(&book_a)
                })
                .then_with(|| a.0.cmp(b.0))
        });

        let mut entries = Vec::new();
        let mut cumulative_count: usize = 0;
        let mut rank = 0;

        for (word, count) in sorted_words {
            rank += 1;
            cumulative_count += count;

            let chapter_freq = count as f64 / chapter_total as f64;
            let chapter_pct = chapter_freq * 100.0;
            let cum_pct = (cumulative_count as f64 / chapter_total as f64) * 100.0;

            let book_count = self.book_word_counts.get(word).copied().unwrap_or(count);

            // Filter out words that appear less than min_book_count times across the entire book
            if book_count < min_book_count {
                if target_percentage < 100.0 && cum_pct >= target_percentage {
                    break;
                }
                continue;
            }

            let first_chapter = self
                .word_first_chapter
                .get(word)
                .cloned()
                .unwrap_or_else(|| "Unknown".to_string());

            // If hiding words that appeared in previous chapters, skip lines for words whose first appearance is earlier
            if hide_previous_words && first_chapter != chapter_title {
                if target_percentage < 100.0 && cum_pct >= target_percentage {
                    break;
                }
                continue;
            }

            let book_freq = if self.book_total_words > 0 {
                book_count as f64 / self.book_total_words as f64
            } else {
                0.0
            };
            let book_pct = book_freq * 100.0;

            let pronunciation = self
                .word_pronunciations
                .get(word)
                .cloned()
                .unwrap_or_else(|| word.to_string());

            let pos = self
                .word_pos
                .get(word)
                .cloned()
                .unwrap_or_else(|| "Unknown".to_string());

            entries.push(WordEntry {
                rank,
                word: word.to_string(),
                pronunciation,
                pos,
                occurrences: count,
                chapter_frequency: chapter_freq,
                chapter_percent: format!("{:.4}%", chapter_pct),
                cumulative_percent: format!("{:.2}%", cum_pct),
                book_occurrences: book_count,
                book_frequency: book_freq,
                book_percent: format!("{:.4}%", book_pct),
                first_appearance_chapter: first_chapter,
            });

            // Stop once cumulative percentage reaches or exceeds target percentage (if target < 100.0)
            if target_percentage < 100.0 && cum_pct >= target_percentage {
                break;
            }
        }

        entries
    }
}
