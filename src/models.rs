use serde::Serialize;

#[derive(Debug, Clone)]
pub struct Chapter {
    pub index: usize,
    pub title: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
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
    let trimmed = title.trim().trim_matches(|c: char| {
        c == '【' || c == '】' || c == '「' || c == '」' || c == '『' || c == '』'
            || c == '[' || c == ']' || c == '(' || c == ')' || c == '　'
    });
    let lower = trimmed.to_lowercase();
    lower.starts_with("プロローグ")
        || lower.starts_with("序章")
        || lower == "序"
        || lower.starts_with("まえがき")
        || lower.starts_with("前書き")
        || lower.starts_with("prologue")
        || lower.starts_with("intro")
}

pub fn adjust_chapter_indices(chapters: &mut [Chapter]) {
    if chapters.is_empty() {
        return;
    }
    let has_prologue = is_prologue_title(&chapters[0].title);
    for (i, ch) in chapters.iter_mut().enumerate() {
        if has_prologue {
            ch.index = i; // 0, 1, 2, ...
        } else {
            ch.index = i + 1; // 1, 2, 3, ...
        }
    }
}
