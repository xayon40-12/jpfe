use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use anyhow::{Context, Result};
use lindera::dictionary::{load_fs_dictionary, Dictionary};
use lindera_dictionary::dictionary::{
    character_definition::CharacterDefinition, connection_cost_matrix::ConnectionCostMatrix, metadata::Metadata,
    prefix_dictionary::PrefixDictionary, unknown_dictionary::UnknownDictionary,
};
use std::sync::Arc;
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

/// Basic verbs/adjectives normalized to their kana spelling when written in kana: (lemma, word, reading).
const KANA_LEMMAS: [(&str, &str, &str); 6] = [
    ("為る", "する", "スル"),
    ("居る", "いる", "イル"),
    ("成る", "なる", "ナル"),
    ("有る", "ある", "アル"),
    ("無い", "ない", "ナイ"),
    ("良い", "いい", "イイ"),
];

fn is_set(field: &str) -> bool {
    !field.is_empty() && field != "*"
}

#[cfg(feature = "embed-dict")]
fn embedded_dictionary() -> Result<Dictionary> {
    lindera::dictionary::load_embedded_dictionary(lindera::dictionary::DictionaryKind::UniDic)
        .context("Failed to load embedded UniDic dictionary")
}

#[cfg(not(feature = "embed-dict"))]
fn embedded_dictionary() -> Result<Dictionary> {
    anyhow::bail!("This build has no embedded dictionary; pass a dictionary directory")
}

/// Files of a compiled lindera dictionary, as found in a dictionary directory.
pub const DICTIONARY_FILES: [&str; 9] = [
    "metadata.json", "char_def.bin", "matrix.mtx", "dict.trie", "dict.valsidx",
    "dict.vals", "dict.wordsidx", "dict.words", "unk.bin",
];

/// Build a dictionary from the contents of [`DICTIONARY_FILES`] (for the web viewer, which downloads them).
pub fn dictionary_from_files(mut file: impl FnMut(&str) -> Option<Vec<u8>>) -> Result<Dictionary> {
    let mut get = |name: &str| file(name).with_context(|| format!("Missing dictionary file {name}"));
    let metadata = Metadata::load(&get("metadata.json")?)?;
    metadata.validate_format_version()?;
    let prefix_dictionary = PrefixDictionary::load(
        get("dict.trie")?, get("dict.valsidx")?, get("dict.vals")?, get("dict.wordsidx")?, get("dict.words")?,
    )?;
    Ok(Dictionary {
        prefix_dictionary: Arc::new(prefix_dictionary),
        connection_cost_matrix: Arc::new(ConnectionCostMatrix::load(get("matrix.mtx")?)?),
        character_definition: Arc::new(CharacterDefinition::load(&get("char_def.bin")?)?),
        unknown_dictionary: Arc::new(UnknownDictionary::load(&get("unk.bin")?)?),
        metadata: Arc::new(metadata),
    })
}

impl JapaneseTokenizer {
    /// Tokenizer using the dictionary at `dict_path`, or the embedded UniDic.
    pub fn new(dict_path: Option<&Path>, reading_in_hiragana: bool, include_particles: bool) -> Result<Self> {
        let dictionary = match dict_path {
            Some(p) => load_fs_dictionary(p).with_context(|| format!("Failed to load dictionary from: {:?}", p))?,
            None => embedded_dictionary()?,
        };
        Ok(Self::with_dictionary(dictionary, reading_in_hiragana, include_particles))
    }

    pub fn with_dictionary(dictionary: Dictionary, reading_in_hiragana: bool, include_particles: bool) -> Self {
        Self {
            segmenter: Segmenter::new(Mode::Normal, dictionary, None),
            reading_cache: HashMap::new(),
            reading_in_hiragana,
            include_particles,
        }
    }

    pub fn tokenize(&mut self, text: &str) -> Result<Vec<ParsedWord>> {
        let Self { segmenter, reading_cache, reading_in_hiragana, include_particles } = self;
        let mut words = Vec::new();
        for mut token in segmenter.segment(Cow::Borrowed(text))? {
            let surface = token.surface.to_string();
            let details = token.details();
            let field = |i: usize| details.get(i).copied().unwrap_or("*");
            let pos = field(0);
            match pos {
                "記号" | "補助記号" | "空白" | "接尾辞" | "接頭辞" => continue,
                "助詞" | "助動詞" if !*include_particles => continue,
                _ => {}
            }
            let raw_reading = field(6);
            // Strip etymology annotations from UniDic lemma (e.g. "トラック-truck" -> "トラック")
            let lemma = field(7).split('-').next().unwrap_or_default();
            let orth_base = field(10);

            let (base_word, reading_seed) = if !is_set(lemma) {
                (surface.clone(), raw_reading.to_string())
            } else if pos == "動詞" || pos == "形容詞" {
                let kana = KANA_LEMMAS.iter().find(|(l, ..)| *l == lemma);
                match kana {
                    Some(&(_, w, r)) if surface.chars().all(is_hiragana) => (w.to_string(), r.to_string()),
                    _ => (adapt_kanji(&surface, lemma), raw_reading.to_string()),
                }
            } else {
                // Prefer surface orthography (e.g. 煙草, ご飯, トラック) over the phonetic lemma
                let w = if is_set(orth_base) { orth_base } else { lemma };
                (w.to_string(), raw_reading.to_string())
            };

            if !is_valid_word(&base_word) || is_problematic_word(&base_word) {
                continue;
            }

            let reading = resolve_reading(segmenter, reading_cache, &base_word, &surface, &reading_seed);
            words.push(ParsedWord {
                pronunciation: if *reading_in_hiragana { katakana_to_hiragana(&reading) } else { reading },
                pos: pos.to_string(),
                base_word,
            });
        }
        Ok(words)
    }
}

/// If the surface used a different kanji than the representative lemma (e.g. 治せる vs 直す), keep the surface kanji.
fn adapt_kanji(surface: &str, lemma: &str) -> String {
    let (mut s, mut l) = (surface.chars(), lemma.chars());
    match (s.next(), l.next()) {
        (Some(fs), Some(fl))
            if surface.chars().count() > 1 && lemma.chars().count() > 1
                && fs != fl && is_cjk_kanji(fs) && is_cjk_kanji(fl) =>
        {
            std::iter::once(fs).chain(l).collect()
        }
        _ => lemma.to_string(),
    }
}

fn resolve_reading(
    segmenter: &Segmenter,
    cache: &mut HashMap<String, String>,
    base: &str,
    surface: &str,
    raw_reading: &str,
) -> String {
    cache
        .entry(base.to_string())
        .or_insert_with(|| {
            let katakana = base.chars().all(is_katakana);
            if is_set(raw_reading) && (base == surface || !katakana) {
                raw_reading.to_string()
            } else if katakana {
                base.to_string()
            } else {
                // Segment the base word on its own to find its reading
                segmenter
                    .segment(Cow::Borrowed(base))
                    .ok()
                    .filter(|t| t.len() == 1)
                    .and_then(|mut t| t[0].details().get(6).copied().filter(|r| is_set(r)).map(str::to_string))
                    .unwrap_or_else(|| base.to_string())
            }
        })
        .clone()
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
        .map(|c| match c {
            '\u{30A1}'..='\u{30F6}' => char::from_u32(c as u32 - 0x60).unwrap_or(c),
            _ => c,
        })
        .collect()
}

/// Has a kanji, kana or letter other than the marks ー and 々 (which `char::is_alphabetic` accepts).
pub fn is_valid_word(s: &str) -> bool {
    s.chars().any(|c| c.is_alphabetic() && !matches!(c, 'ー' | '々'))
}

/// Single-hiragana words, colloquial contractions, archaic auxiliaries and bound fragments.
pub fn is_problematic_word(word: &str) -> bool {
    let mut chars = word.chars();
    if matches!((chars.next(), chars.next()), (Some(c), None) if is_hiragana(c)) {
        return true;
    }
    matches!(
        word,
        // Colloquial verb contractions (口語縮約形・補助動詞)
        "てる" | "でる" | "とく" | "どく" | "ちゃう" | "ちまう" | "じゃう" | "じまう"
        | "でく" | "てく" | "とる" | "とれる" | "とける" | "たげる"
        | "てらっしゃる" | "てらしゃる" | "でらっしゃる" | "でらしゃる"
        | "しめる" | "よる" | "はる" | "やがる" | "たがる"
        // Colloquial slang and speech particles (口語短縮・音変化)
        | "もん" | "っつう" | "つう" | "っちゅう" | "ちゅう" | "ちゃ" | "じゃ"
        | "っす" | "でちゅ" | "みてえ" | "みてぇ" | "みたく"
        | "そりゃ" | "こりゃ" | "ありゃ" | "トコ" | "とこ" | "っけ" | "っと"
        // Archaic auxiliaries (文語助動詞)
        | "まじ" | "べし" | "まい"
        // Bound suffixes/prefixes and fragments (接頭辞・接尾辞・語の分断)
        | "っぽい" | "ぽい" | "やすい" | "にくい" | "づらい" | "がたい" | "くさい"
        | "がましい" | "ったらしい" | "たらしい" | "だらけ" | "っぱなし"
        | "がち" | "まとい" | "ごかし" | "ぶり" | "がる" | "ぶる"
        | "たち" | "ども" | "ちゃん" | "さん" | "さま" | "じい" | "とう" | "おじ" | "おば"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_alone_are_not_words() {
        assert!(!is_valid_word("ー"));
        assert!(!is_valid_word("々"));
        assert!(!is_valid_word("123"));
        assert!(is_valid_word("スーパー"));
        assert!(is_valid_word("人々"));
        assert!(is_valid_word("abc"));
    }
}
