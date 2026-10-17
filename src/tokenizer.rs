use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use anyhow::{Context, Result};
use lindera::dictionary::{load_embedded_dictionary, load_fs_dictionary, DictionaryKind};
use lindera::mode::Mode;
use lindera::segmenter::Segmenter;

pub struct JapaneseTokenizer {
    segmenter: Segmenter,
    reading_cache: HashMap<String, String>,
    reading_in_hiragana: bool,
    include_particles: bool,
}

#[derive(Debug, Clone)]
pub struct ParsedWord {
    pub base_word: String,
    pub pronunciation: String,
    pub pos: String,
}

impl JapaneseTokenizer {
    pub fn new(
        dict_path: Option<&Path>,
        reading_in_hiragana: bool,
        include_particles: bool,
    ) -> Result<Self> {
        let dictionary = if let Some(p) = dict_path {
            load_fs_dictionary(p)
                .with_context(|| format!("Failed to load dictionary from: {:?}", p))?
        } else {
            load_embedded_dictionary(DictionaryKind::IPADIC)
                .with_context(|| "Failed to load embedded IPADIC dictionary")?
        };

        let segmenter = Segmenter::new(Mode::Normal, dictionary, None);

        Ok(Self {
            segmenter,
            reading_cache: HashMap::new(),
            reading_in_hiragana,
            include_particles,
        })
    }

    pub fn tokenize(&mut self, text: &str) -> Result<Vec<ParsedWord>> {
        let mut tokens = self.segmenter.segment(Cow::Borrowed(text))?;
        
        // Extract raw fields first so tokens borrow of segmenter is released
        let mut raw_items = Vec::with_capacity(tokens.len());
        for token in tokens.iter_mut() {
            let surface = token.surface.to_string();
            let details = token.details();
            let pos = details.first().copied().unwrap_or("*").to_string();
            let raw_base = details.get(6).copied().unwrap_or("*").to_string();
            let raw_reading = details.get(7).copied().unwrap_or("*").to_string();
            raw_items.push((surface, pos, raw_base, raw_reading));
        }

        let mut words = Vec::with_capacity(raw_items.len());

        for (surface, pos, raw_base, raw_reading) in raw_items {
            // Filter out punctuation and symbols
            if pos == "記号" {
                continue;
            }

            // Filter out particles and auxiliaries if not requested
            if !self.include_particles && (pos == "助詞" || pos == "助動詞") {
                continue;
            }

            // Extract base form (dictionary form)
            let base_word = if raw_base == "*" || raw_base.is_empty() {
                surface.clone()
            } else {
                raw_base
            };

            // Filter out empty, whitespace, pure punctuation, symbols, or standalone dashes/ellipses
            if !is_valid_word(&base_word) {
                continue;
            }

            // Resolve canonical reading for base word
            let canon_reading = self.resolve_reading(&base_word, &surface, &raw_reading);
            let pronunciation = if self.reading_in_hiragana {
                katakana_to_hiragana(&canon_reading)
            } else {
                canon_reading
            };

            words.push(ParsedWord {
                base_word,
                pronunciation,
                pos,
            });
        }

        Ok(words)
    }

    fn resolve_reading(&mut self, base: &str, surface: &str, raw_reading: &str) -> String {
        if let Some(cached) = self.reading_cache.get(base) {
            return cached.clone();
        }

        // If base is identical to surface and reading is provided, cache and return
        if base == surface && raw_reading != "*" && !raw_reading.is_empty() {
            self.reading_cache.insert(base.to_string(), raw_reading.to_string());
            return raw_reading.to_string();
        }

        // If word is pure Katakana (e.g. fantasy names), the reading is the word itself
        if base.chars().all(is_katakana) {
            self.reading_cache.insert(base.to_string(), base.to_string());
            return base.to_string();
        }

        // If base differs from surface (e.g. conjugated verb/adjective), segment the base form
        if let Ok(mut base_tokens) = self.segmenter.segment(Cow::Borrowed(base)) {
            if base_tokens.len() == 1 {
                let b_details = base_tokens[0].details();
                if let Some(&r) = b_details.get(7) {
                    if r != "*" && !r.is_empty() {
                        self.reading_cache.insert(base.to_string(), r.to_string());
                        return r.to_string();
                    }
                }
            }
        }

        let fallback = if raw_reading != "*" && !raw_reading.is_empty() {
            raw_reading.to_string()
        } else {
            base.to_string()
        };

        self.reading_cache.insert(base.to_string(), fallback.clone());
        fallback
    }
}

pub fn is_katakana(c: char) -> bool {
    ('\u{30A1}'..='\u{30FA}').contains(&c) || c == 'ー'
}

pub fn katakana_to_hiragana(s: &str) -> String {
    s.chars()
        .map(|c| {
            if ('\u{30A1}'..='\u{30F6}').contains(&c) {
                char::from_u32(c as u32 - 0x60).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

pub fn is_valid_word(s: &str) -> bool {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return false;
    }

    // Must contain at least one valid Japanese linguistic character
    // (Kanji, Hiragana, Katakana except standalone prolonged sound marks) or alphabetic letter
    let has_linguistic_char = trimmed.chars().any(|c| {
        // Kanji (CJK Unified Ideographs + Extension A)
        ('\u{4E00}'..='\u{9FFF}').contains(&c)
            || ('\u{3400}'..='\u{4DBF}').contains(&c)
            // Hiragana (\u{3041}..\u{3096})
            || ('\u{3041}'..='\u{3096}').contains(&c)
            // Katakana (\u{30A1}..\u{30FA}, excluding standalone prolonged sound mark \u{30FC})
            || (('\u{30A1}'..='\u{30FA}').contains(&c) && c != 'ー')
            // Alphabetic letters
            || c.is_alphabetic()
    });

    if !has_linguistic_char {
        return false;
    }

    // Filter out pure numbers (e.g. ASCII digits)
    if trimmed.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }

    true
}
