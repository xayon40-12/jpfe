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
}

/// What a word is, for word lists to choose which words to list: the tokenizer reports every word it finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordKind {
    /// A word to learn
    Word,
    /// Particles and auxiliary verbs (助詞・助動詞), which include colloquial contractions (てる, ちゃう, とく);
    /// listed only when asked
    Grammar,
    /// A single hiragana other than a conjunction: interjections (あ), prefixes (お) and suffixes (さ, げ), never listed
    Fragment,
}

#[derive(Debug, Clone)]
pub struct ParsedWord {
    /// The word's dictionary form, which identifies it in counts and across books
    pub base_word: String,
    /// The dictionary form as the text writes it: the same as `base_word`, unless the text writes in kana a
    /// word whose dictionary form has kanji (いる for 居る, こと for 事); word lists show each word in the
    /// spelling its book uses most
    pub spelling: String,
    pub pronunciation: String,
    pub pos: String,
    /// Where the word is written in the tokenized text (bytes): its surface, which may be conjugated (言っ)
    pub span: Range<usize>,
    pub kind: WordKind,
}

impl ParsedWord {
    /// Whether word lists count this word; particles and auxiliary verbs only with `include_particles`.
    pub fn is_listed(&self, include_particles: bool) -> bool {
        match self.kind {
            WordKind::Word => true,
            WordKind::Grammar => include_particles,
            WordKind::Fragment => false,
        }
    }
}

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
    /// A word of an entry written only in kana (いい)
    kana: bool,
    /// Every reading of the word in any entry (・-separated), when it has others than `reading`
    readings: &'a str,
}

/// How a word written in kana relates to a dictionary form in kanji UniDic gives it.
#[derive(Debug, PartialEq)]
enum KanaForm {
    /// A JMdict reading of the dictionary form: the same word (できる for 出来る, こと for 事)
    Spelling,
    /// Not one of its readings, but this JMdict word of its own, in kana (いい, whose entry is not 良い's;
    /// also for イイ)
    OwnWord(String),
    /// Neither: another form of the word, such as a potential form (かえれる for 帰る)
    Other,
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
        let readings = fields.next().unwrap_or_default();
        let pos = match pos {
            "動詞" => "動詞",
            "形容詞" => "形容詞",
            "副詞" => "副詞",
            "感動詞" => "感動詞",
            "表現" => "表現",
            _ => "名詞",
        };
        Some(Headword { reading, pos, rare: kind == "r", kana: kind == "a", readings })
    }

    /// A kanji headword (not rarely used), which tokens may be merged into.
    fn headword(&self, word: &str) -> Option<Headword<'_>> {
        self.get(word).filter(|h| !h.rare && !h.kana)
    }

    /// How `kana`, the way the text writes a word, relates to its dictionary form `word` (see [`KanaForm`]).
    /// A word of its own must be the same part of speech (`pos`, as UniDic gives it) for verbs and adjectives.
    fn kana_form(&self, word: &str, kana: &str, pos: &str) -> KanaForm {
        let hiragana = katakana_to_hiragana(kana);
        let reads = |r: &str| r == kana || r == hiragana;
        if self.get(word).is_some_and(|h| reads(h.reading) || h.readings.split('・').any(reads)) {
            return KanaForm::Spelling;
        }
        let same_kind = |h: &Headword| !matches!(pos, "動詞" | "形容詞") || h.pos == pos;
        let own = [hiragana.as_str(), kana].into_iter().find(|k| self.get(k).is_some_and(|h| h.kana && same_kind(&h)));
        own.map_or(KanaForm::Other, |k| KanaForm::OwnWord(k.to_string()))
    }

    /// The kana spelling of a rarely used kanji spelling (遣る → やる).
    fn kana_spelling(&self, word: &str) -> Option<&str> {
        self.get(word).filter(|h| h.rare).map(|h| h.reading)
    }

    /// For a verb JMdict does not have, the verb it is a form of, if JMdict has that one:
    /// - a causative or passive form (思わす, 付き合わせる, 揺られる → 思う, 付き合う, 揺る)
    /// - the godan verb of an -eru verb: its transitive pair, short causative or potential form
    ///   (読み進める → 読み進む, 研ぎ澄ませる → 研ぎ澄ます, 窺い知れる → 窺い知る)
    fn verb_base(&self, verb: &str) -> Option<(String, Headword<'_>)> {
        if !verb.chars().any(is_cjk_kanji) || self.get(verb).is_some() {
            return None;
        }
        let found = |word: String| self.get(&word).filter(|h| h.pos == "動詞").map(|h| (word, h));
        // Causative and passive forms: of an ichidan verb (食べさせる, 食べられる), then of a godan verb, whose
        // stem ends in the a-row (座らせる, 座らす, 座られる)
        let ichidan = ["させる", "られる"].iter().find_map(|end| verb.strip_suffix(end)).and_then(|stem| found(format!("{stem}る")));
        let godan = || {
            let stem = ["せる", "れる", "す"].iter().find_map(|end| verb.strip_suffix(end))?;
            let (rest, last) = split_last(stem)?;
            found(format!("{rest}{}", row_shift(last, A_ROW, U_ROW)?))
        };
        // The godan verb of an -eru verb
        let counterpart = || {
            let (rest, last) = split_last(verb.strip_suffix('る')?)?;
            found(format!("{rest}{}", row_shift(last, E_ROW, U_ROW)?))
        };
        ichidan.or_else(godan).or_else(counterpart)
    }

    /// A compound verb JMdict does not have, as the two verbs it is made of (握り直す → 握る + 直す): their
    /// dictionary forms, and the first one as written in `verb` (握り).
    fn split_compound<'v>(&self, verb: &'v str) -> Option<([(String, Headword<'_>); 2], &'v str)> {
        if self.get(verb).is_some() {
            return None;
        }
        let found = |word: String| self.get(&word).filter(|h| h.pos == "動詞").map(|h| (word, h));
        verb.char_indices().skip(1).find_map(|(i, c)| {
            let (first, second) = verb.split_at(i);
            if !is_cjk_kanji(c) || second.chars().count() < 2 {
                return None;
            }
            let second = found(second.to_string())?;
            // The first verb is in its 連用形: godan (握り → 握る) or ichidan (消え → 消える)
            let (rest, last) = split_last(first)?;
            let godan = row_shift(last, I_ROW, U_ROW).and_then(|u| found(format!("{rest}{u}")));
            let base = godan.or_else(|| found(format!("{first}る")))?;
            Some(([base, second], first))
        })
    }
}

/// Kana rows that conjugation moves between, in the same column order.
const A_ROW: &str = "あかがさたなばまらわ";
const I_ROW: &str = "いきぎしちにびみり";
const U_ROW: &str = "うくぐすつぬぶむるう";
const E_ROW: &str = "えけげせてねべめれ";

/// The kana of `to` in the column of `c` in `from` (か in the a-row → く in the u-row).
fn row_shift(c: char, from: &str, to: &str) -> Option<char> {
    let column = from.chars().position(|f| f == c)?;
    to.chars().nth(column)
}

/// The text before the last character, and that character.
fn split_last(text: &str) -> Option<(&str, char)> {
    let last = text.chars().last()?;
    Some((&text[..text.len() - last.len_utf8()], last))
}

/// The word a dictionary has for a verb or adjective JMdict does not have.
enum DictionaryWord<'h> {
    /// One word and its reading (hiragana); `with_next` when it takes in the next token (計り知れ + ない)
    Word { word: String, reading: Cow<'h, str>, with_next: bool },
    /// Two verbs, the second written from `boundary` (bytes) in the token's surface
    Compound { verbs: [(String, Headword<'h>); 2], boundary: usize },
}

/// For a verb or adjective whose base form `word` JMdict does not have, the word to list instead (any spelling
/// JMdict has counts, even a rarely used one, since it is the one in the text), trying:
/// 1. the base form of the token as written, or UniDic's lemma: UniDic's lemma may have other kanji
///    (旨い for 上手く, 悔しい for 口惜しい)
/// 2. an expression only used in the negative, when the next token is the negative: UniDic makes up a
///    positive verb for it (計り知れない, 言い知れぬ and 思い掛けない as 計り知れる + ない…)
/// 3. for a word written in kana, its base form in kana: UniDic's lemma may be a spelling JMdict does not
///    have (於く, 蹌踉く for おく, よろめく)
/// 4. the verb it is a form of: see [`Headwords::verb_base`]
/// 5. the two verbs of a compound verb: see [`Headwords::split_compound`]
fn dictionary_word<'h>(headwords: &'h Headwords, word: &str, token: &Token, next: Option<&Token>) -> Option<DictionaryWord<'h>> {
    let is_verb = token.field(0) == "動詞";
    let lemma = token.field(7).split('-').next().unwrap_or_default();
    for written in [token.field(10), lemma] {
        if !is_set(written) || written == word {
            continue;
        }
        if let Some(headword) = headwords.get(written) {
            return Some(DictionaryWord::Word { word: written.to_string(), reading: headword.reading.into(), with_next: false });
        }
    }
    let endings: &[&str] = match next.map(|t| t.field(7)) {
        Some("無い" | "ない") => &["ない"],
        Some("ず") => &["ず", "ぬ"],
        _ => &[],
    };
    // As written first (思いがけ + ない), then as UniDic spells it (思い掛け + ない)
    let stems = [token.field(10), word].map(|w| w.strip_suffix('る'));
    for (stem, ending) in stems.into_iter().flatten().flat_map(|stem| endings.iter().map(move |e| (stem, e))) {
        let expression = format!("{stem}{ending}");
        if let Some(headword) = headwords.get(&expression) {
            return Some(DictionaryWord::Word { word: expression, reading: headword.reading.into(), with_next: true });
        }
    }
    // Written in kana: its base form in kana (JMdict's kana spellings are not in the headword file)
    let written = token.field(10);
    if is_set(written) && !token.surface.chars().any(is_cjk_kanji) && !written.chars().any(is_cjk_kanji) {
        return Some(DictionaryWord::Word { word: written.to_string(), reading: katakana_to_hiragana(written).into(), with_next: false });
    }
    if !is_verb {
        return None;
    }
    if let Some((verb, headword)) = headwords.verb_base(word) {
        return Some(DictionaryWord::Word { word: verb, reading: headword.reading.into(), with_next: false });
    }
    let (verbs, first) = headwords.split_compound(word)?;
    // Where the second verb starts as written: after the first one (消え|かけ), or at its kanji (握|り直し → 握り|直し)
    let second = verbs[1].0.chars().next()?;
    let after_first = token.surface.char_indices().nth(1).map_or(token.surface.len(), |(i, _)| i);
    let boundary = if token.surface.starts_with(first) && first.len() < token.surface.len() {
        first.len()
    } else {
        after_first + token.surface[after_first..].find(second)?
    };
    Some(DictionaryWord::Compound { verbs, boundary })
}

/// The text writes in kana (`surface`, with base form `orth_base`) a word whose dictionary form `word` has kanji.
fn writes_in_kana(surface: &str, word: &str, orth_base: &str) -> bool {
    let kana = |s: &str| !s.chars().any(is_cjk_kanji);
    kana(surface) && !kana(word) && is_set(orth_base) && kana(orth_base) && is_valid_word(orth_base)
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
pub fn tokenizer_from_files(mut file: impl FnMut(&str) -> Option<Vec<u8>>, reading_in_hiragana: bool) -> Result<JapaneseTokenizer> {
    let headwords = file(HEADWORDS_FILE).with_context(|| format!("Missing dictionary file {HEADWORDS_FILE}"))?;
    let headwords = Headwords::parse(String::from_utf8(headwords)?)?;
    let dictionary = dictionary_from_files(file)?;
    Ok(JapaneseTokenizer::with_dictionary(dictionary, Some(headwords), reading_in_hiragana))
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
    pub fn new(dict_path: Option<&Path>, reading_in_hiragana: bool) -> Result<Self> {
        let dictionary = match dict_path {
            Some(p) => load_fs_dictionary(p).with_context(|| format!("Failed to load dictionary from: {:?}", p))?,
            None => embedded_dictionary()?,
        };
        #[cfg(feature = "embed-dict")]
        let headwords = Some(Headwords::embedded());
        #[cfg(not(feature = "embed-dict"))]
        let headwords = None;
        Ok(Self::with_dictionary(dictionary, headwords, reading_in_hiragana))
    }

    /// Without `headwords`, compounds stay split as UniDic splits them (神出 + 鬼没).
    pub fn with_dictionary(dictionary: Dictionary, headwords: Option<Headwords>, reading_in_hiragana: bool) -> Self {
        Self {
            segmenter: Segmenter::new(Mode::Normal, dictionary, None),
            headwords,
            reading_cache: HashMap::new(),
            reading_in_hiragana,
        }
    }

    /// Every word of `text`, with what kind of word it is (see [`WordKind`]): only symbols, spaces and
    /// tokens that are not words (numerals in digits, runs of punctuation) are left out.
    pub fn tokenize(&mut self, text: &str) -> Result<Vec<ParsedWord>> {
        let Self { segmenter, headwords, reading_cache, reading_in_hiragana } = self;
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
                    spelling: word.clone(),
                    base_word: word,
                    span: token.span.start..tokens[end - 1].span.end,
                    kind: WordKind::Word,
                });
                continue;
            }
            let surface = token.surface.clone();
            let field = |i: usize| token.field(i);
            let pos = field(0);
            match pos {
                "記号" | "補助記号" | "空白" => continue,
                _ => {}
            }
            let raw_reading = field(6);
            // Strip etymology annotations from UniDic lemma (e.g. "トラック-truck" -> "トラック")
            let lemma = field(7).split('-').next().unwrap_or_default();
            let orth_base = field(10);

            let (base_word, reading_seed) = if !is_set(lemma) {
                (surface.clone(), raw_reading.to_string())
            } else if pos == "動詞" || pos == "形容詞" {
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
            } else if headwords.as_ref().is_some_and(|h| {
                writes_in_kana(&surface, lemma, orth_base) && h.kana_form(lemma, orth_base, pos) == KanaForm::Spelling
            }) {
                // Written in kana (こと, タバコ): counted under its dictionary form in kanji (事, 煙草)
                (lemma.to_string(), raw_reading.to_string())
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
            // Written in kana, with a dictionary form in kanji: one of its spellings, a word of its own, or another form
            let form = headwords
                .as_ref()
                .filter(|_| writes_in_kana(&surface, &base_word, orth_base))
                .map(|h| h.kana_form(&base_word, orth_base, pos));
            let (base_word, reading_seed, spelling) = match form {
                Some(KanaForm::Spelling) => (base_word, reading_seed, Some(orth_base.to_string())),
                Some(KanaForm::OwnWord(word)) => {
                    let spelling = (word != orth_base).then(|| orth_base.to_string());
                    let reading = hiragana_to_katakana(&word);
                    (word, reading, spelling)
                }
                Some(KanaForm::Other) | None => (base_word, reading_seed, None),
            };

            // A verb or adjective JMdict does not have: the word (or words) a dictionary has instead
            let mut span = token.span.clone();
            let missing = matches!(pos, "動詞" | "形容詞") && base_word.chars().any(is_cjk_kanji);
            let found = headwords
                .as_ref()
                .filter(|h| missing && h.get(&base_word).is_none())
                .and_then(|h| dictionary_word(h, &base_word, token, tokens.get(index + 1)));
            let (base_word, reading_seed, spelling) = match found {
                Some(DictionaryWord::Word { word, reading, with_next }) => {
                    if with_next {
                        span.end = tokens[index + 1].span.end;
                        next = index + 2;
                    }
                    (word, hiragana_to_katakana(&reading), None)
                }
                Some(DictionaryWord::Compound { verbs, boundary }) => {
                    let spans = [token.span.start..token.span.start + boundary, token.span.start + boundary..token.span.end];
                    for ((word, headword), span) in verbs.into_iter().zip(spans) {
                        words.push(ParsedWord {
                            pronunciation: if *reading_in_hiragana { headword.reading.to_string() } else { hiragana_to_katakana(headword.reading) },
                            pos: headword.pos.to_string(),
                            spelling: word.clone(),
                            base_word: word,
                            span,
                            kind: WordKind::Word,
                        });
                    }
                    continue;
                }
                None => (base_word, reading_seed, spelling),
            };
            let spelling = spelling.unwrap_or_else(|| base_word.clone());

            if !is_valid_word(&spelling) {
                continue;
            }
            let kind = match pos {
                "助詞" | "助動詞" => WordKind::Grammar,
                "接続詞" => WordKind::Word,
                _ if is_single_hiragana(&spelling) => WordKind::Fragment,
                _ => WordKind::Word,
            };

            // The reading of the spelling: a kana spelling is its own reading (いい, not よい for 良い)
            let reading = if spelling != base_word {
                hiragana_to_katakana(&spelling)
            } else {
                resolve_reading(segmenter, reading_cache, &base_word, &surface, &reading_seed)
            };
            words.push(ParsedWord {
                pronunciation: if *reading_in_hiragana { katakana_to_hiragana(&reading) } else { reading },
                pos: pos.to_string(),
                base_word,
                spelling,
                span,
                kind,
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
            if let Some((verb, headword)) = headwords.verb_base(&candidate) {
                return Some((end, verb, headword));
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

/// A single hiragana (あ, お, さ).
fn is_single_hiragana(word: &str) -> bool {
    let mut chars = word.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if is_hiragana(c))
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
    use super::{is_cjk_kanji, JapaneseTokenizer, WordKind};

    fn words(text: &str) -> Vec<String> {
        let mut tokenizer = JapaneseTokenizer::new(None, true).unwrap();
        tokenizer.tokenize(text).unwrap().into_iter().filter(|w| w.is_listed(false)).map(|w| w.spelling).collect()
    }

    #[test]
    fn reports_every_word_and_lists_only_some() {
        let mut tokenizer = JapaneseTokenizer::new(None, true).unwrap();
        let text = "あ、グレイラット家の本を５冊読んでる";
        let found: Vec<(&str, WordKind)> = tokenizer.tokenize(text).unwrap().into_iter().map(|w| (&text[w.span], w.kind)).collect();
        use WordKind::*;
        assert_eq!(
            found,
            [("あ", Fragment), ("グレイ", Word), ("ラット", Word), ("家", Word), ("の", Grammar), ("本", Word), ("を", Grammar), ("冊", Word), ("読ん", Word), ("でる", Grammar)]
        );
        // Ordinary verbs that share their kana with contractions are words (とって is 取る, によって 因る)
        let pairs = |list: &[(&str, &str)]| list.iter().map(|&(k, s)| (k.to_string(), s.to_string())).collect::<Vec<_>>();
        assert_eq!(keys("手にとって"), pairs(&[("手", "手"), ("取る", "とる")]));
        // (JMdict marks 其れ and 因る as rarely used spellings)
        assert_eq!(keys("それによって"), pairs(&[("それ", "それ"), ("よる", "よる")]));
    }

    /// Dictionary form and spelling of each word.
    fn keys(text: &str) -> Vec<(String, String)> {
        let mut tokenizer = JapaneseTokenizer::new(None, true).unwrap();
        tokenizer.tokenize(text).unwrap().into_iter().filter(|w| w.is_listed(false)).map(|w| (w.base_word, w.spelling)).collect()
    }

    #[test]
    fn counts_kana_spellings_under_the_dictionary_form() {
        let pairs = |list: &[(&str, &str)]| list.iter().map(|&(k, s)| (k.to_string(), s.to_string())).collect::<Vec<_>>();
        // JMdict marks 此処 as rarely used, so ここ is its dictionary form
        assert_eq!(keys("ここにいる"), pairs(&[("ここ", "ここ"), ("居る", "いる")]));
        assert_eq!(keys("よいことができた"), pairs(&[("良い", "よい"), ("事", "こと"), ("出来る", "できる")]));
        // いい is a JMdict entry of its own, not a reading of 良い
        assert_eq!(keys("いい"), pairs(&[("いい", "いい")]));
        assert_eq!(keys("イイ"), pairs(&[("いい", "イイ")]));
        assert_eq!(keys("出来る事"), pairs(&[("出来る", "出来る"), ("事", "事")]));
        // A potential form written in kana is not one of the verb's readings: still counted as the verb
        let (verb, spelling) = keys("かえれる").remove(0);
        assert!(verb.chars().any(is_cjk_kanji) && spelling == verb, "{verb} {spelling}");
        let mut tokenizer = JapaneseTokenizer::new(None, true).unwrap();
        assert_eq!(tokenizer.tokenize("いい").unwrap()[0].pronunciation, "いい");
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
        let mut tokenizer = JapaneseTokenizer::new(None, true).unwrap();
        let spans: Vec<(String, &str)> =
            tokenizer.tokenize(text).unwrap().into_iter().filter(|w| w.is_listed(false)).map(|w| (w.base_word, &text[w.span])).collect();
        let expected = [("冒険者", "冒険者"), ("神出鬼没", "神出鬼没"), ("怪盗", "怪盗"), ("見る", "見")];
        assert_eq!(spans, expected.map(|(w, s)| (w.to_string(), s)));
    }

    #[test]
    fn maps_verbs_missing_from_jmdict_to_their_godan_verb() {
        assert_eq!(words("読み進めていくと"), ["読み進む", "いく"]);
        assert_eq!(words("言い切れない"), ["言い切る"]);
        assert_eq!(words("張り巡らせた"), ["張り巡らす"]);
        assert_eq!(words("窺い知れる"), ["窺い知る"]);
    }

    #[test]
    fn uses_the_base_form_as_written_when_the_lemma_is_not_in_jmdict() {
        assert_eq!(words("上手くいった"), ["上手い", "いく"]);
        assert_eq!(words("口惜しい"), ["口惜しい"]);
        assert_eq!(words("よろめいた"), ["よろめく"]);
    }

    #[test]
    fn maps_causative_and_passive_forms_to_their_verb() {
        assert_eq!(words("習わせる"), ["習う"]);
        assert_eq!(words("魅入られた"), ["魅入る"]);
        assert_eq!(words("思わす"), ["思う"]);
    }

    #[test]
    fn keeps_expressions_only_used_in_the_negative_whole() {
        assert_eq!(words("思い掛けない"), ["思い掛けない"]);
        assert_eq!(words("思いがけない"), ["思いがけない"]);
        assert_eq!(words("思い掛けず"), ["思い掛けず"]);
        assert_eq!(words("言い知れぬ"), ["言い知れぬ"]);
        assert_eq!(words("居た堪れない"), ["居た堪れない"]);
        assert_eq!(words("いたたまれない"), ["居た堪れない"]);
    }

    #[test]
    fn splits_compound_verbs_missing_from_jmdict() {
        assert_eq!(words("剣を握り直した"), ["剣", "握る", "直す"]);
        assert_eq!(words("頷き返す"), ["頷く", "返す"]);
        assert_eq!(words("消え掛けた"), ["消える", "掛ける"]);
        assert_eq!(words("消えかけた"), ["消える", "掛ける"]);
        assert_eq!(words("巡り巡って"), ["巡る", "巡る"]);
        let text = "握り直した";
        let mut tokenizer = JapaneseTokenizer::new(None, true).unwrap();
        let spans: Vec<&str> = tokenizer.tokenize(text).unwrap().into_iter().filter(|w| w.is_listed(false)).map(|w| &text[w.span]).collect();
        assert_eq!(spans, ["握り", "直し"]);
    }

    #[test]
    fn keeps_okurigana_of_the_surface() {
        assert_eq!(words("抑えた"), ["抑える"]);
        assert_eq!(words("治せる"), ["治す"]);
    }
}
