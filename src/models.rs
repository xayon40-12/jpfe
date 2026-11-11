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
