use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;
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
    headwords: Option<Headwords>,
    reading_cache: HashMap<String, String>,
    reading_in_hiragana: bool,
    include_particles: bool,
}

#[derive(Debug, Clone)]
pub struct ParsedWord {
    pub base_word: String,
    pub pronunciation: String,
    pub pos: String,
    /// Where the word is written in the tokenized text (bytes): its surface, which may be conjugated (言っ)
    pub span: Range<usize>,
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

/// UniDic verbs that are not dictionary headwords, mapped to the word a learner looks up: (lemma, word, reading).
/// UniDic lists some causative/passive forms as verbs of their own (研ぎ澄ませる), and makes up positive
/// verbs for expressions only used in the negative (計り知れない is split as 計り知れる + ない).
const DERIVED_LEMMAS: [(&str, &str, &str); 9] = [
    ("研ぎ澄ませる", "研ぎ澄ます", "トギスマス"),
    ("張り巡らせる", "張り巡らす", "ハリメグラス"),
    ("習わせる", "習う", "ナラウ"),
    ("紛らわせる", "紛らわす", "マギラワス"),
    ("魅入られる", "魅入る", "ミイル"),
    ("窺い知れる", "窺い知る", "ウカガイシル"),
    ("計り知れる", "計り知れない", "ハカリシレナイ"),
    ("言い知れる", "言い知れぬ", "イイシレヌ"),
    ("居た堪れる", "居た堪れない", "イタタマレナイ"),
];

/// Longest run of tokens tried as one JMdict headword.
const MAX_RUN: usize = 6;

/// A token: its surface and its UniDic fields, joined by [`FIELD_SEPARATOR`] (one allocation instead of 17).
struct Token {
    surface: String,
    fields: String,
    /// Bytes of the surface in the text
    span: Range<usize>,
}

const FIELD_SEPARATOR: char = '\u{1f}';

impl Token {
    fn field(&self, i: usize) -> &str {
        self.fields.split(FIELD_SEPARATOR).nth(i).unwrap_or("*")
    }
}

/// Words from JMdict, as extracted by `scripts/fetch-jmdict.sh`: the kanji headwords that tokens are merged
/// into (神出鬼没), and kanji spellings JMdict marks as rarely used (遣る), with the kana spelling to show instead.
///
/// The file is kept as it is (sorted by word) and searched by binary search: a map of its 230,000 words
/// would take tens of MB, which matters in a browser on a phone.
pub struct Headwords {
    text: Cow<'static, str>,
    /// Start and end of each word (the first field of each line after the version line)
    words: Vec<(u32, u32)>,
    pub version: String,
}

/// One JMdict word: its reading (hiragana), part of speech and whether it is a rarely used spelling.
struct Headword<'a> {
    reading: &'a str,
    pos: &'static str,
    rare: bool,
}

impl Headwords {
    pub fn parse(text: impl Into<Cow<'static, str>>) -> Result<Self> {
        let text = text.into();
        let first = text.lines().next().unwrap_or_default();
        let version = first.strip_prefix("# JMdict ").context("Not a JMdict headword file")?.to_string();
        let mut words = Vec::new();
        let mut start = first.len() + 1;
        for line in text[start.min(text.len())..].split_terminator('\n') {
            let end = start + line.find('\t').unwrap_or(line.len());
            words.push((u32::try_from(start).context("JMdict headword file too large")?, end as u32));
            start += line.len() + 1;
        }
        Ok(Self { text, words, version })
    }

    /// The headwords extracted when building (needs the `embed-dict` feature).
    #[cfg(feature = "embed-dict")]
    pub fn embedded() -> Self {
        Self::parse(include_str!(env!("JPFE_JMDICT_HEADWORDS"))).expect("the embedded JMdict headwords are valid")
    }

    fn word(&self, i: usize) -> &str {
        let (start, end) = self.words[i];
        &self.text[start as usize..end as usize]
    }

    /// Index of the first word that is not before `word`.
    fn lower_bound(&self, word: &str) -> usize {
        let (mut low, mut high) = (0, self.words.len());
        while low < high {
            let middle = (low + high) / 2;
            if self.word(middle) < word {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        low
    }

    /// Whether some word starts with `prefix`.
    fn has_prefix(&self, prefix: &str) -> bool {
        let i = self.lower_bound(prefix);
        i < self.words.len() && self.word(i).starts_with(prefix)
    }

    fn get(&self, word: &str) -> Option<Headword<'_>> {
        let i = self.lower_bound(word);
        if i == self.words.len() || self.word(i) != word {
            return None;
        }
        let rest = &self.text[self.words[i].1 as usize..];
        let mut fields = rest[..rest.find('\n').unwrap_or(rest.len())].split('\t').skip(1);
        let (reading, pos, kind) = (fields.next()?, fields.next()?, fields.next()?);
        let pos = match pos {
            "動詞" => "動詞",
            "形容詞" => "形容詞",
            "副詞" => "副詞",
            "感動詞" => "感動詞",
            "表現" => "表現",
            _ => "名詞",
        };
        Some(Headword { reading, pos, rare: kind == "r" })
    }

    fn headword(&self, word: &str) -> Option<Headword<'_>> {
        self.get(word).filter(|h| !h.rare)
    }

    /// The kana spelling of a rarely used kanji spelling (遣る → やる).
    fn kana_spelling(&self, word: &str) -> Option<&str> {
        self.get(word).filter(|h| h.rare).map(|h| h.reading)
    }
}

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

/// The JMdict headwords file, downloaded by the web viewer with [`DICTIONARY_FILES`].
pub const HEADWORDS_FILE: &str = "jmdict.tsv";

/// Everything [`tokenizer_from_files`] needs.
pub const TOKENIZER_FILES: [&str; 10] = [
    "metadata.json", "char_def.bin", "matrix.mtx", "dict.trie", "dict.valsidx",
    "dict.vals", "dict.wordsidx", "dict.words", "unk.bin", HEADWORDS_FILE,
];

/// A tokenizer from the contents of [`TOKENIZER_FILES`] (for the web viewer, which downloads them).
pub fn tokenizer_from_files(
    mut file: impl FnMut(&str) -> Option<Vec<u8>>,
    reading_in_hiragana: bool,
    include_particles: bool,
) -> Result<JapaneseTokenizer> {
    let headwords = file(HEADWORDS_FILE).with_context(|| format!("Missing dictionary file {HEADWORDS_FILE}"))?;
    let headwords = Headwords::parse(String::from_utf8(headwords)?)?;
    let dictionary = dictionary_from_files(file)?;
    Ok(JapaneseTokenizer::with_dictionary(dictionary, Some(headwords), reading_in_hiragana, include_particles))
}

/// Build a dictionary from the contents of [`DICTIONARY_FILES`].
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
    /// Tokenizer using the dictionary at `dict_path`, or the embedded UniDic, with the embedded JMdict headwords.
    pub fn new(dict_path: Option<&Path>, reading_in_hiragana: bool, include_particles: bool) -> Result<Self> {
        let dictionary = match dict_path {
            Some(p) => load_fs_dictionary(p).with_context(|| format!("Failed to load dictionary from: {:?}", p))?,
            None => embedded_dictionary()?,
        };
        #[cfg(feature = "embed-dict")]
        let headwords = Some(Headwords::embedded());
        #[cfg(not(feature = "embed-dict"))]
        let headwords = None;
        Ok(Self::with_dictionary(dictionary, headwords, reading_in_hiragana, include_particles))
    }

    /// Without `headwords`, compounds stay split as UniDic splits them (神出 + 鬼没).
    pub fn with_dictionary(dictionary: Dictionary, headwords: Option<Headwords>, reading_in_hiragana: bool, include_particles: bool) -> Self {
        Self {
            segmenter: Segmenter::new(Mode::Normal, dictionary, None),
            headwords,
            reading_cache: HashMap::new(),
            reading_in_hiragana,
            include_particles,
        }
    }

    /// Whether particles (助詞) and auxiliary verbs (助動詞) are kept.
    pub fn set_include_particles(&mut self, include: bool) {
        self.include_particles = include;
    }

    pub fn tokenize(&mut self, text: &str) -> Result<Vec<ParsedWord>> {
        let Self { segmenter, headwords, reading_cache, reading_in_hiragana, include_particles } = self;
        let mut words = Vec::new();
        let separator = FIELD_SEPARATOR.to_string();
        let tokens: Vec<Token> = segmenter
            .segment(Cow::Borrowed(text))?
            .into_iter()
            .map(|mut t| Token {
                surface: t.surface.to_string(),
                span: t.byte_start..t.byte_end,
                fields: t.details().join(&separator),
            })
            .collect();
        let mut next = 0;
        for (index, token) in tokens.iter().enumerate() {
            if index < next {
                continue;
            }
            if let Some((end, word, headword)) = headwords.as_ref().and_then(|h| longest_headword(h, &tokens, index)) {
                next = end;
                words.push(ParsedWord {
                    pronunciation: if *reading_in_hiragana { headword.reading.to_string() } else { hiragana_to_katakana(headword.reading) },
                    pos: headword.pos.to_string(),
                    base_word: word,
                    span: token.span.start..tokens[end - 1].span.end,
                });
                continue;
            }
            let surface = token.surface.clone();
            let field = |i: usize| token.field(i);
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
                let derived = DERIVED_LEMMAS.iter().find(|(l, ..)| *l == lemma);
                match (kana, derived) {
                    (Some(&(_, w, r)), _) if surface.chars().all(is_hiragana) => (w.to_string(), r.to_string()),
                    (_, Some(&(_, w, r))) => (adapt_kanji(&surface, w), r.to_string()),
                    _ => {
                        let adapted = adapt_kanji(&surface, lemma);
                        // Same kanji with other okurigana (抑さえる for 抑える): the surface's own base form is right,
                        // unless it is pronounced differently (a potential form such as 帰れる)
                        if adapted != lemma && is_set(orth_base) && kanji_of(orth_base) == kanji_of(&adapted)
                            && same_pronunciation(field(11), raw_reading)
                        {
                            (orth_base.to_string(), raw_reading.to_string())
                        } else {
                            (adapted, raw_reading.to_string())
                        }
                    }
                }
            } else {
                // Prefer surface orthography (e.g. 煙草, ご飯, トラック) over the phonetic lemma
                let w = if is_set(orth_base) { orth_base } else { lemma };
                (w.to_string(), raw_reading.to_string())
            };

            // Written in kana, with a kanji spelling JMdict marks as rarely used (やっ: 遣る → やる)
            let written_in_kana = !surface.chars().any(is_cjk_kanji) && base_word.chars().any(is_cjk_kanji);
            let kana = headwords.as_ref().filter(|_| written_in_kana).and_then(|h| h.kana_spelling(&base_word));
            let (base_word, reading_seed) = match kana {
                Some(kana) => (kana.to_string(), hiragana_to_katakana(kana)),
                None => (base_word, reading_seed),
            };

            if !is_valid_word(&base_word) || is_problematic_word(&base_word) {
                continue;
            }

            let reading = resolve_reading(segmenter, reading_cache, &base_word, &surface, &reading_seed);
            words.push(ParsedWord {
                pronunciation: if *reading_in_hiragana { katakana_to_hiragana(&reading) } else { reading },
                pos: pos.to_string(),
                base_word,
                span: token.span.clone(),
            });
        }
        Ok(words)
    }
}

/// The longest run of tokens from `start` that is a JMdict headword, as written or with its last word in its
/// base form (気に入っ → 気に入る): (end, headword, its JMdict entry).
fn longest_headword<'h>(headwords: &'h Headwords, tokens: &[Token], start: usize) -> Option<(usize, String, Headword<'h>)> {
    let field = |i: usize, f: usize| tokens[i].field(f);
    // Only word building (神出+鬼没, 冒険+者, お+嬢様), not phrases (頭を下げる, その時) or inflections (知らない)
    if matches!(field(start, 0), "助詞" | "助動詞" | "補助記号" | "記号" | "空白" | "連体詞" | "副詞" | "代名詞" | "接続詞" | "感動詞" | "接尾辞")
        || field(start, 1) == "助動詞語幹"
        || start > 0 && field(start - 1, 1) == "数詞" && field(start, 2).starts_with("助数詞")
        || field(start, 7) == "為る"
        // A verb or adjective before a noun is a phrase (幼い頃, 知る限り)
        || matches!(field(start, 0), "動詞" | "形容詞") && (field(start, 5).starts_with("連体形") || field(start, 5).starts_with("終止形"))
    {
        return None;
    }
    let last = tokens.len().min(start + MAX_RUN);
    let mut end = start + 1;
    while end < last && !matches!(field(end, 0), "補助記号" | "記号" | "空白" | "助詞") && field(end, 7) != "為る" {
        end += 1;
    }
    // Every candidate starts with the first token and the first character of the next one: most starts end here
    let next = tokens.get(start + 1).and_then(|t| t.surface.chars().next())?;
    if end < start + 2 || !headwords.has_prefix(&format!("{}{next}", tokens[start].surface)) {
        return None;
    }
    for end in (start + 2..=end).rev() {
        let tail = end - 1;
        if field(tail, 0) == "助動詞" || field(tail, 1) == "助動詞語幹" || field(tail, 1) == "数詞"
            // Productive endings: plurals, auxiliary verbs and adjectives (歩き始める, 大きくなる, 言いにくい)
            || matches!(field(tail, 7), "達" | "等" | "共" | "始める" | "続ける" | "過ぎる" | "終わる" | "出す" | "掛ける" | "成る" | "出来る" | "難い" | "易い" | "辛い")
            // Numbers with counters (三人, 一年生)
            || (start..tail).any(|i| {
                field(i, 1) == "数詞" && (field(i + 1, 0) == "接尾辞" || field(i + 1, 2).starts_with("助数詞"))
            })
        {
            continue;
        }
        let head: String = tokens[start..tail].iter().map(|t| t.surface.as_str()).collect();
        let (surface, base) = (tokens[tail].surface.as_str(), field(tail, 10));
        let conjugated = matches!(field(tail, 0), "動詞" | "形容詞") && is_set(base) && surface != base;
        // A conjugated ending is looked up in its base form (気に入っ → 気に入る), or as written when it is the
        // 連用形 that also makes nouns (拍子抜け, 魔法使い)
        let written = (!conjugated || field(tail, 5).starts_with("連用形")).then(|| format!("{head}{surface}"));
        let base = conjugated.then(|| format!("{head}{base}"));
        if !head.chars().chain(surface.chars()).any(is_cjk_kanji) {
            continue;
        }
        // Noun + ない when the noun is a word of its own (関係ない), unlike 不甲斐ない
        if field(tail, 7) == "無い" && headwords.headword(&head).is_some() {
            continue;
        }
        for candidate in base.into_iter().chain(written) {
            if let Some(headword) = headwords.headword(&candidate) {
                return Some((end, candidate, headword));
            }
        }
    }
    None
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

/// `pron` (with ー for long vowels) is the pronunciation of `reading`.
fn same_pronunciation(pron: &str, reading: &str) -> bool {
    pron.chars().count() == reading.chars().count() && pron.chars().zip(reading.chars()).all(|(p, r)| p == r || p == 'ー')
}

fn kanji_of(s: &str) -> Vec<char> {
    s.chars().filter(|&c| is_cjk_kanji(c)).collect()
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

/// Written only in katakana (mostly loanwords such as パソコン); ー and ・ alone do not count.
pub fn is_katakana_word(word: &str) -> bool {
    word.chars().any(|c| c != 'ー' && c != '・') && word.chars().all(|c| is_katakana(c) || c == '・')
}

pub fn hiragana_to_katakana(s: &str) -> String {
    s.chars().map(|c| if is_hiragana(c) { char::from_u32(c as u32 + 0x60).unwrap_or(c) } else { c }).collect()
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
    fn recognizes_katakana_words() {
        assert!(is_katakana_word("パソコン"));
        assert!(is_katakana_word("スーパー"));
        assert!(is_katakana_word("ツインデス・スコルピオ"));
        assert!(!is_katakana_word("ー"));
        assert!(!is_katakana_word("お嬢様"));
        assert!(!is_katakana_word("ドン引き"));
        assert!(!is_katakana_word("ひらがな"));
    }

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

#[cfg(all(test, feature = "embed-dict"))]
mod dictionary_tests {
    use super::JapaneseTokenizer;

    fn words(text: &str) -> Vec<String> {
        let mut tokenizer = JapaneseTokenizer::new(None, true, false).unwrap();
        tokenizer.tokenize(text).unwrap().into_iter().map(|w| w.base_word).collect()
    }

    #[test]
    fn merges_tokens_into_jmdict_words() {
        assert_eq!(words("神出鬼没の怪盗"), ["神出鬼没", "怪盗"]);
        assert_eq!(words("魔法使いになった"), ["魔法使い", "なる"]);
        assert_eq!(words("魔法使える"), ["魔法", "使う"]);
        assert_eq!(words("至近距離で"), ["至近距離"]);
        assert_eq!(words("冒険者の拍子抜け"), ["冒険者", "拍子抜け"]);
        assert_eq!(words("気に入った"), ["気", "入る"]);
    }

    #[test]
    fn keeps_phrases_and_inflections_apart() {
        assert_eq!(words("頭を下げる"), ["頭", "下げる"]);
        assert_eq!(words("知らない"), ["知る"]);
        assert_eq!(words("歩き始める"), ["歩く", "始める"]);
        assert_eq!(words("関係ない"), ["関係", "ない"]);
    }

    #[test]
    fn writes_rare_kanji_spellings_in_kana() {
        assert_eq!(words("やってしまう"), ["やる", "しまう"]);
        assert_eq!(words("遣る"), ["遣る"]);
    }

    #[test]
    fn maps_derived_verbs_to_headwords() {
        assert_eq!(words("研ぎ澄ませた"), ["研ぎ澄ます"]);
        assert_eq!(words("計り知れない"), ["計り知れない"]);
    }

    #[test]
    fn finds_where_words_are_written() {
        let text = "冒険者は神出鬼没の怪盗を見た";
        let mut tokenizer = JapaneseTokenizer::new(None, true, false).unwrap();
        let spans: Vec<(String, &str)> =
            tokenizer.tokenize(text).unwrap().into_iter().map(|w| (w.base_word, &text[w.span])).collect();
        let expected = [("冒険者", "冒険者"), ("神出鬼没", "神出鬼没"), ("怪盗", "怪盗"), ("見る", "見")];
        assert_eq!(spans, expected.map(|(w, s)| (w.to_string(), s)));
    }

    #[test]
    fn keeps_okurigana_of_the_surface() {
        assert_eq!(words("抑えた"), ["抑える"]);
        assert_eq!(words("治せる"), ["治す"]);
    }
}
