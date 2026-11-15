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
    /// A number with no counter (二十三, 2020, 第二, 数十), never listed: numbers are learned as a system, not word by
    /// word (with a counter, the counter is the word: see [`ParsedWord::written_reading`])
    Number,
}

#[derive(Debug, Clone)]
pub struct ParsedWord {
    /// The word's dictionary form, which identifies it in counts and across books (with its reading for a
    /// homograph: see [`Self::key`])
    pub base_word: String,
    /// The dictionary form as the text writes it: the same as `base_word`, unless the text writes in kana a
    /// word whose dictionary form has kanji (いる for 居る, こと for 事); word lists show each word in the
    /// spelling its book uses most
    pub spelling: String,
    pub pronunciation: String,
    pub pos: String,
    /// Where the word is written in the tokenized text (bytes), with its conjugation endings (数えられる)
    pub span: Range<usize>,
    pub kind: WordKind,
    /// What UniDic makes words of their own but belongs to the word (see [`Self::with_affixes`]): the polite
    /// prefix written before it (the お of お二人), and the endings written after it, conjugation endings (られる,
    /// た, て, the ない of 美しくない) and suffixes (静かさ, 彼ら). Part of the word in the text, and grammar in
    /// word lists.
    pub affixes: Vec<ParsedWord>,
    /// What the word's affixes mean, in order ("polite (お)", "passive or potential (られる)", "past (た)")
    pub forms: Vec<&'static str>,
    /// For a word with the same dictionary form as another word but read differently (the 様 of ようだ, not
    /// the 様 of エミリア様), its reading, which keeps the two apart in counts (see [`Self::key`])
    pub homograph: Option<String>,
    /// How the whole span is read, when it is more than the word: a number with its counter, which is the
    /// word (一つ is つ, read ひとつ; 三本 is 本, read さんぼん)
    pub written_reading: Option<String>,
}

impl ParsedWord {
    /// What identifies the word in counts and across books: its dictionary form, with its reading for a
    /// homograph (様【よう】, while 様 is さま).
    pub fn key(&self) -> Cow<'_, str> {
        match &self.homograph {
            Some(reading) => Cow::Owned(format!("{}【{reading}】", self.base_word)),
            None => Cow::Borrowed(&self.base_word),
        }
    }

    /// The word, then its affixes: what word lists count.
    pub fn with_affixes(&self) -> impl Iterator<Item = &ParsedWord> {
        std::iter::once(self).chain(&self.affixes)
    }

    /// Whether word lists count this word; particles and auxiliary verbs only with `include_particles`.
    pub fn is_listed(&self, include_particles: bool) -> bool {
        match self.kind {
            WordKind::Word => true,
            WordKind::Grammar => include_particles,
            WordKind::Fragment | WordKind::Number => false,
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
    /// The readings that are other words written the same (・-separated): よう and ざま for 様, read さま
    homographs: &'a str,
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
        let homographs = fields.next().unwrap_or_default();
        let pos = match pos {
            "動詞" => "動詞",
            "形容詞" => "形容詞",
            "副詞" => "副詞",
            "感動詞" => "感動詞",
            "表現" => "表現",
            _ => "名詞",
        };
        Some(Headword { reading, pos, rare: kind == "r", kana: kind == "a", readings, homographs })
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

    /// For `word`, UniDic's lemma `lemma` read `reading` (katakana), that reading when it is another word
    /// written the same (よう for 様, whose usual reading is さま): a reading of a JMdict entry of `word` that
    /// shares no meaning with the entry of its usual reading (see `scripts/jmdict-headwords.py`).
    fn homograph(&self, word: &str, lemma: &str, reading: &str) -> Option<String> {
        if word != lemma || !word.chars().any(is_cjk_kanji) || !is_set(reading) {
            return None;
        }
        let reading = katakana_to_hiragana(reading);
        let headword = self.get(word)?;
        headword.homographs.split('・').any(|r| r == reading).then_some(reading)
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
    /// tokens that are not words (runs of punctuation) are left out. Prefixes, conjugation endings and suffixes
    /// are part of their word (see [`ParsedWord::affixes`]), and numbers of their counter.
    pub fn tokenize(&mut self, text: &str) -> Result<Vec<ParsedWord>> {
        let Self { segmenter, headwords, reading_cache, reading_in_hiragana } = self;
        let mut words = Vec::new();
        // The tokens each word is made of
        let mut sources: Vec<Range<usize>> = Vec::new();
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
                    affixes: Vec::new(),
                    forms: Vec::new(),
                    homograph: None,
                    written_reading: None,
                });
                sources.push(index..end);
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
            let mut source = index..index + 1;
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
                        source.end = next;
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
                            affixes: Vec::new(),
                            forms: Vec::new(),
                            homograph: None,
                            written_reading: None,
                        });
                        sources.push(index..index + 1);
                    }
                    continue;
                }
                None => (base_word, reading_seed, spelling),
            };
            // 一人 and 二人 (or ひとり, ふたり), which UniDic keeps as one word: counted under their counter like
            // 三人, read as a whole (see `attach_counters`)
            if pos == "名詞" && matches!(lemma, "一人" | "二人") {
                let reading = katakana_to_hiragana(raw_reading);
                words.push(ParsedWord {
                    pronunciation: if *reading_in_hiragana { "にん".to_string() } else { "ニン".to_string() },
                    pos: "接尾辞".to_string(),
                    base_word: "人".to_string(),
                    spelling: "人".to_string(),
                    span,
                    kind: WordKind::Word,
                    affixes: Vec::new(),
                    forms: Vec::new(),
                    homograph: headwords.as_ref().and_then(|h| h.homograph("人", "人", "ニン")),
                    written_reading: Some(if *reading_in_hiragana { reading } else { hiragana_to_katakana(&reading) }),
                });
                sources.push(source);
                continue;
            }
            let spelling = spelling.unwrap_or_else(|| base_word.clone());
            let homograph = headwords.as_ref().and_then(|h| h.homograph(&base_word, lemma, raw_reading));

            // Numbers in digits stay until counters are attached to them (3人), and are left out otherwise
            if !is_valid_word(&spelling) && field(1) != "数詞" {
                continue;
            }
            let kind = match pos {
                "助詞" | "助動詞" => WordKind::Grammar,
                "接続詞" => WordKind::Word,
                _ if is_single_hiragana(&spelling) => WordKind::Fragment,
                _ => WordKind::Word,
            };

            // The reading of the spelling: a kana spelling is its own reading (いい, not よい for 良い)
            // A homograph's reading is the one that tells it apart (readings are cached by dictionary form)
            let reading = if spelling != base_word {
                hiragana_to_katakana(&spelling)
            } else if let Some(reading) = &homograph {
                hiragana_to_katakana(reading)
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
                affixes: Vec::new(),
                forms: Vec::new(),
                homograph,
                written_reading: None,
            });
            sources.push(source);
        }
        let (words, sources) = attach_counters(words, sources, &tokens, *reading_in_hiragana);
        Ok(attach_prefixes(attach_endings(words, sources, &tokens, *reading_in_hiragana)))
    }
}

/// Groups numbers into words. A run of numbers goes into the counter after it (一 + つ → 一つ, 二十 + 三 + 人 →
/// 二十三人, 3 + 人 → 3人), with an ordinal 目 after them (一つ目): the word is the counter, and the whole is read as
/// a number with its counter (see [`ParsedWord::written_reading`]). With no counter, the run is one number, which
/// is not listed (see [`WordKind::Number`]). 第 before the numbers makes them an ordinal (第三章, 第二).
/// `sources[i]` are the tokens `words[i]` is made of.
fn attach_counters(
    words: Vec<ParsedWord>,
    sources: Vec<Range<usize>>,
    tokens: &[Token],
    reading_in_hiragana: bool,
) -> (Vec<ParsedWord>, Vec<Range<usize>>) {
    let single = |source: &Range<usize>| (source.len() == 1).then(|| &tokens[source.start]);
    // A number, but not a single kana UniDic takes for one (the く of くふ, the シ of シッ; ひと of ひとつ is one)
    let is_number = |source: &Range<usize>| {
        single(source).is_some_and(|t| {
            t.field(1) == "数詞" && (t.surface.chars().count() > 1 || t.surface.chars().any(|c| is_cjk_kanji(c) || c.is_numeric()))
        })
    };
    // Words that follow each other in the text, not only in tokens: line breaks have none (a page number of
    // a PDF at the end of a line, and the next line)
    let next_to = |previous: &ParsedWord, previous_source: &Range<usize>, word: &ParsedWord, source: &Range<usize>| {
        source.start == previous_source.end && word.span.start == previous.span.end
    };
    let in_kana = |reading: String| if reading_in_hiragana { reading } else { hiragana_to_katakana(&reading) };
    let is_ordinal = |s: &Range<usize>| single(s).is_some_and(|t| t.field(0) == "接尾辞" && t.field(7) == "目");
    let (mut attached, mut attached_sources) = (Vec::with_capacity(words.len()), Vec::with_capacity(words.len()));
    let mut items = words.into_iter().zip(sources).peekable();
    while let Some((word, source)) = items.next() {
        // 第 before a number makes it an ordinal
        let ordinal = word.pos == "接頭辞"
            && word.base_word == "第"
            && items.peek().is_some_and(|(w, s)| next_to(&word, &source, w, s) && is_number(s));
        let (prefix, (mut word, source)) = match ordinal {
            true => (Some((word, source)), items.next().expect("a number follows")),
            false => (None, (word, source)),
        };
        if !is_number(&source) {
            // A number and its counter UniDic keeps as one word (一人): with an ordinal 目 after it (一人目), which
            // UniDic may take for the noun 目 there (二人目を産む)
            let mut source = source;
            let counted = word.written_reading.is_some();
            let is_ordinal = |s: &Range<usize>| single(s).is_some_and(|t| t.field(7) == "目" && matches!(t.field(0), "接尾辞" | "名詞"));
            if let Some((ordinal, ordinal_source)) = items.next_if(|(w, s)| counted && next_to(&word, &source, w, s) && is_ordinal(s)) {
                word.span.end = ordinal.span.end;
                source.end = ordinal_source.end;
                word.written_reading.iter_mut().for_each(|reading| reading.push_str(&in_kana("め".to_string())));
                word.forms.push("ordinal (目)");
            }
            // A single kana UniDic takes for a number is not one (the シ of シッ), nor kana it takes for 第 (the ダイ
            // of ダイスキヤキ, the だい of だいぃぶ)
            let kana_number = single(&source).is_some_and(|t| t.field(1) == "数詞") && word.spelling.chars().count() == 1;
            let kana_ordinal = word.pos == "接頭辞" && word.base_word == "第" && single(&source).is_some_and(|t| t.surface != "第");
            if kana_number || kana_ordinal {
                word.kind = WordKind::Fragment;
            }
            attached.push(word);
            attached_sources.push(source);
            continue;
        }
        let first = prefix.as_ref().map_or((word.span.start, source.start), |(w, s)| (w.span.start, s.start));
        let numbers_start = source.start;
        let mut numbers = vec![(word, source)];
        while let Some(number) = items.next_if(|(w, s)| {
            let (last, last_source) = &numbers[numbers.len() - 1];
            next_to(last, last_source, w, s) && is_number(s)
        }) {
            numbers.push(number);
        }
        let (last, last_source) = &numbers[numbers.len() - 1];
        let (end, span_end) = (last_source.end, last.span.end);
        let number_tokens = &tokens[numbers_start..end];
        let ordinal_reading = |reading: String| if prefix.is_some() { format!("だい{reading}") } else { reading };
        // Not the decimals of a number (1.5倍), which UniDic splits at the point
        let decimals = numbers_start > 0 && matches!(tokens[numbers_start - 1].surface.as_str(), "." | "．");
        let counter = items.next_if(|(w, s)| !decimals && next_to(last, last_source, w, s) && single(s).is_some_and(is_counter));
        let Some((mut word, mut source)) = counter else {
            // Words such as 何 and 数 when no number comes with them
            if !number_tokens.iter().any(is_numeral) {
                attached.extend(prefix.iter().map(|(w, _)| w.clone()));
                attached_sources.extend(prefix.map(|(_, s)| s));
                for (word, source) in numbers.into_iter().filter(|(w, _)| is_valid_word(&w.spelling)) {
                    attached.push(word);
                    attached_sources.push(source);
                }
                continue;
            }
            let written: String = tokens[first.1..end].iter().map(|t| t.surface.as_str()).collect();
            let number = ParsedWord {
                pronunciation: in_kana(ordinal_reading(number_reading_of(number_tokens, ""))),
                pos: "名詞".to_string(),
                spelling: written.clone(),
                base_word: written,
                span: first.0..span_end,
                kind: WordKind::Number,
                affixes: Vec::new(),
                forms: if prefix.is_some() { vec!["ordinal (第)"] } else { Vec::new() },
                homograph: None,
                written_reading: None,
            };
            attached.push(number);
            attached_sources.push(first.1..end);
            continue;
        };
        let mut reading = ordinal_reading(number_with_counter(number_tokens, &tokens[source.start]));
        word.span.start = first.0;
        source.start = first.1;
        word.kind = WordKind::Word;
        if let Some((mut prefix, _)) = prefix {
            prefix.kind = WordKind::Grammar;
            word.affixes.push(prefix);
            word.forms.push("ordinal (第)");
        }
        if let Some((ordinal, ordinal_source)) = items.next_if(|(w, s)| next_to(&word, &source, w, s) && is_ordinal(s)) {
            word.span.end = ordinal.span.end;
            source.end = ordinal_source.end;
            reading.push('め');
            word.forms.push("ordinal (目)");
        }
        word.written_reading = Some(in_kana(reading));
        attached.push(word);
        attached_sources.push(source);
    }
    (attached, attached_sources)
}

/// Kanji for numbers, and digits: what a number is written with (一, 十, 万, 〇, 3, ３).
fn is_numeral_char(c: char) -> bool {
    "〇零一二三四五六七八九十百千万億兆".contains(c) || c.is_ascii_digit() || ('０'..='９').contains(&c)
}

/// A token that is a number, possibly an approximate one (二十, 2020, 数十, 何百), as written or as UniDic's lemma
/// (ひと and イチ are 一, 0 is ゼロ), but not 何 or 数 alone.
fn is_numeral(token: &Token) -> bool {
    let numeral = |text: &str| text.chars().any(is_numeral_char) && text.chars().all(|c| is_numeral_char(c) || "数何幾".contains(c));
    numeral(&token.surface) || numeral(token.field(7).split('-').next().unwrap_or_default())
}

/// The value of numbers written in digits (2020, ２０２０) or digit by digit in kanji (二〇二〇).
fn digits_value(numbers: &[Token]) -> Option<u64> {
    const KANJI: &str = "〇一二三四五六七八九";
    let digit = |c: char| {
        c.to_digit(10)
            .or_else(|| ('０'..='９').contains(&c).then(|| c as u32 - '０' as u32))
            .or_else(|| KANJI.chars().position(|k| k == c).map(|d| d as u32))
    };
    let chars: Vec<char> = numbers.iter().flat_map(|t| t.surface.chars()).collect();
    // In kanji, one digit (三, 四時 よじ), or digit by digit with a zero (二〇二〇); not 二三日 (に、さんにち)
    if chars.iter().any(|c| KANJI.contains(*c)) && chars.len() > 1 && !chars.contains(&'〇') {
        return None;
    }
    chars.into_iter().map(digit).try_fold(0u64, |value, d| value.checked_mul(10)?.checked_add(u64::from(d?)))
}

/// How `numbers` (tokens) are read before `counter` (a lemma, or empty): from their value when written in
/// digits, as UniDic reads them there otherwise (ヒト in 一つ, ミッ in 三日).
fn number_reading_of(numbers: &[Token], counter: &str) -> String {
    match digits_value(numbers) {
        Some(value) => number_reading(value, counter),
        None => numbers.iter().map(token_reading).collect(),
    }
}

/// A counter after a number: a suffix (つ, 人, 匹) or a noun UniDic says can count (円, 回, 時間).
fn is_counter(token: &Token) -> bool {
    match token.field(0) {
        "接尾辞" => token.field(1) == "名詞的",
        "名詞" => token.field(2) == "助数詞可能",
        _ => false,
    }
}

/// How `numbers` (tokens) and the `counter` after them are read together, in hiragana: the number as UniDic
/// reads it there (ヒト in 一つ, ミッ in 三日), or from its value when it is written in digits, with the sound
/// changes between a number and its counter (いっぽん, さんびき, じゅっぷん).
fn number_with_counter(numbers: &[Token], counter: &Token) -> String {
    let counter_reading = katakana_to_hiragana(counter.field(6));
    let lemma = counter.field(7).split('-').next().unwrap_or_default();
    if let Some(whole) = digits_value(numbers).and_then(|value| native_count(value, lemma)) {
        return whole.to_string();
    }
    join_counter(&number_reading_of(numbers, lemma), &counter_reading, lemma)
}

/// The reading of a token in context, in hiragana: its pronunciation, with the long vowels (ー) of its lemma
/// reading (ジュー → じゅう).
fn token_reading(token: &Token) -> String {
    let (pronunciation, lemma_reading) = (token.field(9), token.field(6));
    let reading = if !is_set(pronunciation) {
        lemma_reading.to_string()
    } else if pronunciation.chars().count() == lemma_reading.chars().count() {
        pronunciation.chars().zip(lemma_reading.chars()).map(|(p, l)| if p == 'ー' { l } else { p }).collect()
    } else {
        pronunciation.to_string()
    };
    katakana_to_hiragana(&reading)
}

/// Counts in digits read as one Japanese word (1つ ひとつ, 2人 ふたり, 3日 みっか).
fn native_count(value: u64, counter: &str) -> Option<&'static str> {
    const THINGS: [&str; 10] = ["ひとつ", "ふたつ", "みっつ", "よっつ", "いつつ", "むっつ", "ななつ", "やっつ", "ここのつ", "とお"];
    const DAYS: [&str; 9] = ["ふつか", "みっか", "よっか", "いつか", "むいか", "なのか", "ようか", "ここのか", "とおか"];
    Some(match (counter, value) {
        ("つ", 1..=10) => THINGS[value as usize - 1],
        ("人", 1) => "ひとり",
        ("人", 2) => "ふたり",
        ("日", 2..=10) => DAYS[value as usize - 2],
        ("日", 14) => "じゅうよっか",
        ("日", 20) => "はつか",
        ("日", 24) => "にじゅうよっか",
        _ => return None,
    })
}

/// A number read from its value (2020 → にせんにじゅう), with the 4, 7 and 9 some counters take (4時 よじ).
fn number_reading(value: u64, counter: &str) -> String {
    const DIGITS: [&str; 10] = ["", "いち", "に", "さん", "よん", "ご", "ろく", "なな", "はち", "きゅう"];
    if value == 0 {
        return "ぜろ".to_string();
    }
    // Below 10,000: thousands, hundreds, tens and ones, with their sound changes (さんびゃく, はっせん)
    let below_10000 = |n: u64, reading: &mut String| {
        let digit = |place: u64| (n / place % 10) as usize;
        match digit(1000) {
            0 => {}
            3 => reading.push_str("さんぜん"),
            8 => reading.push_str("はっせん"),
            d => reading.extend([if d == 1 { "" } else { DIGITS[d] }, "せん"]),
        }
        match digit(100) {
            0 => {}
            3 => reading.push_str("さんびゃく"),
            6 => reading.push_str("ろっぴゃく"),
            8 => reading.push_str("はっぴゃく"),
            d => reading.extend([if d == 1 { "" } else { DIGITS[d] }, "ひゃく"]),
        }
        match digit(10) {
            0 => {}
            d => reading.extend([if d == 1 { "" } else { DIGITS[d] }, "じゅう"]),
        }
        reading.push_str(DIGITS[digit(1)]);
    };
    let mut reading = String::new();
    for (unit, size) in [("ちょう", 1_000_000_000_000u64), ("おく", 100_000_000), ("まん", 10_000), ("", 1)] {
        let group = value / size % 10_000;
        if group > 0 {
            below_10000(group, &mut reading);
            reading.push_str(unit);
        }
    }
    let replace_end = |reading: &mut String, from: &str, to: &str| {
        if reading.ends_with(from) {
            reading.truncate(reading.len() - from.len());
            reading.push_str(to);
        }
    };
    match (value % 10, counter) {
        (4, "時" | "人" | "年") => replace_end(&mut reading, "よん", "よ"),
        (7, "時") => replace_end(&mut reading, "なな", "しち"),
        (9, "時") => replace_end(&mut reading, "きゅう", "く"),
        _ => {}
    }
    reading
}

/// A number followed by its counter (`lemma`, read `counter`), with the sound changes between them: a small
/// っ after いち, はち and じゅう (and after ろく and ひゃく before k and h), which turns h into p (いっぽん,
/// じゅっさい), and voicing after さん, なん, せん and まん (さんびき, なんがい, さんぷん).
fn join_counter(number: &str, counter: &str, lemma: &str) -> String {
    const K: &str = "かきくけこ";
    const S: &str = "さしすせそ";
    const T: &str = "たちつてと";
    const H: &str = "はひふへほ";
    let Some(first) = counter.chars().next() else { return number.to_string() };
    let rest = &counter[first.len_utf8()..];
    let shifted = |from: &str, to: &str| row_shift(first, from, to).map_or_else(|| counter.to_string(), |c| format!("{c}{rest}"));
    let in_row = |row: &str| row.contains(first);
    let geminates = if ["いち", "はち", "じゅう"].iter().any(|n| number.ends_with(n)) {
        in_row(K) || in_row(S) || in_row(T) || in_row(H)
    } else {
        ["ろく", "ひゃく"].iter().any(|n| number.ends_with(n)) && (in_row(K) || in_row(H))
    };
    if geminates {
        let stem = split_last(number).map_or(number, |(stem, _)| stem);
        return format!("{stem}っ{}", shifted(H, "ぱぴぷぺぽ"));
    }
    if ["さん", "なん", "せん", "まん"].iter().any(|n| number.ends_with(n)) {
        let counter = match lemma {
            "分" | "発" | "泊" | "版" => shifted(H, "ぱぴぷぺぽ"),
            "階" | "軒" => shifted(K, "がぎぐげご"),
            "足" => shifted(S, "ざじずぜぞ"),
            _ => shifted(H, "ばびぶべぼ"),
        };
        return format!("{number}{counter}");
    }
    format!("{number}{counter}")
}

/// Moves each conjugation ending into the word it follows (数え + られる → 数えられる, with られる as an
/// ending), and names what the conjugation means. `sources[i]` are the tokens `words[i]` is made of.
fn attach_endings(words: Vec<ParsedWord>, sources: Vec<Range<usize>>, tokens: &[Token], reading_in_hiragana: bool) -> Vec<ParsedWord> {
    let mut attached: Vec<ParsedWord> = Vec::with_capacity(words.len());
    // The last token of the last word, endings included
    let mut last_token: Option<usize> = None;
    for (mut word, source) in words.into_iter().zip(sources) {
        // Next to the word in the text, not only in tokens (line breaks have none)
        let ending = source.len() == 1
            && attached.last().is_some_and(|head: &ParsedWord| head.span.end == word.span.start)
            && last_token.is_some_and(|last| last + 1 == source.start && is_ending(&tokens[last], &tokens[source.start]));
        let (Some(head), true) = (attached.last_mut(), ending) else {
            let last = &tokens[source.end - 1];
            word.forms.extend(own_form(last));
            last_token = Some(source.end - 1);
            attached.push(word);
            continue;
        };
        let (previous, token) = (&tokens[source.start - 1], &tokens[source.start]);
        // Particles and auxiliaries under their lemma: the だ of 読んだ is た, the ん of 行かん is ず
        if matches!(token.field(0), "助動詞" | "助詞") {
            let lemma = token.field(7).split('-').next().unwrap_or_default();
            if is_set(lemma) {
                word.base_word = lemma.to_string();
                word.spelling = lemma.to_string();
                word.pronunciation = if reading_in_hiragana { katakana_to_hiragana(lemma) } else { hiragana_to_katakana(lemma) };
            }
        }
        word.kind = WordKind::Grammar;
        head.span.end = word.span.end;
        for form in ending_form(previous, token).into_iter().chain(own_form(token)) {
            if head.forms.last() != Some(&form) {
                head.forms.push(form);
            }
        }
        head.affixes.push(word);
        last_token = Some(source.start);
    }
    attached
}

/// Puts each polite prefix お or ご into the word after it (お二人 is 二人, ご出席 is 出席, お聞き is 聞く), unless
/// JMdict has the whole word (お茶, ご飯), which is then already one word; and 第 into a word that starts with a
/// number (第一軍 is 一軍), which UniDic sometimes reads as one word (第 before numbers is part of them: see
/// [`attach_counters`]).
fn attach_prefixes(words: Vec<ParsedWord>) -> Vec<ParsedWord> {
    let mut attached: Vec<ParsedWord> = Vec::with_capacity(words.len());
    let mut prefix: Option<ParsedWord> = None;
    for mut word in words {
        if let Some(mut polite) = prefix.take() {
            // A word to learn, next to the prefix in the text: not a particle or another interjection (お、お…)
            let ordinal = polite.base_word == "第";
            let fits = if ordinal { word.spelling.starts_with(is_numeral_char) } else { !matches!(word.pos.as_str(), "接頭辞" | "感動詞") };
            if polite.span.end == word.span.start && word.kind == WordKind::Word && fits {
                word.span.start = polite.span.start;
                // A counter is read with its number, and now its prefix too (お二人 おふたり)
                if let Some(reading) = word.written_reading.as_mut() {
                    reading.insert_str(0, &polite.pronunciation);
                }
                word.forms.insert(0, match polite.spelling.as_str() {
                    _ if ordinal => "ordinal (第)",
                    "ご" => "polite (ご)",
                    _ => "polite (お)",
                });
                polite.kind = WordKind::Grammar;
                word.affixes.insert(0, polite);
                attached.push(word);
                continue;
            }
            attached.push(polite);
        }
        let polite = word.pos == "接頭辞"
            && (word.base_word == "御" && matches!(word.spelling.as_str(), "お" | "ご" | "御") || word.spelling == "第" && word.kind == WordKind::Word);
        if polite {
            prefix = Some(word);
        } else {
            attached.push(word);
        }
    }
    attached.extend(prefix);
    attached
}

/// Whether `token` is a conjugation ending of the token before it, `previous` (itself a word or an ending):
/// - after a verb, adjective or auxiliary (or a suffix that conjugates, っぽい): auxiliaries (られる, た, ない,
///   ます), the particles that make conjugated forms (て, ば, たり, ながら), the stems of そうだ and ようだ, and
///   ない after an adjective (美しくない, 行きたくない; not after a noun: 関係ない)
/// - after a na-adjective, the stem of そうだ or げ: the copula (静かだった, 静かな, 寒そうだ, 悲しげな)
/// - the suffixes that make another word of an adjective, na-adjective or noun that can be one (静かさ, 寒がる,
///   子供っぽさ, 真剣さ), plurals of nouns (彼ら, 子供達, 私ども), and 方, 難い, やすい and づらい after the stem
///   of a verb (戦い方, 使いやすい, 食べ辛い): see [`is_derivation`]
///
/// Nouns take nothing else (学生だ, 勉強する), and verbs after て stay words of their own (書いて + しまう).
fn is_ending(previous: &Token, token: &Token) -> bool {
    if token.field(0) == "接尾辞" {
        return is_derivation(previous, token);
    }
    let previous_pos = previous.field(0);
    let na = previous_pos == "形状詞" || previous_pos == "接尾辞" && previous.field(1) == "形状詞的";
    let conjugates = na || matches!(previous_pos, "動詞" | "形容詞" | "助動詞") || previous_pos == "接尾辞" && is_set(previous.field(4));
    if !conjugates {
        return false;
    }
    let lemma = token.field(7);
    match token.field(0) {
        "助動詞" => !na || matches!(lemma, "だ" | "です"),
        "助詞" => !na && matches!(lemma, "て" | "ば" | "たり" | "ながら" | "つつ"),
        "形容詞" => {
            lemma == "無い" && token.field(1) == "非自立可能" && matches!(previous_pos, "形容詞" | "助動詞") && previous.field(5).starts_with("連用形")
                // 辛い after the stem of a verb, which UniDic sometimes takes for the adjective (食べ辛い)
                || lemma == "辛い" && matches!(token.field(6), "ツライ" | "ヅライ") && previous_pos == "動詞" && previous.field(5).starts_with("連用形")
        }
        "形状詞" => !na && token.field(1) == "助動詞語幹",
        _ => false,
    }
}

/// Whether the suffix `token` makes another word of the one before it, `previous` (JMdict words made this way
/// are already whole: 美しさ, 嬉しがる, 言い方): さ, み, げ, め and がる after the stem of an adjective (寒がる,
/// 子供っぽさ), a na-adjective (静かさ) or a noun that can be one (真剣さ), the plurals 達, ら and ども after
/// a noun or pronoun (子供達, 彼ら, 私ども), which may end in a suffix (ママさんたち, 候補者たち), and after the
/// stem of a verb, 方 (戦い方) and the adjectives 難い, にくい, やすい and づらい (耐え難い, 使いやすい), which
/// JMdict merging leaves apart even where JMdict has the whole (see [`longest_headword`]); っぽい after a noun, verb,
/// adjective or auxiliary (メイドっぽい, 助かるっぽい), and めく and めかす after a noun (儀式めいた, 冗談めかして) or
/// the 連用形 of a verb, which makes nouns (脅しめいた).
fn is_derivation(previous: &Token, token: &Token) -> bool {
    let (pos, sub) = (previous.field(0), previous.field(1));
    let stem = matches!(pos, "形容詞" | "接尾辞") && previous.field(5).starts_with("語幹")
        || pos == "形状詞" && sub != "助動詞語幹"
        || pos == "名詞" && previous.field(2) == "形状詞可能";
    // Also a noun ending in a suffix (ママさん, ケガ人, 候補者)
    let noun = matches!(pos, "名詞" | "代名詞") && sub != "数詞" || pos == "接尾辞" && sub == "名詞的";
    // Also of a passive or causative form (誤解されやすい)
    let verb_stem = previous.field(5).starts_with("連用形")
        && (pos == "動詞" || pos == "助動詞" && matches!(previous.field(7), "れる" | "られる" | "せる" | "させる"));
    match token.field(7) {
        "さ" | "み" | "気" | "目" | "がる" => stem,
        "方" => verb_stem && token.field(6) == "カタ",
        "難い" | "易い" | "辛い" => verb_stem,
        // After a noun, or a verb, adjective or auxiliary in any form (メイドっぽい, 助かるっぽい, 来たっぽい, 飽きっぽい)
        "ぽい" => noun || pos == "形状詞" || matches!(pos, "動詞" | "形容詞" | "助動詞"),
        // After a noun, also the 連用形 of a verb used as one (脅しめいた)
        "めく" | "めかす" => noun || pos == "形状詞" || verb_stem,
        "達" => noun,
        "等" => noun && token.field(6) == "ラ",
        "共" => noun && token.surface == "ども",
        _ => false,
    }
}

/// What a conjugation ending (see [`is_ending`]) means, with the ending in its usual form.
fn ending_form(previous: &Token, token: &Token) -> Option<&'static str> {
    let form = token.field(5);
    Some(match (token.field(0), token.field(7)) {
        ("助動詞", "れる") => "passive (れる)",
        ("助動詞", "られる") => "passive or potential (られる)",
        ("助動詞", "せる" | "させる") => "causative (させる)",
        ("助動詞", "た") if form.starts_with("仮定形") => "conditional (たら)",
        ("助動詞", "た") => "past (た)",
        ("助動詞", "ない") | ("形容詞", "無い") => "negative (ない)",
        ("助動詞", "ず" | "ぬ") => "negative (ず)",
        ("助動詞", "たい") => "want to (たい)",
        ("助動詞", "ます") => "polite (ます)",
        ("助動詞", "です") => "polite (です)",
        // The う of 行こう or だろう, unless the token before is already volitional
        ("助動詞", "う" | "よう") if previous.field(5) != "意志推量形" => "volitional (よう)",
        ("助動詞", "まい") => "negative volitional (まい)",
        ("助動詞", "らしい") => "seems (らしい)",
        ("助動詞", "ちゃう" | "じゃう" | "ちまう" | "じまう") => "completed (ちゃう)",
        ("助動詞", "てる" | "でる") => "ongoing (てる)",
        ("助動詞", "とく" | "どく") => "in advance (とく)",
        ("助動詞", "だ") if form == "連用形-ニ" => "adverb (に)",
        ("助動詞", "だ") if form == "連用形-一般" => "te-form (で)",
        ("助動詞", "だ") if form.starts_with("連体形") => "before a noun (な)",
        ("形状詞", "そう-様態") => "looks (そう)",
        ("形状詞", "そう-伝聞") => "hearsay (そう)",
        ("形状詞", "みたい") => "seems (みたい)",
        ("形状詞", lemma) if lemma.starts_with("よう") || lemma.starts_with('様') => "seems (よう)",
        ("助詞", "て") => "te-form (て)",
        ("助詞", "ば") => "conditional (ば)",
        ("助詞", "たり") => "listing (たり)",
        ("助詞", "ながら" | "つつ") => "while (ながら)",
        ("接尾辞", "さ") => "noun of degree (さ)",
        ("接尾辞", "み") => "noun (み)",
        ("接尾辞", "気") => "seeming (げ)",
        ("接尾辞", "目") => "somewhat (め)",
        ("接尾辞", "がる") => "shows signs of (がる)",
        ("接尾辞", "方") => "way of (方)",
        ("接尾辞", "難い") if token.field(6) == "ガタイ" => "hard to (がたい)",
        ("接尾辞", "難い") => "hard to (にくい)",
        ("接尾辞", "易い") => "easy to (やすい)",
        ("接尾辞" | "形容詞", "辛い") => "hard to (づらい)",
        ("接尾辞", "ぽい") => "-ish (っぽい)",
        ("接尾辞", "めく") => "-like (めく)",
        ("接尾辞", "めかす") => "making it seem (めかす)",
        ("接尾辞", "達") => "plural (たち)",
        ("接尾辞", "等") => "plural (ら)",
        ("接尾辞", "共") => "plural (ども)",
        _ => return None,
    })
}

/// A form a token is conjugated in that no ending shows: imperative (書け) and volitional (食べよう, だろう).
fn own_form(token: &Token) -> Option<&'static str> {
    let form = token.field(5);
    if form.starts_with("命令形") {
        Some("imperative")
    } else if form.starts_with("意志推量形") {
        Some(match token.field(7) {
            "だ" => "presumptive (だろう)",
            "です" => "presumptive (でしょう)",
            _ => "volitional (よう)",
        })
    } else {
        None
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
            // Numbers with counters (三人, 一年生), and 一人 or 二人 with an ordinal (二人目; not 二人連れ)
            || (start..tail).any(|i| {
                field(i, 1) == "数詞" && (field(i + 1, 0) == "接尾辞" || field(i + 1, 2).starts_with("助数詞"))
                    || matches!(field(i, 7), "一人" | "二人") && field(i + 1, 7) == "目"
            })
        {
            continue;
        }
        let head: String = tokens[start..tail].iter().map(|t| t.surface.as_str()).collect();
        let (surface, base) = (tokens[tail].surface.as_str(), field(tail, 10));
        // Also a suffix that conjugates (嘘っぽかった → 嘘っぽい, 謎めいた → 謎めく)
        let conjugates = matches!(field(tail, 0), "動詞" | "形容詞") || field(tail, 0) == "接尾辞" && is_set(field(tail, 4));
        let conjugated = conjugates && is_set(base) && surface != base;
        // A conjugated ending is looked up in its base form (気に入っ → 気に入る), or as written when it is the
        // 連用形 that also makes nouns (拍子抜け, 魔法使い), or a suffix (人たらし, not 人たらしい)
        let written = (!conjugated || field(tail, 5).starts_with("連用形") || field(tail, 0) == "接尾辞").then(|| format!("{head}{surface}"));
        let base = conjugated.then(|| format!("{head}{base}"));
        if !head.chars().chain(surface.chars()).any(is_cjk_kanji) {
            continue;
        }
        // Noun + ない when the noun is a word of its own (関係ない), unlike 不甲斐ない
        if field(tail, 7) == "無い" && headwords.headword(&head).is_some() {
            continue;
        }
        // A suffix as written first, as before suffixes were looked up in their base form (強がり, not 強がる)
        let candidates = if field(tail, 0) == "接尾辞" { [written, base] } else { [base, written] };
        for candidate in candidates.into_iter().flatten() {
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
            [("あ", Fragment), ("グレイ", Word), ("ラット", Word), ("家", Word), ("の", Grammar), ("本", Word), ("を", Grammar), ("５冊", Word), ("読んでる", Word)]
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
        let expected = [("冒険者", "冒険者"), ("神出鬼没", "神出鬼没"), ("怪盗", "怪盗"), ("見る", "見た")];
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
        assert_eq!(spans, ["握り", "直した"]);
    }

    /// Each word as written, its dictionary form, what its conjugation means and its endings.
    fn conjugated(text: &str) -> Vec<(String, String, String, String)> {
        let mut tokenizer = JapaneseTokenizer::new(None, true).unwrap();
        let words = tokenizer.tokenize(text).unwrap();
        let join = |parts: Vec<&str>| parts.join(", ");
        words
            .iter()
            .map(|w| {
                let endings = join(w.affixes.iter().map(|e| e.base_word.as_str()).collect());
                (text[w.span.clone()].to_string(), w.base_word.clone(), join(w.forms.clone()), endings)
            })
            .collect()
    }

    /// What identifies each word listed in `text` in counts.
    fn counted(text: &str) -> Vec<String> {
        let mut tokenizer = JapaneseTokenizer::new(None, true).unwrap();
        tokenizer.tokenize(text).unwrap().iter().filter(|w| w.is_listed(false)).map(|w| w.key().into_owned()).collect()
    }

    /// Each number with its counter as written, the counter it is counted as, and how it is read.
    fn counted_with_numbers(text: &str) -> Vec<(String, String, String)> {
        let mut tokenizer = JapaneseTokenizer::new(None, true).unwrap();
        let words = tokenizer.tokenize(text).unwrap();
        words
            .iter()
            .filter_map(|w| Some((text[w.span.clone()].to_string(), w.key().into_owned(), w.written_reading.clone()?)))
            .collect()
    }

    #[test]
    fn keeps_numbers_with_their_counter() {
        let one = |written: &str, word: &str, reading: &str| vec![(written.to_string(), word.to_string(), reading.to_string())];
        // Read as UniDic reads the number there, with the sound changes before the counter
        assert_eq!(counted_with_numbers("一つ"), one("一つ", "つ", "ひとつ"));
        assert_eq!(counted_with_numbers("二十三人"), one("二十三人", "人【にん】", "にじゅうさんにん"));
        assert_eq!(counted_with_numbers("一本"), one("一本", "本", "いっぽん"));
        assert_eq!(counted_with_numbers("三匹"), one("三匹", "匹", "さんびき"));
        assert_eq!(counted_with_numbers("十分後"), one("十分", "分", "じゅっぷん"));
        assert_eq!(counted_with_numbers("三日"), one("三日", "日", "みっか"));
        assert_eq!(counted_with_numbers("三階"), one("三階", "階", "さんがい"));
        assert_eq!(counted_with_numbers("一つ目"), one("一つ目", "つ", "ひとつめ"));
        // Also 一人 and 二人, which UniDic keeps as one word, in kanji or kana, and with an ordinal; not the words
        // JMdict makes of them (一人一人, 二人組)
        assert_eq!(counted_with_numbers("二人で"), one("二人", "人【にん】", "ふたり"));
        assert_eq!(counted_with_numbers("ひとりで"), one("ひとり", "人【にん】", "ひとり"));
        assert_eq!(counted_with_numbers("一人目"), one("一人目", "人【にん】", "ひとりめ"));
        assert_eq!(counted_with_numbers("二人目"), one("二人目", "人【にん】", "ふたりめ"));
        assert_eq!(counted_with_numbers("二人目を産む"), one("二人目", "人【にん】", "ふたりめ"));
        assert_eq!(counted_with_numbers("お二人"), one("お二人", "人【にん】", "おふたり"));
        assert_eq!(counted("一人一人の二人組と二人連れ"), ["一人一人", "二人組", "二人連れ"]);
        // 第 makes an ordinal
        assert_eq!(counted_with_numbers("第二話"), one("第二話", "話【わ】", "だいにわ"));
        // Numbers in digits, read from their value
        assert_eq!(counted_with_numbers("10人"), one("10人", "人【にん】", "じゅうにん"));
        assert_eq!(counted_with_numbers("1人"), one("1人", "人【にん】", "ひとり"));
        assert_eq!(counted_with_numbers("3つ"), one("3つ", "つ", "みっつ"));
        assert_eq!(counted_with_numbers("2020年"), one("2020年", "年", "にせんにじゅうねん"));
        assert_eq!(counted_with_numbers("4時"), one("4時", "時", "よじ"));
        // Counted as the counter, which is a word to learn even when a single kana; numbers with no counter are
        // one number, never listed, and decimals keep their counter apart
        assert_eq!(counted("一つ"), ["つ"]);
        let numbers = |text: &str| {
            let mut tokenizer = JapaneseTokenizer::new(None, true).unwrap();
            let words = tokenizer.tokenize(text).unwrap();
            words.iter().filter(|w| w.kind == WordKind::Number).map(|w| (text[w.span.clone()].to_string(), w.pronunciation.clone())).collect::<Vec<_>>()
        };
        let pairs = |list: &[(&str, &str)]| list.iter().map(|&(w, r)| (w.to_string(), r.to_string())).collect::<Vec<_>>();
        assert_eq!(numbers("二十三"), pairs(&[("二十三", "にじゅうさん")]));
        assert_eq!(numbers("二〇二〇年まで2020"), pairs(&[("2020", "にせんにじゅう")]));
        assert_eq!(numbers("第三章"), pairs(&[("第三", "だいさん")]));
        assert_eq!(numbers("数十"), pairs(&[("数十", "すうじゅう")]));
        assert_eq!(counted("二十三の"), Vec::<String>::new());
        assert_eq!(counted("何が"), ["何"]);
        assert_eq!(counted_with_numbers("ひとつ"), one("ひとつ", "つ", "ひとつ"));
        assert_eq!(counted("3"), Vec::<String>::new());
        assert_eq!(counted("1.5倍"), ["倍"]);
        // Only numbers in kanji or digits, and only next to their counter in the text
        assert_eq!(counted("「くふ」"), counted("ふ"));
        assert_eq!(counted_with_numbers("12\nら"), []);
    }

    #[test]
    fn counts_homographs_apart() {
        // 様 is read さま in エミリア様 and よう in ように, which JMdict has as words of their own
        assert_eq!(counted("エミリア様"), ["エミリア", "様"]);
        assert_eq!(counted("猫のように"), ["猫", "様【よう】"]);
        assert_eq!(counted("その様な"), ["その", "様【よう】"]);
        assert_eq!(counted("部屋の中"), ["部屋", "中【なか】"]);
        assert_eq!(counted("やめる"), ["止める【やめる】"]);
        assert_eq!(counted("とめる"), ["止める"]);
        // Entries that share a meaning are one word: わたし and わたくし, なに and なん, and also 方 かた and ほう
        assert_eq!(counted("私"), counted("わたし"));
        assert_eq!(counted("何"), counted("なん"));
        assert_eq!(counted("こっちの方が"), ["こちら", "方"]);
    }

    #[test]
    fn keeps_conjugation_endings_with_their_word() {
        let one = |written: &str, word: &str, forms: &str, endings: &str| vec![(written.into(), word.into(), forms.into(), endings.into())];
        assert_eq!(conjugated("数えられる"), one("数えられる", "数える", "passive or potential (られる)", "られる"));
        assert_eq!(conjugated("あった"), one("あった", "有る", "past (た)", "た"));
        assert_eq!(
            conjugated("食べさせられた"),
            one("食べさせられた", "食べる", "causative (させる), passive or potential (られる), past (た)", "させる, られる, た")
        );
        // The ない of an adjective is its negative, and the だ of 読んだ is た
        assert_eq!(conjugated("美しくなかった"), one("美しくなかった", "美しい", "negative (ない), past (た)", "無い, た"));
        assert_eq!(conjugated("読んだ"), one("読んだ", "読む", "past (た)", "た"));
        assert_eq!(conjugated("静かだった"), one("静かだった", "静か", "past (た)", "だ, た"));
        assert_eq!(conjugated("高かったら"), one("高かったら", "高い", "conditional (たら)", "た"));
        assert_eq!(conjugated("早く書け").remove(1), one("書け", "書く", "imperative", "").remove(0));
        // Suffixes that make another word, and plurals; JMdict's words made this way stay whole
        assert_eq!(conjugated("静かさ"), one("静かさ", "静か", "noun of degree (さ)", "さ"));
        assert_eq!(conjugated("眩しがった"), one("眩しがった", "眩しい", "shows signs of (がる), past (た)", "がる, た"));
        // JMdict's word, also conjugated
        assert_eq!(conjugated("寒がった"), one("寒がった", "寒がる", "past (た)", "た"));
        assert_eq!(conjugated("悲しげな"), one("悲しげな", "悲しげ", "before a noun (な)", "だ"));
        assert_eq!(conjugated("彼ら"), one("彼ら", "彼", "plural (ら)", "等"));
        assert_eq!(conjugated("子供達"), one("子供達", "子供", "plural (たち)", "達"));
        assert_eq!(conjugated("候補者たち"), one("候補者たち", "候補者", "plural (たち)", "達"));
        assert_eq!(conjugated("美しさ"), one("美しさ", "美しさ", "", ""));
        // After the stem of a verb: 方 unless JMdict has the whole, and 難い, やすい and づらい always
        assert_eq!(conjugated("戦い方"), one("戦い方", "戦う", "way of (方)", "方"));
        assert_eq!(conjugated("言い方"), one("言い方", "言い方", "", ""));
        assert_eq!(conjugated("耐え難い"), one("耐え難い", "耐える", "hard to (がたい)", "難い"));
        assert_eq!(conjugated("書きにくかった"), one("書きにくかった", "書く", "hard to (にくい), past (た)", "難い, た"));
        assert_eq!(conjugated("使いやすい"), one("使いやすい", "使う", "easy to (やすい)", "易い"));
        assert_eq!(conjugated("騙されやすい"), one("騙されやすい", "騙す", "passive (れる), easy to (やすい)", "れる, 易い"));
        assert_eq!(conjugated("居づらい"), one("居づらい", "居る", "hard to (づらい)", "辛い"));
        assert_eq!(conjugated("食べ辛い"), one("食べ辛い", "食べる", "hard to (づらい)", "辛い"));
        // っぽい after a noun or verb, めく and めかす after a noun, unless JMdict has the whole, also conjugated
        assert_eq!(conjugated("メイドっぽい"), one("メイドっぽい", "メイド", "-ish (っぽい)", "っぽい"));
        assert_eq!(conjugated("来たっぽい"), one("来たっぽい", "来る", "past (た), -ish (っぽい)", "た, っぽい"));
        assert_eq!(conjugated("儀式めいた"), one("儀式めいた", "儀式", "-like (めく), past (た)", "めく, た"));
        assert_eq!(conjugated("脅しめいた").remove(0).2, "-like (めく), past (た)");
        assert_eq!(conjugated("嘘っぽかった"), one("嘘っぽかった", "嘘っぽい", "past (た)", "た"));
        assert_eq!(conjugated("子供っぽさ"), one("子供っぽさ", "子供っぽい", "noun of degree (さ)", "さ"));
        assert_eq!(conjugated("謎めいた"), one("謎めいた", "謎めく", "past (た)", "た"));
        assert_eq!(conjugated("嬉しがった"), one("嬉しがった", "嬉しがる", "past (た)", "た"));
        // Polite prefixes, unless JMdict has the whole word
        assert_eq!(conjugated("お二人"), one("お二人", "人", "polite (お)", "御"));
        assert_eq!(conjugated("ご出席"), one("ご出席", "出席", "polite (ご)", "御"));
        assert_eq!(conjugated("お聞き"), one("お聞き", "聞く", "polite (お)", "御"));
        assert_eq!(conjugated("お茶"), one("お茶", "お茶", "", ""));
        // Verbs after て, nouns and what follows them stay words of their own
        let written = |text: &str| conjugated(text).into_iter().map(|(w, ..)| w).collect::<Vec<_>>();
        assert_eq!(written("それによって"), ["それ", "に", "よって"]);
        assert_eq!(written("書いてしまった"), ["書いて", "しまった"]);
        assert_eq!(written("読まれている"), ["読まれて", "いる"]);
        assert_eq!(written("勉強した"), ["勉強", "した"]);
        assert_eq!(written("関係なかった"), ["関係", "なかった"]);
        assert_eq!(written("学生だった"), ["学生", "だった"]);
        assert_eq!(written("お、おじゃま"), ["お", "おじゃま"]);
        assert_eq!(written("行った方がいい"), ["行った", "方", "が", "いい"]);
        assert_eq!(written("塩辛い"), ["塩辛い"]);
        assert_eq!(written("強がりを言った"), ["強がり", "を", "言った"]);
        // Word lists count the word once, and its endings as grammar
        assert_eq!(words("食べさせられた"), ["食べる"]);
        let mut tokenizer = JapaneseTokenizer::new(None, true).unwrap();
        let listed: Vec<String> = tokenizer
            .tokenize("美しくなかった")
            .unwrap()
            .iter()
            .flat_map(|w| w.with_affixes())
            .filter(|w| w.is_listed(true))
            .map(|w| w.base_word.clone())
            .collect();
        assert_eq!(listed, ["美しい", "無い", "た"]);
    }

    #[test]
    fn keeps_okurigana_of_the_surface() {
        assert_eq!(words("抑えた"), ["抑える"]);
        assert_eq!(words("治せる"), ["治す"]);
    }
}
