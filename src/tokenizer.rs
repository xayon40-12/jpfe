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

            // Filter out bound affixes (suffixes and prefixes) which cannot stand as independent words
            if pos == "接尾辞" || pos == "接頭辞" {
                continue;
            }

            // Filter out particles and auxiliaries by default unless requested
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

            // Filter out spurious colloquial contractions, slang fragments, archaic auxiliaries, and single-kana fragments
            if is_problematic_word(&base_word, &pos) {
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

pub fn is_problematic_word(word: &str, pos: &str) -> bool {
    // 1. Bound affixes (prefixes and suffixes) cannot stand alone as independent words
    if pos == "接尾辞" || pos == "接頭辞" {
        return true;
    }

    // 2. Single-character hiragana words (in Japanese text, almost all single-kana words
    // are particles, auxiliaries, or noise fragments, causing false homophone lookups in flashcard dictionaries)
    if word.chars().count() == 1 && word.chars().all(is_hiragana) {
        return true;
    }

    // 3. Problematic words across Categories 1 to 4
    match word {
        // Category 1: Colloquial Verb Contractions (口語縮約形・補助動詞)
        "てる" | "でる" | "とく" | "どく" | "ちゃう" | "ちまう" | "じゃう" | "じまう"
        | "でく" | "てく" | "とる" | "とれる" | "とける" | "たげる"
        | "てらっしゃる" | "てらしゃる" | "でらっしゃる" | "でらしゃる"
        | "しめる" | "よる" | "はる" | "やがる" | "たがる" => true,

        // Category 2: Colloquial Slang & Speech Particles (口語短縮・音変化)
        "もん" | "っつう" | "つう" | "っちゅう" | "ちゅう" | "ちゃ" | "じゃ" | "ん"
        | "っす" | "でちゅ" | "みてえ" | "みてぇ" | "みたく"
        | "そりゃ" | "こりゃ" | "ありゃ" | "トコ" | "とこ" | "っけ" | "っと" => true,

        // Category 3: Archaic & Classical Grammar Auxiliaries (文語助動詞・古典語尾)
        "まじ" | "む" | "べし" | "まい" | "ぬ" | "ず" | "り" | "き" | "る" | "ど" => true,

        // Category 4: Bound Suffixes/Prefixes & Kinship/Phrase Fragments (接頭辞・接尾辞・語の分断)
        "っぽい" | "ぽい" | "やすい" | "にくい" | "づらい" | "がたい" | "くさい"
        | "がましい" | "ったらしい" | "たらしい" | "げ" | "だらけ" | "っぱなし"
        | "がち" | "まとい" | "ごかし" | "ぶり" | "め" | "さ" | "み" | "がる" | "ぶる"
        | "ら" | "たち" | "ども" | "ちゃん" | "さん" | "さま" | "お" | "ご"
        | "じい" | "とう" | "おじ" | "おば" | "ま" | "い" => true,

        _ => false,
    }
}

