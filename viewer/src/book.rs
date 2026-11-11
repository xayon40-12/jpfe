//! Loaded books, their reading progress, and the statistics shown for them.

use std::collections::HashSet;
use std::io::{Cursor, Write};

use anyhow::{bail, Result};
use jpfe::epub::extract_epub_chapters_from_bytes;
use jpfe::frequency::FrequencyAnalyzer;
use jpfe::models::{chapter_file_stem, vocabulary_ratio, Chapter, ChapterStat};
use jpfe::pdf::extract_pdf_chapters_from_bytes;
use jpfe::tokenizer::{JapaneseTokenizer, ParsedWord};
use web_time::Instant;

/// Same word-list settings as the CLI defaults.
const TARGET_PERCENTAGE: f64 = 99.0;
const MIN_BOOK_COUNT: usize = 3;

pub struct ChapterInfo {
    pub index: usize,
    pub title: String,
    pub characters: usize,
}

pub struct Book {
    pub name: String,
    pub chapters: Vec<ChapterInfo>,
    /// Words to learn in each chapter, as in the CLI's `words/` files (only words new to the book).
    pub words: Vec<Vec<String>>,
    /// Position of the last chapter whose words the reader has learned; `None` before the first one.
    pub learned: Option<usize>,
}

impl Book {
    /// Words of every chapter up to and including the learned one.
    pub fn learned_words(&self) -> impl Iterator<Item = &str> {
        let count = self.learned.map_or(0, |pos| pos + 1);
        self.words.iter().take(count).flatten().map(String::as_str)
    }

    pub fn chapter_label(&self, pos: usize) -> String {
        let c = &self.chapters[pos];
        format!("{}: {}", c.index, c.title)
    }

    /// Words of each chapter, minus the words already learned in the other books.
    pub fn stats(books: &[Book], shown: usize) -> BookStats {
        let known: HashSet<&str> = books
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != shown)
            .flat_map(|(_, b)| b.learned_words())
            .collect();
        let book = &books[shown];
        let words: Vec<Vec<String>> = book
            .words
            .iter()
            .map(|ws| ws.iter().filter(|w| !known.contains(w.as_str())).cloned().collect())
            .collect();
        let excluded = book.words.iter().map(Vec::len).sum::<usize>() - words.iter().map(Vec::len).sum::<usize>();
        // Normalized to the first chapter, like the CLI
        let baseline = vocabulary_ratio(words[0].len(), book.chapters[0].characters);
        let chapters = book
            .chapters
            .iter()
            .zip(&words)
            .map(|(c, w)| ChapterStat::new(c.index, &c.title, w.len(), c.characters, baseline))
            .collect();
        BookStats { chapters, words, excluded }
    }
}

pub struct BookStats {
    pub chapters: Vec<ChapterStat>,
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
                writeln!(zip, "{word}")?;
            }
        }
        Ok(zip.finish()?.into_inner())
    }
}

/// A book being tokenized a few chapters per frame, so the window stays responsive
/// (the web version has no threads).
pub struct PendingBook {
    pub name: String,
    chapters: Vec<Chapter>,
    tokens: Vec<Vec<ParsedWord>>,
}

impl PendingBook {
    pub fn parse(file_name: &str, bytes: Vec<u8>) -> Result<Self> {
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
        Ok(Self { name, chapters, tokens: Vec::new() })
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

    pub fn finish(self) -> Book {
        let analyzer = FrequencyAnalyzer::from_tokens(&self.chapters, &self.tokens);
        let words = self
            .tokens
            .iter()
            .enumerate()
            .map(|(i, tokens)| {
                analyzer
                    .analyze_chapter(i, tokens, TARGET_PERCENTAGE, MIN_BOOK_COUNT, true)
                    .into_iter()
                    .map(|e| e.word)
                    .collect()
            })
            .collect();
        let chapters = self
            .chapters
            .into_iter()
            .map(|c| ChapterInfo { index: c.index, characters: c.text.chars().count(), title: c.title })
            .collect();
        Book { name: self.name, chapters, words, learned: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn book(name: &str, words: &[&[&str]]) -> Book {
        Book {
            name: name.to_string(),
            chapters: (0..words.len())
                .map(|i| ChapterInfo { index: i, title: format!("第{i}話"), characters: 100 })
                .collect(),
            words: words.iter().map(|ws| ws.iter().map(|w| w.to_string()).collect()).collect(),
            learned: None,
        }
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
