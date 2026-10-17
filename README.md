# Japanese Novel Word Frequency Extractor (`frequency_extraction`)

A high-performance Rust tool designed for Japanese language learners and natural language processing to extract word frequencies per chapter from **EPUB** and **PDF** books and light novels.

## Features

- **Accurate Lemmatization (辞書形 / 原形)**: Converts all inflected verbs, adjectives, and auxiliaries back to their dictionary forms (e.g., `追い出された` → `追い出す`, `教わった` → `教わる`, `死んでしまった` → `死ぬ`, `美しかった` → `美しい`).
- **Complete Fantasy Vocabulary**: Powered by **Lindera** and the **IPADIC** dictionary, supporting fantasy novel terminology, Katakana character names and terms, and compound nouns.
- **Both EPUB and PDF Support**:
  - **EPUB**: Automatically parses container, manifest, spine, and TOC; safely strips `<rt>` and `<rp>` ruby tags so furigana annotations do not duplicate or pollute word tokens.
  - **PDF**: Multi-threaded page extraction using `rayon` and `lopdf` with support for Japanese PDF font encodings (including 2-byte `UniJIS-UTF16-H` UTF-16BE streams) and automatic chapter boundary detection.
- **Punctuation & Symbol Filtering**: Non-word sequences (e.g., `「――――`, `　………`, `―――`, `！　`, `　『`) are strictly stripped from token counts and output files.
- **Blazing Fast**: Scans and lemmatizes millions of words across hundreds of chapters in about 1 second using Rust.
- **Target Vocabulary Coverage (Default 99%)**: Calculates running cumulative frequency percentages and filters down to any chosen percentage (e.g., 99%, 95%, 90%) to focus on high-yield vocabulary.
- **Hiding Previously Seen Words (Default)**: Automatically hides words that already appeared in earlier chapters while preserving the word's original chapter rank (creating natural rank "holes"). This allows learners to focus strictly on new vocabulary introduced in each chapter.
- **Whole-Book Context & First Appearance Tracking**: Reports each word's frequency in the current chapter and across the entire book, as well as the first chapter in which it appears (when showing all words).
- **Learner-Friendly Output**: Exports tab-separated (`.tsv`) or comma-separated (`.csv`) files per chapter with readings converted to clean Hiragana (or Katakana).

---

## Output Columns

Each generated chapter file contains the following columns:

| Column | Description | Example |
| :--- | :--- | :--- |
| `rank` | Frequency rank of the word in this chapter (holes appear when previous words are hidden) | `20` |
| `word` | Dictionary / base form (原形) | `母親` |
| `pronunciation` | Canonical reading (Hiragana or Katakana) | `ははおや` |
| `pos` | Part of speech (品詞: 名詞, 動詞, 形容詞, 助詞, etc.) | `名詞` |
| `occurrences` | Number of occurrences in this chapter | `13` |
| `chapter_frequency` | Relative frequency in this chapter (decimal) | `0.005783` |
| `chapter_percent` | Relative frequency in this chapter (percentage) | `0.5783%` |
| `cumulative_percent` | Running cumulative coverage of this chapter | `48.09%` |
| `book_occurrences` | Total occurrences across the entire novel | `87` |
| `book_frequency` | Relative frequency across the entire novel | `0.000058` |
| `book_percent` | Relative frequency across the entire novel (percentage) | `0.0058%` |
| `first_appearance_chapter`* | Chapter where this word is first encountered (*included when showing all words) | `第一話` |

---

## Build

Compile the release binary with Cargo:

```bash
cargo build --release
```

The compiled binary will be located at `./target/release/frequency_extraction`.

---

## Usage Examples

### 1. Default Run on an EPUB (First 3 Chapters, 99% Coverage, New Words Only)
Processes the first 3 chapters, filtering to 99% cumulative vocabulary coverage and omitting words seen in earlier chapters:
```bash
./target/release/frequency_extraction book.epub -n 3 -o output
```

### 2. Show All Words (Including Previously Seen Words)
Disables hiding of previous words, outputting all vocabulary for each chapter along with the `first_appearance_chapter` column:
```bash
./target/release/frequency_extraction book.epub -n 3 -o output_all --show-previous-words
```

### 3. Process Specific Chapters (e.g., Chapters 1 through 5)
```bash
./target/release/frequency_extraction book.epub -c 1-5 -o output
```

### 4. Custom Target Percentage (e.g., Top 95%)
```bash
./target/release/frequency_extraction book.epub -n 3 -o output -p 95.0
```

### 5. Process a PDF
```bash
./target/release/frequency_extraction book.pdf -n 3 -o output_pdf
```

### 6. Extract Content Words Only (Nouns, Verbs, Adjectives, Adverbs)
Excludes grammatical particles (`助詞`) and auxiliary verbs (`助動詞`):
```bash
./target/release/frequency_extraction book.epub -n 3 -o output_content --content-only
```

### 7. Export All Words (100% Coverage) with Katakana Readings in CSV Format
```bash
./target/release/frequency_extraction book.epub -n 3 -o output_csv -p 100.0 --reading katakana --format csv --show-previous-words
```

---

## Sorting Order

Within each chapter, vocabulary is sorted using a multi-level priority:
1. **Primary sort**: Number of occurrences in this chapter (descending).
2. **Secondary sort**: Number of occurrences in the whole book (descending) for words with the same chapter count.
3. **Tertiary tie-breaker**: Alphabetical ordering.

---

## CLI Options Reference

| Flag / Option | Short | Default | Description |
| :--- | :--- | :--- | :--- |
| `<INPUT_FILE>` | | *(required)* | Path to the input `.epub` or `.pdf` file |
| `--output-dir` | `-o` | `output` | Directory where per-chapter files are saved |
| `--target-percentage` | `-p` | `99.0` | Target cumulative frequency threshold (0.0 - 100.0) |
| `--show-previous-words` | | `false` | Show all words including those seen in earlier chapters (default hides them) |
| `--show-first-appearance` | | `false` | Force including `first_appearance_chapter` column when hiding previous words |
| `--num-chapters` | `-n` | *(all)* | Number of chapters to export (e.g. `3`) |
| `--chapters` | `-c` | *(all)* | Specific chapters to export (e.g. `1,2,3` or `1-4`) |
| `--format` | | `tsv` | Output format: `tsv` or `csv` |
| `--reading` | | `hiragana` | Reading pronunciation format: `hiragana` or `katakana` |
| `--content-only` | | `false` | If set, excludes particles (`助詞`) and auxiliary verbs (`助動詞`) |
| `--min-count` | | `1` | Minimum word occurrences in the chapter to include |
| `--dict-path` | | `None` | Path to optional external MeCab/Lindera dictionary directory |
| `--scan-scope` | | `full` | `full` (scans whole book for global statistics) or `selected` |
