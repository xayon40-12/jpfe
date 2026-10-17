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
    pub chapter_frequency: f64,
    pub chapter_percent: String,
    pub cumulative_percent: String,
    pub book_occurrences: usize,
    pub book_frequency: f64,
    pub book_percent: String,
    pub first_appearance_chapter: String,
}
