//! Word definitions from JMdict and kanji from KANJIDIC2, as extracted by `scripts/fetch-jmdict.sh`
//! (see `scripts/jmdict-definitions.py` and `scripts/kanjidic.py` for the file formats).
//!
//! Like [`crate::tokenizer::Headwords`], the files are kept as they are and searched by binary search,
//! which takes far less memory than maps of their words (this matters in a browser on a phone).

use std::borrow::Cow;
use std::collections::HashMap;

use anyhow::{bail, Context, Result};

/// The definitions file, downloaded by the web viewer with the dictionary.
pub const DEFINITIONS_FILE: &str = "definitions.tsv";
/// The kanji file, downloaded by the web viewer with the dictionary.
pub const KANJI_FILE: &str = "kanji.tsv";

/// Start offsets of the lines of `text[start..end]`, which must end with a line break if not empty.
fn line_starts(text: &str, start: usize, end: usize) -> Result<Vec<u32>> {
    let mut starts = Vec::new();
    let mut at = start;
    while at < end {
        starts.push(u32::try_from(at).context("dictionary file too large")?);
        at += text[at..end].find('\n').map_or(end - at, |i| i + 1);
    }
    Ok(starts)
}

/// The line starting at `start`, without its line break.
fn line_at(text: &str, start: u32) -> &str {
    let rest = &text[start as usize..];
    &rest[..rest.find('\n').unwrap_or(rest.len())]
}

/// Index of the line whose first field is `key`, in lines sorted by it.
fn find_sorted(text: &str, lines: &[u32], key: &str) -> Option<usize> {
    lines
        .binary_search_by(|&start| {
            let line = line_at(text, start);
            line[..line.find('\t').unwrap_or(line.len())].cmp(key)
        })
        .ok()
}

/// The first line, which names the release: `# <name> <version>`.
fn version(text: &str, name: &str) -> Result<String> {
    let first = text.lines().next().unwrap_or_default();
    let version = first.strip_prefix(&format!("# {name} ")).with_context(|| format!("Not a {name} file"))?;
    Ok(version.to_string())
}

/// JMdict entries, found by any of their spellings or readings.
pub struct Definitions {
    text: Cow<'static, str>,
    /// Start of each entry's line, by entry number
    entries: Vec<u32>,
    /// Start of each index line, sorted by word
    index: Vec<u32>,
    /// Description of each part-of-speech or usage tag
    tags: HashMap<String, String>,
    pub version: String,
}

/// A dictionary entry: a word, its other spellings and readings, and its meanings.
#[derive(Debug, PartialEq)]
pub struct Entry<'a> {
    /// Kanji spellings, the most common first (none for words only written in kana)
    pub spellings: Vec<&'a str>,
    pub readings: Vec<&'a str>,
    pub senses: Vec<Sense<'a>>,
}

#[derive(Debug, PartialEq)]
pub struct Sense<'a> {
    /// Part of speech and usage codes (`n`, `v5r`, `uk`…): see [`Definitions::tag`]
    pub tags: Vec<&'a str>,
    /// English glosses, separated by "; "
    pub glosses: &'a str,
}

impl Definitions {
    pub fn parse(text: impl Into<Cow<'static, str>>) -> Result<Self> {
        let text = text.into();
        let version = version(&text, "JMdict")?;
        let mut lines = text.split_inclusive('\n').skip(1);
        let tag_line = lines.next().unwrap_or_default();
        let Some(tag_fields) = tag_line.trim_end().strip_prefix("#tags") else { bail!("JMdict definitions: no tags line") };
        let tags = tag_fields
            .split('\t')
            .filter_map(|t| t.split_once('='))
            .map(|(code, description)| (code.to_string(), description.to_string()))
            .collect();
        let entries_start = text.find('\n').map_or(text.len(), |i| i + 1) + tag_line.len();
        let marker = "\n#index\n";
        let Some(marker_at) = text[entries_start.saturating_sub(1)..].find(marker).map(|i| i + entries_start - 1) else {
            bail!("JMdict definitions: no index");
        };
        let entries = line_starts(&text, entries_start, marker_at + 1)?;
        let index = line_starts(&text, marker_at + marker.len(), text.len())?;
        Ok(Self { text, entries, index, tags, version })
    }

    /// The definitions extracted when building (needs the `embed-dict` feature).
    #[cfg(feature = "embed-dict")]
    pub fn embedded() -> Self {
        Self::parse(include_str!(env!("JPFE_JMDICT_DEFINITIONS"))).expect("the embedded JMdict definitions are valid")
    }

    /// Every entry spelled or read `word`, those where it is a common word first.
    pub fn lookup(&self, word: &str) -> Vec<Entry<'_>> {
        let Some(i) = find_sorted(&self.text, &self.index, word) else { return Vec::new() };
        let line = line_at(&self.text, self.index[i]);
        let numbers = line.split_once('\t').map_or("", |(_, n)| n);
        numbers
            .split(',')
            .filter_map(|n| self.entries.get(n.parse::<usize>().ok()?))
            .map(|&start| {
                let mut fields = line_at(&self.text, start).split('\t');
                let mut list = || fields.next().unwrap_or_default().split('・').filter(|s| !s.is_empty()).collect::<Vec<_>>();
                let (spellings, readings) = (list(), list());
                let senses = fields
                    .map(|sense| {
                        let (tags, glosses) = sense.split_once('|').unwrap_or(("", sense));
                        Sense { tags: tags.split(',').filter(|t| !t.is_empty()).collect(), glosses }
                    })
                    .collect();
                Entry { spellings, readings, senses }
            })
            .collect()
    }

    /// What a tag means ("noun", "Godan verb with 'ru' ending"…), or the tag itself if unknown.
    pub fn tag<'a>(&'a self, code: &'a str) -> &'a str {
        self.tags.get(code).map_or(code, String::as_str)
    }
}

/// KANJIDIC2 characters, found by kanji.
pub struct Kanjidic {
    text: Cow<'static, str>,
    /// Start of each line after the first, sorted by kanji
    lines: Vec<u32>,
    pub version: String,
}

/// Readings and meanings of a kanji.
#[derive(Debug, PartialEq)]
pub struct Kanji<'a> {
    pub literal: &'a str,
    /// On'yomi, in katakana
    pub on: Vec<&'a str>,
    /// Kun'yomi, in hiragana, with a dot before the okurigana (い.きる) and a dash for affixes
    pub kun: Vec<&'a str>,
    /// Readings only used in names
    pub nanori: Vec<&'a str>,
    pub meanings: Vec<&'a str>,
    pub strokes: Option<u32>,
    /// School grade (1–6, 8 for the rest of the jōyō kanji, 9–10 for jinmeiyō kanji)
    pub grade: Option<u32>,
    /// Level of the old JLPT (4 is the easiest, 1 the hardest)
    pub jlpt: Option<u32>,
    /// Rank among the 2,500 most used kanji in newspapers
    pub frequency: Option<u32>,
}

impl Kanjidic {
    pub fn parse(text: impl Into<Cow<'static, str>>) -> Result<Self> {
        let text = text.into();
        let version = version(&text, "KANJIDIC2")?;
        let start = text.find('\n').map_or(text.len(), |i| i + 1);
        let lines = line_starts(&text, start, text.len())?;
        Ok(Self { text, lines, version })
    }

    /// The kanji extracted when building (needs the `embed-dict` feature).
    #[cfg(feature = "embed-dict")]
    pub fn embedded() -> Self {
        Self::parse(include_str!(env!("JPFE_KANJIDIC"))).expect("the embedded KANJIDIC2 file is valid")
    }

    pub fn lookup(&self, kanji: char) -> Option<Kanji<'_>> {
        let i = find_sorted(&self.text, &self.lines, kanji.encode_utf8(&mut [0; 4]))?;
        let mut fields = line_at(&self.text, self.lines[i]).split('\t');
        let literal = fields.next()?;
        let mut list = |separator: &str| -> Vec<&str> {
            fields.next().unwrap_or_default().split(separator).filter(|s| !s.is_empty()).collect()
        };
        let (on, kun, nanori, meanings) = (list("、"), list("、"), list("、"), list("; "));
        let mut number = || fields.next().and_then(|f| f.parse().ok());
        Some(Kanji { literal, on, kun, nanori, meanings, strokes: number(), grade: number(), jlpt: number(), frequency: number() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFINITIONS: &str = "# JMdict 1.0\n#tags\tn=noun\tadj-na=adjectival nouns or quasi-adjectives\n\
        上手・上て\tじょうず\tadj-na,n|skillful; skilled\tn|flattery\n\
        上手\tうわて・かみて\tn|upper part\n\
        \tする\tvs-i|to do\n\
        #index\nうわて\t1\nかみて\t1\nじょうず\t0\nする\t2\n上て\t0\n上手\t0,1\n";

    #[test]
    fn finds_entries_by_spelling_or_reading() {
        let definitions = Definitions::parse(DEFINITIONS).unwrap();
        assert_eq!(definitions.version, "1.0");
        let entries = definitions.lookup("上手");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].spellings, ["上手", "上て"]);
        assert_eq!(entries[0].readings, ["じょうず"]);
        assert_eq!(entries[0].senses[0], Sense { tags: vec!["adj-na", "n"], glosses: "skillful; skilled" });
        assert_eq!(entries[1].readings, ["うわて", "かみて"]);
        assert_eq!(definitions.lookup("じょうず"), definitions.lookup("上て"));
        let suru = definitions.lookup("する");
        assert!(suru[0].spellings.is_empty());
        assert_eq!(suru[0].senses[0].glosses, "to do");
        assert!(definitions.lookup("下手").is_empty());
        assert_eq!(definitions.tag("n"), "noun");
        assert_eq!(definitions.tag("vs-i"), "vs-i");
    }

    #[test]
    fn finds_kanji() {
        let kanjidic = Kanjidic::parse(
            "# KANJIDIC2 1.0\n上\tジョウ、ショウ\tうえ、-うえ、あ.げる\t\tabove; up\t3\t1\t4\t35\n生\tセイ\tい.きる\t\tlife\t5\t\t\t\n",
        )
        .unwrap();
        let up = kanjidic.lookup('上').unwrap();
        assert_eq!(up.on, ["ジョウ", "ショウ"]);
        assert_eq!(up.kun, ["うえ", "-うえ", "あ.げる"]);
        assert!(up.nanori.is_empty());
        assert_eq!(up.meanings, ["above", "up"]);
        assert_eq!((up.strokes, up.grade, up.jlpt, up.frequency), (Some(3), Some(1), Some(4), Some(35)));
        assert_eq!(kanjidic.lookup('生').unwrap().grade, None);
        assert!(kanjidic.lookup('下').is_none());
    }

    #[test]
    fn rejects_other_files() {
        assert!(Definitions::parse("# JMdict 1.0\n上手\tじょうず\tn|skill\n").is_err());
        assert!(Kanjidic::parse("# JMdict 1.0\n").is_err());
    }
}

#[cfg(all(test, feature = "embed-dict"))]
mod embedded_tests {
    use super::*;

    #[test]
    fn embedded_files_have_words_and_kanji() {
        let started = std::time::Instant::now();
        let (definitions, kanjidic) = (Definitions::embedded(), Kanjidic::embedded());
        eprintln!("parsed in {:?}", started.elapsed());
        let entries = definitions.lookup("上手");
        assert!(entries.iter().any(|e| e.readings.contains(&"じょうず") && e.senses[0].glosses.contains("skil")));
        assert!(!definitions.lookup("じょうず").is_empty());
        assert!(!definitions.lookup("食べる").is_empty());
        let life = kanjidic.lookup('生').unwrap();
        assert!(life.on.contains(&"セイ"));
        assert!(life.meanings.contains(&"life"));
    }
}
