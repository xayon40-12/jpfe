//! How a word is used in its sentence: which dictionary entry and sense it is, and so which part of speech colors
//! it in the reader. The highlight and the dictionary window both use [`usage`], so they always agree.
//!
//! UniDic tags each word, and JMdict lists what a word can be. They often disagree on the part of speech (UniDic
//! tags 完璧 a noun that can be a な-adjective, JMdict lists it as a な-adjective first), so the reader starts from
//! the entry the window shows and chooses its sense from how the sentence uses the word: the token after it, as
//! UniDic tags it (the な or に of a な-adjective is a copula, the に of a noun a particle).

use jpfe::dictionary::{Definitions, Entry, Sense};
use jpfe::tokenizer::{hiragana_to_katakana, ParsedWord, WordKind};

use crate::theme::WordFills;
use egui::Color32;

/// What part of a word some of its characters are, which gives their color when the reader highlights words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Noun,
    /// Names of people, places, organizations (名詞-固有名詞) that JMdict has no common word for
    ProperNoun,
    Verb,
    /// い- and な-adjectives
    Adjective,
    Adverb,
    /// A counter, after its number (５冊)
    Counter,
    Pronoun,
    /// Interjections, conjunctions, adnominals, expressions
    Other,
    /// The prefix before a word: polite (お, ご), ordinal (第), or a kanji prefix (大)
    Prefix,
    /// Conjugation endings and auxiliaries after a word (られる, た, て, ない)
    Ending,
    /// Suffixes after a word (さ, ら, たち)
    Suffix,
    /// Particles and auxiliaries
    Grammar,
    /// Numbers (also the number of a counter) and fragments
    Faint,
}

impl Part {
    /// The word itself, from UniDic's tags alone (before the dictionary is loaded). A word of its own is never a
    /// suffix or a prefix: those are inside the words they belong to.
    pub fn of_unidic(word: &ParsedWord) -> Self {
        match word.kind {
            WordKind::Grammar => Self::Grammar,
            WordKind::Number | WordKind::Fragment => Self::Faint,
            WordKind::Word if word.written_reading.is_some() => Self::Counter,
            WordKind::Word => match word.pos.as_str() {
                "名詞" if word.proper_noun => Self::ProperNoun,
                "動詞" => Self::Verb,
                "形容詞" | "形状詞" => Self::Adjective,
                "副詞" => Self::Adverb,
                "代名詞" => Self::Pronoun,
                "名詞" | "接尾辞" | "接頭辞" => Self::Noun,
                _ => Self::Other,
            },
        }
    }

    /// One of a word's affixes (see [`ParsedWord::affixes`]): prefixes are tagged 接頭辞 and suffixes 接尾辞;
    /// the rest are conjugation endings (auxiliaries, the particles of conjugated forms such as て, the ない of
    /// 美しくない, the stems of そうだ and ようだ), except the adjective づらい (辛い), a suffix like やすい.
    pub fn of_affix(affix: &ParsedWord) -> Self {
        match affix.pos.as_str() {
            "接頭辞" => Self::Prefix,
            "接尾辞" => Self::Suffix,
            "形容詞" if !matches!(affix.base_word.as_str(), "無い" | "ない") => Self::Suffix,
            _ => Self::Ending,
        }
    }

    /// The part a JMdict part-of-speech tag stands for (`None` for usage and field tags such as `uk` or `astron`).
    pub fn of_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            // vs: a noun that takes する
            "n" | "n-adv" | "n-t" | "n-pref" | "n-suf" | "adj-no" | "vs" => Self::Noun,
            "n-pr" => Self::ProperNoun,
            "adj-na" | "adj-i" | "adj-ix" | "adj-t" | "adj-f" | "adj-nari" | "adj-ku" | "adj-shiku" | "adj-kari" => Self::Adjective,
            "adv" | "adv-to" => Self::Adverb,
            "pn" => Self::Pronoun,
            "ctr" => Self::Counter,
            "suf" => Self::Suffix,
            "pref" => Self::Prefix,
            "prt" | "aux" | "aux-v" | "aux-adj" | "cop" => Self::Grammar,
            "conj" | "int" | "adj-pn" | "exp" | "unc" => Self::Other,
            "num" => Self::Faint,
            t if t.starts_with('v') && !matches!(t, "vt" | "vi" | "vulg" | "vidg") => Self::Verb,
            _ => return None,
        })
    }

    pub fn fill(self, fills: &WordFills) -> Color32 {
        match self {
            Self::Noun => fills.noun,
            Self::ProperNoun => fills.proper_noun,
            Self::Verb => fills.verb,
            Self::Adjective => fills.adjective,
            Self::Adverb => fills.adverb,
            Self::Counter => fills.counter,
            Self::Pronoun => fills.pronoun,
            Self::Other => fills.other,
            Self::Prefix => fills.prefix,
            Self::Ending => fills.ending,
            Self::Suffix => fills.suffix,
            Self::Grammar => fills.grammar,
            Self::Faint => fills.faint,
        }
    }

    /// What the dictionary window says the word is used as.
    pub fn label(self) -> &'static str {
        match self {
            Self::Noun => "noun",
            Self::ProperNoun => "name",
            Self::Verb => "verb",
            Self::Adjective => "adjective",
            Self::Adverb => "adverb",
            Self::Counter => "counter",
            Self::Pronoun => "pronoun",
            Self::Other => "expression",
            Self::Prefix => "prefix",
            Self::Ending => "ending",
            Self::Suffix => "suffix",
            Self::Grammar => "grammar",
            Self::Faint => "number",
        }
    }
}

/// What comes right after a word's stem in the text: its first ending, or the next word when nothing (no
/// punctuation) is between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// The copula だ or です in any form, as UniDic tags the な, に or で of a な-adjective (完璧に, 完璧な)
    Copula,
    /// A particle (助詞): the に, の, が, を of a noun
    Particle,
    /// The particle と, which follows adverbs as well as nouns (意外と, ゆっくりと, 完璧と言える)
    To,
    /// A verb or an adjective (絶対負けない, 一番簡単), which an adverb comes right before
    Predicate,
    /// Another word
    Other,
    /// A particle that ends a sentence (大丈夫よ, 本当か), which tells nothing of the word
    Final,
    /// Punctuation, or the end of the paragraph
    Nothing,
}

impl Next {
    /// After `word`, which `following` comes right after in the text (if anything does).
    pub fn after(word: &ParsedWord, following: Option<&ParsedWord>) -> Self {
        // The first ending after the stem, else the next word
        let stem_start = word.affixes.iter().filter(|a| a.pos == "接頭辞").map(|a| a.span.end).max().unwrap_or(word.span.start);
        let ending = word.affixes.iter().filter(|a| a.pos != "接頭辞" && a.span.start >= stem_start).min_by_key(|a| a.span.start);
        let Some(next) = ending.or(following) else { return Self::Nothing };
        match next.pos.as_str() {
            "助動詞" if matches!(next.base_word.as_str(), "だ" | "です") => Self::Copula,
            "助詞" if next.base_word == "と" => Self::To,
            "助詞" if matches!(next.base_word.as_str(), "よ" | "ね" | "ねえ" | "か" | "な" | "わ" | "ぞ" | "ぜ" | "さ" | "かしら" | "っけ" | "かな") => Self::Final,
            "助詞" => Self::Particle,
            "動詞" | "形容詞" | "形状詞" => Self::Predicate,
            _ => Self::Other,
        }
    }
}

/// How a word is used: its entries with the one it is used as first, the sense of that entry it is used in (when
/// one fits), and the part of speech that colors it.
pub struct Usage<'d> {
    pub entries: Vec<Entry<'d>>,
    pub sense: Option<usize>,
    pub part: Part,
}

impl Usage<'_> {
    /// What the window says the word is used as: the tag of the sense used (な-adjective rather than adjective),
    /// or else the part.
    pub fn label(&self) -> &'static str {
        let tag = self.sense.and_then(|s| self.entries.first()?.senses.get(s)).and_then(|s| s.tags.iter().find(|t| Part::of_tag(t) == Some(self.part)));
        match tag.copied() {
            Some("adj-na") => "な-adjective",
            Some("adj-i" | "adj-ix") => "い-adjective",
            Some("adj-no") => "noun (takes の)",
            Some("adj-pn") => "adnominal",
            Some("int") => "interjection",
            Some("conj") => "conjunction",
            Some("prt") => "particle",
            Some("aux" | "aux-v" | "aux-adj" | "cop") => "auxiliary",
            _ if self.part == Part::Noun && self.entries.first().is_some_and(|e| e.senses.iter().all(|s| s.tags.iter().any(|t| matches!(*t, "suf" | "pref")))) => {
                "noun (a suffix on its own)"
            }
            _ => self.part.label(),
        }
    }
}

fn read_so(entry: &Entry, word: &ParsedWord) -> bool {
    let katakana = hiragana_to_katakana(&word.pronunciation);
    entry.readings.iter().any(|r| *r == word.pronunciation || *r == katakana)
}

/// The first sense of `entry` that `fits`, and the part it has there.
fn sense_where(entry: &Entry, fits: impl Fn(Part, &str) -> bool) -> Option<(usize, Part)> {
    entry.senses.iter().enumerate().find_map(|(i, s): (usize, &Sense)| s.tags.iter().find_map(|t| Part::of_tag(t).filter(|&p| fits(p, t))).map(|p| (i, p)))
}

/// The word's entries in the order the dictionary window lists them: those of its part of speech first (the
/// particle は, not the noun 歯), and among them, first, the one read as in the text.
fn entries<'d>(definitions: &'d Definitions, word: &ParsedWord) -> Vec<Entry<'d>> {
    let mut entries = definitions.lookup_as(&word.base_word, &word.pos);
    let first = entries.iter().position(|e| read_so(e, word) && e.is_part_of_speech(&word.pos)).or_else(|| entries.iter().position(|e| read_so(e, word)));
    if let Some(i) = first {
        entries[..=i].rotate_right(1);
    }
    entries
}

/// How `word` is used, followed by `next` (see the module's documentation):
/// 1. grammar is grammar, in the entry and sense of a particle or an auxiliary (のだ's ん is の, not "yes");
/// 2. numbers are numbers, and a number with its counter is the counter, in its counter sense;
/// 3. a suffix or a prefix that is a word of its own is what it can be alone, a noun when it can only be an affix
///    (さん, 様);
/// 4. a name (固有名詞) stays a name unless JMdict has a common word read the same (伊達 is だて);
/// 5. a noun, な-adjective or adverb is a な-adjective before a copula, a noun before a particle (a noun also before
///    と; never an adverb UniDic tags so, nor before a particle that ends the sentence), and an adverb before a verb
///    or an adjective, when the entry the window shows has such a sense;
/// 6. otherwise, the first sense of its entry, unless UniDic's verb, pronoun, adnominal, adverb or な-adjective is
///    one of its senses (UniDic tagged it so from the sentence; a noun is left to JMdict, which tells nouns that are
///    な-adjectives apart).
pub fn usage<'d>(definitions: &'d Definitions, word: &ParsedWord, next: Next) -> Usage<'d> {
    let entries = entries(definitions, word);
    let unidic = Part::of_unidic(word);
    // Moves the entry `i` first, and uses it in sense `sense` as `part`
    let choose = |mut entries: Vec<Entry<'d>>, found: Option<(usize, usize, Part)>, otherwise: Part| match found {
        Some((i, sense, part)) => {
            entries[..=i].rotate_right(1);
            Usage { entries, sense: Some(sense), part }
        }
        None => Usage { entries, sense: None, part: otherwise },
    };
    // The first entry, among those read as in the text (or all, when none is), with a sense that fits
    let find = |entries: &[Entry], fits: &dyn Fn(Part, &str) -> bool| {
        let read: Vec<usize> = (0..entries.len()).filter(|&i| read_so(&entries[i], word)).collect();
        let candidates = if read.is_empty() { (0..entries.len()).collect() } else { read };
        candidates.into_iter().find_map(|i| sense_where(&entries[i], fits).map(|(s, p)| (i, s, p)))
    };

    match word.kind {
        WordKind::Grammar => {
            let found = find(&entries, &|p, _| p == Part::Grammar);
            // A contraction looked up under its own spelling (ん) may only be found as what it stands for (の)
            if found.is_none() && word.pronunciation != word.base_word {
                let mut more = definitions.lookup_as(&word.pronunciation, &word.pos);
                if let Some((i, sense)) = more.iter().enumerate().find_map(|(i, e)| sense_where(e, |p, _| p == Part::Grammar).map(|(s, _)| (i, s))) {
                    more[..=i].rotate_right(1);
                    more.extend(entries);
                    return Usage { entries: more, sense: Some(sense), part: Part::Grammar };
                }
            }
            return choose(entries, found, Part::Grammar);
        }
        WordKind::Number | WordKind::Fragment => return Usage { entries, sense: None, part: Part::Faint },
        WordKind::Word => {}
    }
    if word.written_reading.is_some() {
        let found = find(&entries, &|p, _| p == Part::Counter);
        return choose(entries, found, Part::Counter);
    }
    if matches!(word.pos.as_str(), "接尾辞" | "接頭辞") {
        let found = find(&entries, &|p, _| !matches!(p, Part::Suffix | Part::Prefix | Part::Counter));
        return choose(entries, found, Part::Noun);
    }
    // A common word written and read as the name is (悪魔, 伊達; not グレイ, whose common spelling is グレー)
    if word.proper_noun && !entries.iter().any(|e| read_so(e, word) && e.common.contains(&word.base_word.as_str())) {
        return Usage { entries, sense: None, part: Part::ProperNoun };
    }
    if entries.is_empty() {
        return Usage { entries, sense: None, part: if unidic == Part::ProperNoun { Part::Noun } else { unidic } };
    }
    // In the entry the window shows, not in other words read the same (どう before a particle is not 堂)
    let pos = word.pos.as_str();
    if matches!(pos, "名詞" | "形状詞" | "副詞") {
        let sense = match next {
            Next::Copula => sense_where(&entries[0], |_, t| t == "adj-na"),
            // A noun before a particle, in a real noun sense (not one that takes の or する); an adverb UniDic tags
            // so stays one before に or と (すぐに, 次々と)
            Next::Particle if pos != "副詞" => sense_where(&entries[0], |_, t| matches!(t, "n" | "n-adv" | "n-t" | "n-suf" | "n-pref")),
            Next::To if pos == "名詞" => sense_where(&entries[0], |_, t| matches!(t, "n" | "n-adv" | "n-t" | "n-suf" | "n-pref")),
            // Before a verb or an adjective. Not before punctuation: a noun stays a noun (時、 その後、 are not the
            // adverbs "occasion", "more"), a な-adjective an adjective (大丈夫。); an adverb UniDic tags so stays one below
            Next::Predicate => sense_where(&entries[0], |p, _| p == Part::Adverb),
            _ => None,
        };
        if let Some((sense, part)) = sense {
            return Usage { entries, sense: Some(sense), part };
        }
    }
    // UniDic's part when the entry has it (but a noun); else the entry's first sense, which is not an affix (a word
    // of its own is not one)
    if matches!(unidic, Part::Verb | Part::Pronoun | Part::Other | Part::Adverb | Part::Adjective) {
        if let Some((sense, part)) = sense_where(&entries[0], |p, _| p == unidic) {
            return Usage { entries, sense: Some(sense), part };
        }
    }
    match sense_where(&entries[0], |p, _| !matches!(p, Part::Suffix | Part::Prefix | Part::Counter | Part::Faint)) {
        Some((sense, part)) => Usage { entries, sense: Some(sense), part: if part == Part::ProperNoun { Part::Noun } else { part } },
        None => Usage { entries, sense: None, part: if unidic == Part::ProperNoun { Part::Noun } else { unidic } },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jpfe::tokenizer::JapaneseTokenizer;
    use std::sync::OnceLock;

    fn definitions() -> &'static Definitions {
        static DEFINITIONS: OnceLock<Definitions> = OnceLock::new();
        DEFINITIONS.get_or_init(Definitions::embedded)
    }

    /// How the word written `surface` in `text` is used: its part, and the start of the glosses of its sense.
    fn used(text: &str, surface: &str) -> (Part, String) {
        let mut tokenizer = JapaneseTokenizer::new(None, true).unwrap();
        let mut words = tokenizer.tokenize(text).unwrap();
        words.sort_by_key(|w| w.span.start);
        let i = words.iter().position(|w| &text[w.span.clone()] == surface).unwrap_or_else(|| panic!("{surface} in {text}"));
        let following = words.get(i + 1).filter(|w| w.span.start == words[i].span.end);
        let usage = usage(definitions(), &words[i], Next::after(&words[i], following));
        let gloss = usage.sense.map_or(String::new(), |s| usage.entries[0].senses[s].glosses.chars().take(12).collect());
        (usage.part, gloss)
    }

    #[test]
    fn a_noun_that_can_be_a_na_adjective_is_one_before_a_copula() {
        // UniDic tags 完璧 a noun, and the に after it a copula: a な-adjective, used adverbially
        assert_eq!(used("心は完璧に折れていた", "完璧").0, Part::Adjective);
        assert_eq!(used("完璧な計画", "完璧").0, Part::Adjective);
        // Before a particle, a noun
        assert_eq!(used("完璧と言える", "完璧").0, Part::Noun);
    }

    #[test]
    fn grammar_is_found_as_grammar() {
        // のだ's ん is the particle の, not ん "yes"
        let (part, gloss) = used("俺が何をしたっていうんだ", "ん");
        assert_eq!(part, Part::Grammar);
        assert!(!gloss.starts_with("yes"), "{gloss}");
        for (text, surface) in [("三時間ほど前", "ほど"), ("コンビニぐらいはある", "ぐらい")] {
            let (part, gloss) = used(text, surface);
            assert_eq!(part, Part::Grammar, "{surface}");
            assert!(!gloss.is_empty(), "{surface}: a particle's sense");
        }
        // JMdict has ずつ only as a suffix: grammar all the same, with no sense marked
        assert_eq!(used("少しずつ進む", "ずつ"), (Part::Grammar, String::new()));
    }

    #[test]
    fn a_counter_is_a_counter_in_its_counter_sense() {
        assert_eq!(used("伊達に十年以上引きこもっていた", "十年").0, Part::Counter);
        let (part, gloss) = used("５人の兄弟", "５人");
        assert_eq!(part, Part::Counter);
        assert!(gloss.starts_with("counter"), "{gloss}");
    }

    #[test]
    fn a_suffix_on_its_own_is_what_it_can_be_alone() {
        // さん is only a suffix in JMdict: a noun; 族 is a noun used as a suffix
        assert_eq!(used("メイドさんを一人雇っている", "さん").0, Part::Noun);
        assert_eq!(used("古代長耳族が創りだした", "族").0, Part::Noun);
    }

    #[test]
    fn a_name_is_a_name_unless_it_is_a_common_word() {
        // UniDic takes 伊達 for a name here, but JMdict has the common word だて
        assert_ne!(used("伊達に十年以上引きこもっていた", "伊達").0, Part::ProperNoun);
        assert_eq!(used("エリスって呼ぶことを許してあげる", "エリス").0, Part::ProperNoun);
        assert_eq!(used("田中さんは東京の本屋で地図を買った", "田中").0, Part::ProperNoun);
    }

    #[test]
    fn adverbs_and_tie_breaks() {
        // 絶対 before a verb: an adverb; 自分 is a pronoun in JMdict; 食べる stays a verb
        assert_eq!(used("絶対負けない", "絶対").0, Part::Adverb);
        assert_eq!(used("自分より上の人間", "自分").0, Part::Pronoun);
        assert_eq!(used("ご飯を食べる", "食べる").0, Part::Verb);
        // An adverb before に or と stays one; a noun before punctuation stays one (時's "occasion" is an adverb too)
        assert_eq!(used("眼を放すとすぐにどこかにいっちゃうの", "すぐ").0, Part::Adverb);
        assert_eq!(used("並んでいた時、いきなり横入りしてきた", "時").0, Part::Noun);
        // A sentence-final particle or と does not make a な-adjective a noun
        assert_eq!(used("これで大丈夫よ", "大丈夫").0, Part::Adjective);
        assert_eq!(used("意外とバカなのかもしれない", "意外").0, Part::Adjective);
    }
}
