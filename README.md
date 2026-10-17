# Japanese Novel Word Frequency Extractor (`frequency_extraction`)

A high-performance Rust CLI tool designed for Japanese language learners and NLP workflows to extract per-chapter word frequencies from **EPUB** and **PDF** books and light novels.

---

## Features

- **Accurate Lemmatization (辞書形 / 原形)**: Converts all inflected verbs, adjectives, and auxiliaries back to their base elementary dictionary forms (e.g., `追い出された` → `追い出す`, `教わった` → `教わる`, `死んでしまった` → `死ぬ`, `美しかった` → `美味しい`).
- **True Root Verb Extraction**: Accurately maps potential forms (可能動詞 / 可能形 like `吸える` → `吸う`, `起こせる` → `起こす`, `行ける` → `行く`, `話せる` → `話す`, `読める` → `読む`, `見れる` → `見る`), passive forms, and causative forms back to their elementary root dictionary verbs, avoiding redundant headwords.
- **Modern Comprehensive Dictionary (UniDic by default)**: Uses NINJAL's **UniDic** by default for modern lexical analysis, accurately separating lemmas (*goiso*) from inflected word forms, while also supporting **IPADIC** or custom external dictionaries.
- **Both EPUB and PDF Support**:
  - **EPUB**: Automatically parses container, manifest, spine, and TOC; safely strips `<rt>` and `<rp>` ruby tags so furigana annotations do not duplicate or pollute word tokens.
  - **PDF**: Multi-threaded page extraction using `rayon` and `lopdf` with support for Japanese PDF font encodings (including 2-byte `UniJIS-UTF16-H` UTF-16BE streams) and automatic chapter boundary detection.
- **Punctuation & Symbol Filtering**: Non-word sequences, ellipses, and punctuation (e.g., `「――――`, `　………`, `―――`, `！　`, `　『`, `ーーー`) are strictly excluded from token counts and output files.
- **Whole-Novel Min Count (`-m`, default: 3)**: Filters out hapax legomena and words that occur less than 3 times across the entire novel, focusing your study effort on recurring vocabulary.
- **Target Vocabulary Coverage (Default: 99.0%)**: Calculates running cumulative frequency percentages and filters down to any chosen threshold (e.g., 99%, 95%, 90%) to prioritize high-yield words.
- **Hiding Previously Seen Words (Default: Active)**: Automatically hides words that already appeared in earlier chapters while preserving the word's true chapter rank (creating natural rank "holes"). This allows learners to focus strictly on new vocabulary introduced in each chapter.
- **Two Parallel Output Formats (`full/` and `words/`)**:
  - `full/`: Comprehensive TSV/CSV tables with full statistics for analysis.
  - `words/`: Single-column `.txt` files containing only the target words (one per line) ready for instant import into Anki, yomitan, or flashcard apps.
- **Blazing Fast**: Scans, tokenizes, and lemmatizes millions of words across hundreds of chapters in ~1 second using Rust.

---

## Output Structure

By default, files are exported to a folder adjacent to the input file named `<filename>_frequency/`:

```text
<input_file>_frequency/
├── full/
│   ├── chapter_01_prologue.tsv
│   ├── chapter_02_chapter_one.tsv
│   └── chapter_03_chapter_two.tsv
└── words/
    ├── chapter_01_prologue.txt
    ├── chapter_02_chapter_one.txt
    └── chapter_03_chapter_two.txt
```

### 1. `words/` Directory
Contains a clean, single-column text file per chapter with one dictionary-form word per line:
```text
母親
よ
父親
方
これ
剣
大丈夫
```

### 2. `full/` Directory
Contains tab-separated (`.tsv`) or comma-separated (`.csv`) files with the following columns:

| Column | Description | Example |
| :--- | :--- | :--- |
| `rank` | Frequency rank in this chapter (holes appear when previous words are hidden) | `20` |
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
| `first_appearance_chapter`* | Chapter where word is first encountered (*included when showing all words) | `第一話` |

---

## Build

Compile the release binary with Cargo:

```bash
cargo build --release
```

The compiled binary will be located at `./target/release/frequency_extraction`.

---

## Usage Examples

### 1. Default Run on an EPUB (First 3 Chapters)
Exports the first 3 chapters to `<path>/book.epub_frequency/` with default 99% coverage, minimum whole-book count of 3, and previously seen words hidden:
```bash
./target/release/frequency_extraction path/to/book.epub -n 3
```

### 2. Default Run on a PDF
```bash
./target/release/frequency_extraction path/to/book.pdf -n 3
```

### 3. Custom Output Directory
Specify an explicit output path using `-o`:
```bash
./target/release/frequency_extraction book.epub -n 3 -o my_vocab_output
```

### 4. Show All Words (Including Previously Seen Words)
Disables hiding previously seen words, outputting all vocabulary for each chapter along with the `first_appearance_chapter` column:
```bash
./target/release/frequency_extraction book.epub -n 3 --show-previous-words
```

### 5. Process Specific Chapters (e.g., Chapters 1 through 5)
```bash
./target/release/frequency_extraction book.epub -c 1-5
```

### 6. Adjust Whole-Novel Minimum Count Threshold
To include words appearing at least 5 times across the whole novel (or set to 1 to include all words):
```bash
./target/release/frequency_extraction book.epub -n 3 -m 5
```

### 7. Custom Target Percentage (e.g., Top 95%)
```bash
./target/release/frequency_extraction book.epub -n 3 -p 95.0
```

### 8. Extract Content Words Only (Nouns, Verbs, Adjectives, Adverbs)
Excludes grammatical particles (`助詞`) and auxiliary verbs (`助動詞`):
```bash
./target/release/frequency_extraction book.epub -n 3 --content-only
```

### 9. Export in CSV Format with Katakana Readings
```bash
./target/release/frequency_extraction book.epub -n 3 --reading katakana --format csv
```

---

## Sorting Order

Within each chapter, vocabulary is sorted using a multi-level priority:
1. **Primary sort**: Number of occurrences in this chapter (descending).
2. **Secondary sort**: Number of occurrences in the whole book (descending) for words with equal chapter counts.
3. **Tertiary tie-breaker**: Alphabetical ordering.

---

## CLI Options Reference

| Flag / Option | Short | Default | Description |
| :--- | :--- | :--- | :--- |
| `<INPUT_FILE>` | | *(required)* | Path to the input `.epub` or `.pdf` file |
| `--output-dir` | `-o` | `<input>_frequency` | Directory where per-chapter `full/` and `words/` files are saved |
| `--min-book-count` | `-m` | `3` | Minimum occurrences across entire novel (alias: `--min-count`) |
| `--target-percentage` | `-p` | `99.0` | Target cumulative frequency threshold (0.0 - 100.0) |
| `--show-previous-words` | | `false` | Show all words including those seen earlier (aliases: `--show-seen`, `--show-all`) |
| `--show-first-appearance` | | `false` | Force including `first_appearance_chapter` column when hiding previous words |
| `--num-chapters` | `-n` | *(all)* | Number of chapters to export (e.g. `3`) |
| `--chapters` | `-c` | *(all)* | Specific chapters to export (e.g. `1,2,3` or `1-4`) |
| `--format` | | `tsv` | Full stats table format: `tsv` or `csv` |
| `--reading` | | `hiragana` | Reading pronunciation format: `hiragana` or `katakana` |
| `--content-only` | | `false` | If set, excludes particles (`助詞`) and auxiliary verbs (`助動詞`) |
| `--dict` | | `unidic` | Morphological dictionary to use: `unidic` or `ipadic` |
| `--dict-path` | | `None` | Path to optional external MeCab/Lindera dictionary directory |
| `--scan-scope` | | `full` | `full` (scans whole novel for global counts) or `selected` |
