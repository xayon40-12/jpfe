#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Chapter {
    pub index: usize,
    pub title: String,
    pub text: String,
}

pub struct WordEntry {
    pub rank: usize,
    pub word: String,
    pub pronunciation: String,
    pub pos: String,
    pub occurrences: usize,
    pub chapter_percent: String,
    pub cumulative_percent: String,
    pub book_occurrences: usize,
    pub book_percent: String,
    pub first_appearance_chapter: String,
}

pub fn is_prologue_title(title: &str) -> bool {
    let trimmed = title.trim().trim_matches(&['【', '】', '「', '」', '『', '』', '[', ']', '(', ')', '　'][..]);
    let lower = trimmed.to_lowercase();
    ["プロローグ", "序章", "まえがき", "前書き", "prologue", "intro"].iter().any(|p| lower.starts_with(p)) || lower == "序"
}

/// Number chapters from 0 when the book opens with a prologue, otherwise from 1.
pub fn adjust_chapter_indices(chapters: &mut [Chapter]) {
    let offset = usize::from(!chapters.first().is_some_and(|c| is_prologue_title(&c.title)));
    for (i, ch) in chapters.iter_mut().enumerate() {
        ch.index = i + offset;
    }
}

/// Filtered vocabulary words per character.
pub fn vocabulary_ratio(filtered_words: usize, characters: usize) -> f64 {
    if characters == 0 {
        0.0
    } else {
        filtered_words as f64 / characters as f64
    }
}

#[derive(Debug, Clone)]
pub struct ChapterStat {
    pub chapter_index: usize,
    pub title: String,
    pub filtered_words: usize,
    pub characters: usize,
    pub raw_ratio: f64,
    pub normalized_ratio: f64,
}

impl ChapterStat {
    /// `baseline_ratio` is the raw ratio of the reference chapter (prologue, or chapter 1).
    pub fn new(chapter_index: usize, title: &str, filtered_words: usize, characters: usize, baseline_ratio: f64) -> Self {
        let raw_ratio = vocabulary_ratio(filtered_words, characters);
        let normalized_ratio = if baseline_ratio > 0.0 { raw_ratio / baseline_ratio } else { 0.0 };
        Self {
            chapter_index,
            title: title.to_string(),
            filtered_words,
            characters,
            raw_ratio,
            normalized_ratio,
        }
    }
}

/// File name stem of a chapter's exported files, e.g. `chapter_003_第三話「魔術教本」`.
pub fn chapter_file_stem(chapter_index: usize, title: &str, pad_width: usize) -> String {
    format!("chapter_{:0pad_width$}_{}", chapter_index, sanitize_filename(title))
}

fn sanitize_filename(name: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '：' | ' ' | '\t' | '　' => '_',
            _ => c,
        })
        .collect();
    // Collapse runs of underscores and trim them from both ends
    let collapsed = mapped.split('_').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("_");
    if collapsed.is_empty() { "chapter".to_string() } else { collapsed }
}
