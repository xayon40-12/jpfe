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
            load_embedded_dictionary(DictionaryKind::UniDic)
                .with_context(|| "Failed to load embedded UniDic dictionary")?
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

        let mut raw_items = Vec::with_capacity(tokens.len());
        for token in tokens.iter_mut() {
            let surface = token.surface.to_string();
            let details = token.details();
            let pos = details.first().copied().unwrap_or("*").to_string();
            let lform_reading = details.get(6).copied().unwrap_or("*").to_string();
            let lemma = details.get(7).copied().unwrap_or("*").to_string();
            let orth_base = details.get(10).copied().unwrap_or("*").to_string();
            raw_items.push((surface, pos, lform_reading, lemma, orth_base));
        }
        drop(tokens);

        let mut words = Vec::with_capacity(raw_items.len());

        for (surface, pos, raw_reading, raw_lemma, raw_orth_base) in raw_items {
            // Filter out punctuation, symbols, and whitespace
            if pos == "記号" || pos == "補助記号" || pos == "空白" {
                continue;
            }

            // Filter out particles and auxiliaries if not requested
            if !self.include_particles && (pos == "助詞" || pos == "助動詞") {
                continue;
            }

            // Strip etymology annotations from UniDic lemma (e.g. "トラック-truck" -> "トラック")
            let clean_lemma = raw_lemma.split('-').next().unwrap_or(&raw_lemma);

            let (base_word, reading_seed) = if clean_lemma == "*" || clean_lemma.is_empty() {
                (surface.clone(), raw_reading)
            } else {
                match pos.as_str() {
                    "動詞" | "形容詞" => {
                        // Normalize common auxiliary/basic verbs usually written in kana
                        if clean_lemma == "為る" && surface.chars().all(is_hiragana) {
                            ("する".to_string(), "スル".to_string())
                        } else if clean_lemma == "居る" && surface.chars().all(is_hiragana) {
                            ("いる".to_string(), "イル".to_string())
                        } else if clean_lemma == "成る" && surface.chars().all(is_hiragana) {
                            ("なる".to_string(), "ナル".to_string())
                        } else if clean_lemma == "有る" && surface.chars().all(is_hiragana) {
                            ("ある".to_string(), "アル".to_string())
                        } else if clean_lemma == "無い" && surface.chars().all(is_hiragana) {
                            ("ない".to_string(), "ナイ".to_string())
                        } else if clean_lemma == "良い" && surface.chars().all(is_hiragana) {
                            ("いい".to_string(), "イイ".to_string())
                        } else {
                            // If surface used a different kanji than representative lemma (e.g. 治せる vs 直す)
                            let adapted = if clean_lemma.chars().count() > 1 && surface.chars().count() > 1 {
                                let first_surface = surface.chars().next().unwrap();
                                let first_lemma = clean_lemma.chars().next().unwrap();
                                if first_surface != first_lemma && is_cjk_kanji(first_surface) && is_cjk_kanji(first_lemma) {
                                    let remainder: String = clean_lemma.chars().skip(1).collect();
                                    format!("{}{}", first_surface, remainder)
                                } else {
                                    clean_lemma.to_string()
                                }
                            } else {
                                clean_lemma.to_string()
                            };
                            (adapted, raw_reading)
                        }
                    }
                    "名詞" => {
                        // For nouns, prefer surface orthography (e.g. 煙草, ご飯, トラック) over phonetic lemma
                        let w = if !raw_orth_base.is_empty() && raw_orth_base != "*" {
                            raw_orth_base
                        } else {
                            clean_lemma.to_string()
                        };
                        (w, raw_reading)
                    }
                    _ => {
                        // For other parts of speech (adverbs, conjunctions, etc.)
                        let w = if !raw_orth_base.is_empty() && raw_orth_base != "*" {
                            raw_orth_base
                        } else {
                            clean_lemma.to_string()
                        };
                        (w, raw_reading)
                    }
                }
            };

            // Filter out empty, whitespace, pure punctuation, symbols, or standalone dashes/ellipses
            if !is_valid_word(&base_word) {
                continue;
            }

            // Resolve canonical reading
            let canon_reading = self.resolve_reading(&base_word, &surface, &reading_seed);
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

        // If raw_reading is valid, use it
        if raw_reading != "*" && !raw_reading.is_empty() {
            self.reading_cache.insert(base.to_string(), raw_reading.to_string());
            return raw_reading.to_string();
        }

        // Segment base word to find its reading
        if let Ok(mut base_tokens) = self.segmenter.segment(Cow::Borrowed(base)) {
            if base_tokens.len() == 1 {
                let b_details = base_tokens[0].details();
                if let Some(&r) = b_details.get(6) {
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

pub fn is_cjk_kanji(c: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&c) || ('\u{3400}'..='\u{4DBF}').contains(&c)
}

pub fn is_hiragana(c: char) -> bool {
    ('\u{3041}'..='\u{3096}').contains(&c)
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
        is_cjk_kanji(c)
            // Hiragana (\u{3041}..\u{3096})
            || is_hiragana(c)
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
