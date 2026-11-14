# `jpfe`: Japanese Novel Word Frequency Extractor

A fast, precision Rust CLI tool designed for Japanese language learners and NLP workflows to extract per-chapter vocabulary frequencies from **EPUB** and **PDF** books and light novels.

---

## Features

- **Accurate Lemmatization (辞書形 / 原形)**: Converts all inflected verbs, adjectives, and auxiliaries back to their base elementary dictionary forms (e.g., `追い出された` → `追い出す`, `教わった` → `教わる`, `死んでしまった` → `死ぬ`, `美しかった` → `美味しい`).
- **True Root Verb Extraction**: Accurately maps potential forms (可能動詞 / 可能形 like `吸える` → `吸う`, `起こせる` → `起こす`, `行ける` → `行く`, `話せる` → `話す`, `読める` → `読む`, `見れる` → `見る`), passive forms, and causative forms back to their elementary root dictionary verbs, avoiding redundant headwords.
- **Modern Comprehensive Dictionary (UniDic)**: Powered by NINJAL's modern **UniDic** dictionary, providing accurate lexical analysis with strict separation of lemmas (*goiso*) from conjugated word forms, with optional external dictionary support via `--dict-path`.
- **Words as a Dictionary Has Them (JMdict)**: UniDic splits text into short units, so idioms and compounds such as `神出鬼没`, `冒険者`, `お嬢様` or `魔法使い` would come out as pieces (`神出` + `鬼没`). Consecutive tokens that form a [JMdict](https://www.edrdg.org/wiki/index.php/JMdict-EDICT_Dictionary_Project) headword, the dictionary behind 10ten and most Yomitan setups, are merged into that word. Only word building is merged: phrases (`頭を下げる`), inflections (`知らない`), productive endings (`歩き始める`, `大きくなる`) and numbers with counters (`三人`) stay apart. A word the text writes in kana and in kanji (`できる` and `出来る`, `こと` and `事`, `ほんと` and `本当`) is one word, listed in the spelling the book uses most (the viewer still matches it across books that spell it differently), when JMdict lists the kana as one of the word's readings: a kana form JMdict has as a word of its own stays one (`いい`, `すっごい`, `やっぱ`), and another form of a verb (a potential form such as `かえれる`) counts as the verb; words written in kana whose kanji spelling JMdict marks as rarely used are always kana (`やる`, not `遣る`). A verb or adjective whose base form JMdict does not have is mapped to the word a dictionary has, by general rules: the base form as written (`上手く` → `上手い`, where UniDic's lemma is `旨い`), an expression only used in the negative (`計り知れない`, `思いがけない`), the verb a causative, passive or potential form comes from (`思わす` → `思う`, `魅入られる` → `魅入る`, `窺い知れる` → `窺い知る`), the godan verb of an -eru verb (`読み進める` → `読み進む`), or else the two verbs of a compound verb (`握り直す` → `握る` + `直す`).
- **Both EPUB and PDF Support**:
  - **EPUB**: Automatically parses container, manifest, spine, and TOC; safely strips `<rt>` and `<rp>` ruby tags so furigana annotations do not duplicate or pollute word tokens.
  - **PDF**: Multi-threaded page extraction using `rayon` and `lopdf` with support for Japanese PDF font encodings (including 2-byte `UniJIS-UTF16-H` UTF-16BE streams) and automatic chapter boundary detection. Headings are found anywhere on a page by their font (a Japanese font other than the body text's, at the same size or larger), falling back to names such as `プロローグ` or `第N話` when the PDF uses a single font. An episode's `（前書き）` / `（後書き）` sections stay in that episode, and the cover and `注意事項` pages are skipped. Furigana (text at 60% of the body size or smaller) is removed, like `<rt>` in EPUBs. Lines are joined back into paragraphs (a line cut at the full line width continues on the next one unless that one is indented, and furigana no longer cuts a line in two), so words such as `興味` are not split at line or page breaks; page numbers stay on lines of their own.
- **Punctuation & Symbol Filtering**: Non-word sequences, ellipses, and punctuation (e.g., `「――――`, `　………`, `―――`, `！　`, `　『`, `ーーー`) are strictly excluded from token counts and output files.
- **Visually Aligned Columns**: Automatically pads columns taking into account East Asian display widths (Kanji/Kana = width 2, Latin/digits = width 1), ensuring all columns align vertically in terminal pagers and text editors.
- **Clean Percentages**: Redundant decimal frequencies are omitted, showing clean, readable percentage metrics (`chapter_percent`, `cumulative_percent`, and `book_percent`).
- **Whole-Novel Min Count (`-m`, default: 3)**: Filters out hapax legomena and words that occur less than 3 times across the entire novel, focusing your study effort on recurring vocabulary.
- **Target Vocabulary Coverage (Default: 99.0%)**: Calculates running cumulative frequency percentages and filters down to any chosen threshold (e.g., 99%, 95%, 90%) to prioritize high-yield words.
- **Hiding Previously Seen Words (Default: Active)**: Automatically hides words that already appeared in earlier chapters while preserving the word's true chapter rank (creating natural rank "holes"). This allows learners to focus strictly on new vocabulary introduced in each chapter.
- **Two Parallel Output Formats (`full/` and `words/`)**:
  - `full/`: Comprehensive, visually aligned TSV/CSV tables with full statistics for analysis.
  - `words/`: Single-column `.txt` files containing only the target words (one per line) ready for instant import into Anki, yomitan, or flashcard apps.
- **No Spurious Grammar or Contractions**: By default, particles (`助詞`) and auxiliaries (`助動詞`) are left out, and with them the colloquial contractions UniDic analyzes as auxiliaries (`てる`, which flashcard dictionaries would look up as `照る`). Prefixes, suffixes and counters (`不`, `的`, `冊`) are listed like other words. See [Which Words Are Listed](#which-words-are-listed).
- **Prologue as Chapter 0**: When a book begins with a prologue (`プロローグ`, `序章`, `序`, `まえがき`, etc.), it is automatically detected and indexed as **Chapter 0**. This aligns subsequent story chapters naturally (e.g. `第一話` becomes Chapter 1).
- **Dynamic Zero-Padded Filenames**: Filenames dynamically use leading zeros based on the total number of chapters in the book (e.g., `chapter_000`, `chapter_001`, ..., `chapter_285`), ensuring that operating system file managers and shell tools sort all chapters in exact numerical sequence.
- **Blazing Fast**: Scans, tokenizes, and lemmatizes millions of words across hundreds of chapters in ~1 second using Rust.

---

## Installation & Build

### Install globally with Cargo:
```bash
cargo install --path .
```
This installs the `jpfe` binary into your `~/.cargo/bin` directory, making it available system-wide.

### Or build locally:
```bash
cargo build --release
```
The compiled binary will be located at `./target/release/jpfe`.

Building downloads the dictionaries the first time: lindera fetches UniDic, and `build.rs` runs `scripts/fetch-jmdict.sh` for JMdict (it needs `curl`, `tar` and `python3`).

### Dictionaries and updating JMdict
- **UniDic** is pinned by the lindera version in `Cargo.lock`.
- **JMdict** is pinned in `jmdict.lock`: a release of [jmdict-simplified](https://github.com/scriptin/jmdict-simplified), which publishes JMdict and KANJIDIC2 as JSON every week and keeps the older releases, and the SHA-256 of both archives. `scripts/fetch-jmdict.sh` downloads that release, refuses it if a checksum differs, and extracts into `target/jmdict/` the headwords (`headwords.tsv`), and for the viewer's reader the definitions (`definitions.tsv`) and kanji (`kanji.tsv`). Nothing of the dictionary is committed.

To move to the latest JMdict release:
```bash
scripts/fetch-jmdict.sh --update   # pins the latest release in jmdict.lock and extracts it
cargo test --release               # sentences with known words must still come out right
```
Then compare a book's word lists before and after (run `jpfe` on it with both versions and `diff` the `words/` folders), and commit `jmdict.lock` once the changes look right. Pushing it to `main` rebuilds the web viewer with that release, and the viewer analyzes books again with it. The extraction stops if a release has far fewer headwords than expected.

JMdict and KANJIDIC2 are the property of the [Electronic Dictionary Research and Development Group](https://www.edrdg.org/) and are used under its [licence](https://www.edrdg.org/edrdg/licence.html) (CC BY-SA 4.0).

---

## Output Structure

By default, files are exported to a folder adjacent to the input file named `<filename>_frequency/`:

```text
<input_file>_frequency/
├── full/
│   ├── chapter_000_prologue.tsv
│   ├── chapter_001_chapter_one.tsv
│   └── chapter_002_chapter_two.tsv
├── words/
│   ├── chapter_000_prologue.txt
│   ├── chapter_001_chapter_one.txt
│   └── chapter_002_chapter_two.txt
└── statistics/
    └── vocabulary_ratio.tsv
```

### 1. `words/` Directory
Contains a clean, single-column text file per chapter with one dictionary-form word per line:
```text
母親
父親
方
これ
剣
大丈夫
```

### 2. `full/` Directory
Contains visually aligned tab-separated (`.tsv`) or comma-separated (`.csv`) files with the following columns:

| Column | Description | Example |
| :--- | :--- | :--- |
| `rank` | Frequency rank in this chapter (holes appear when previous words are hidden) | `21` |
| `word` | Dictionary / base form (原形) | `母親` |
| `pronunciation` | Canonical reading (Hiragana or Katakana) | `ははおや` |
| `pos` | Part of speech (品詞: 名詞, 動詞, 形容詞, 助詞, etc.) | `名詞` |
| `occurrences` | Number of occurrences in this chapter | `13` |
| `chapter_percent` | Relative frequency in this chapter (percentage) | `0.5677%` |
| `cumulative_percent` | Running cumulative coverage of this chapter | `50.61%` |
| `book_occurrences` | Total occurrences across the entire novel | `88` |
| `book_percent` | Relative frequency across the entire novel (percentage) | `0.0058%` |
| `first_appearance_chapter`* | Chapter where word is first encountered (*included when showing all words) | `第一話` |

### 3. `statistics/` Directory
Contains chapter-level vocabulary load and density statistics:
- **`vocabulary_ratio.tsv`** (or `.csv`): Clean column table tracking the ratio of filtered vocabulary words divided by character count for each chapter, normalized relative to the Prologue (or Chapter 1 if no prologue):
  - `chapter`: Chapter index
  - `title`: Chapter title
  - `filtered_words`: Number of vocabulary words exported to the chapter's `words/` file
  - `characters`: Total character count of the chapter
  - `raw_ratio`: Raw vocabulary load per character (`filtered_words / characters`)
  - `normalized_ratio`: Normalized ratio relative to Chapter 0 / baseline (`raw_ratio / baseline_ratio`, baseline = `1.000000`)

To see these statistics as charts, use the viewer below.

---

## Interactive Viewer (desktop and web)

`viewer/` is an [egui](https://github.com/emilk/egui) app that charts these statistics live for several books at once:

- **Left panel**: one entry per loaded EPUB/PDF (open them with the button or drop them on the window), each with a **Learned up to (included)** chapter selector. It starts at *Not started*, so loading a book does not mark anything as learned.
- **Central panel**: pick the book to show. Its word lists leave out every word you have learned in the *other* books (their word lists up to the selected chapter), and an orange vertical line marks your progress in the shown book. The plots are, from top to bottom: new words to learn, vocabulary density (new words per 100 words of the chapter: the share of its words that are new), words learned so far (every word to learn from the start of the book up to and including each chapter), and chapter length. They start in log scale; click a plot's "log scale" / "linear scale" label to switch it. Hovering a chapter shows its title and numbers in all the plots.
- **Download word lists (.zip)**: the shown book's filtered word lists, one `words/chapter_…txt` per chapter, as in the CLI output.
- **Reader** (📖 button at the top left; 📊 goes back): the selected book's text, full screen, opened at the chapter you learned up to (the first chapter before any), or where you left it if that has not changed. The text is set on a grid, one full-width character per cell (half-width letters and digits are shown full-width), vertically by default (縦書き: columns from right to left, with the mouse wheel moving on through them), or horizontally (**⚙ Settings**). Punctuation and closing brackets never start a line. PDF page numbers are shown small, after the paragraph where the page ends. Click or tap any word to see its reading and its JMdict definitions; the kanji of the word, shown larger at the top of that window, can be clicked for their on and kun readings, name readings, meanings, stroke count, school grade and frequency (from KANJIDIC2). Escape or a click beside the words closes the windows. Change the font size by pinching, with Shift + scroll wheel (or Cmd/Ctrl + scroll), or in **⚙ Settings**; it is remembered.

Each book entry also shows how many of its words are left once the words learned in your other books are removed. **⚙ Settings** has the reader's font size and changes how the word lists are built, with the same meaning and defaults as the CLI options: coverage target (`-p 99`), minimum occurrences in the book (`-m 3`), only words new to the book (opposite of `--show-previous-words`), leaving out katakana-only words (on by default; viewer only), and including particles and auxiliary verbs (`--include-particles`, which analyzes the books again). At the bottom, **Show debug information** (off by default) shows load times and storage errors in the sidebar. The look follows macOS colors and controls, in light or dark mode depending on the system setting.

Imported books, your progress in each, the settings and the plot scales are remembered: in the browser's private storage (OPFS) on the web, and in the user data folder (e.g. `~/Library/Application Support/jpfe-viewer`) on the desktop. Each book's analysis is cached too, and reused only by the same build of the viewer with the same dictionary; after an update, books are analyzed again from their stored file.

### Run it on the desktop
```bash
cargo run --release -p jpfe-viewer
```
The dictionary is embedded, as in the CLI. Japanese text uses a system font (Hiragino on macOS, Noto Sans CJK on Linux, Yu Gothic / Meiryo on Windows).

### Build the web version
Everything runs in the browser, so books are never uploaded anywhere. The page downloads the UniDic dictionary (about 35 MB, zstd-compressed), the JMdict headwords (2 MB), the reader's definitions (8 MB) and kanji (0.2 MB), and Noto Sans JP (4 MB), and keeps the compressed dictionaries in browser storage. On later visits it only fetches `assets/dict/version.txt` and downloads the dictionaries again when that version changed.
```bash
rustup target add wasm32-unknown-unknown   # once
./viewer/build-web-assets.sh               # dictionaries and font into viewer/assets/ (needs zstd, curl and python3)
cd viewer && trunk serve --release         # or `trunk build --release` for viewer/dist/
```
`.github/workflows/pages.yml` builds and publishes it when a push to `main` changes the code or `jmdict.lock` (or when started from the Actions tab). It caches the compiled dependencies and the dictionaries, which are only prepared again when `Cargo.lock`, `jmdict.lock` or the scripts change. Enable it once in the repository's **Settings → Pages → Source: GitHub Actions**. The published dictionaries include UniDic's BSD license (`assets/dict/UNIDIC-LICENSE.txt`) and JMdict's attribution (`assets/dict/JMDICT-LICENSE.txt`); Noto Sans JP is under the SIL Open Font License.

---

## Which Words Are Listed

The tokenizer reports every word it finds, with what kind of word it is; word lists then choose which ones to count. The viewer's reader shows every word the tokenizer found, so what it can look up is exactly what was found.

- **Left out of the text altogether**: symbols, punctuation and spaces, and tokens that are not words (numerals in digits, runs of punctuation such as `「――――` or `！　` that UniDic takes for nouns).
- **Conjugation endings are part of their word**: UniDic makes words of the auxiliaries and particles that conjugate a verb, adjective or na-adjective (`数えられる` is `数え` + `られる`, `あった` is `あっ` + `た`), and of the `ない` of an adjective (`美しくない`). The tokenizer puts them back into the word they follow, so it is counted once under its dictionary form (`数える`), and the reader looks it up wherever it is clicked and says what its conjugation means (`passive or potential (られる)`, `past (た)`). Verbs after `て` (`書いて` + `しまう`, `読まれて` + `いる`) and what follows a noun (`勉強` + `する`, `学生` + `だ`) stay words of their own.
- **Words written the same are counted apart** when JMdict has them as words of their own, with no meaning in common: `様` in `エミリア様` (さま) and in `ように` (よう), `中` (なか, ちゅう), `止める` (とめる, やめる), or `どれ` and `いずれ` (both `何れ`). UniDic's reading tells which one the text has. Readings of one word stay together even where JMdict gives them entries of their own (`私`: わたし, わたくし; `何`: なに, なん), and so do words whose meanings overlap (`方`: かた, ほう).
- **Particles and auxiliary verbs** (`助詞`, `助動詞`), endings included, are not listed unless `--include-particles` is given (the viewer's settings have the same option). UniDic analyzes colloquial contractions as auxiliaries (`読んでる` is `読む` + `でる`, `書いとく` is `書く` + `とく`, `しちゃう` is `する` + `ちゃう`), so they are left out with them, and dictionaries do not look up `てる` as `照る`.
- **Single hiragana** other than conjunctions are never listed: interjections (`あ`, `え`), and prefixes and suffixes such as `お` and `さ` (`高さ`).
- **Everything else is listed**, prefixes and suffixes included (`不`, `超`, `的`, `性`, `者`, `様`, and counters such as `冊` or `枚`). Words are counted under their dictionary form, so a verb written in kana is not mistaken for a contraction (`手にとって` is `取る`, `によって` is `因る`).

This happens before any counting, so occurrences, percentages, ranks and both the `full/` tables and the `words/` lists follow these rules.

## Usage Examples

### 1. Default Run on an EPUB (First 3 Chapters)
Exports the first 3 chapters to `<path>/book.epub_frequency/` with default 99% coverage, minimum whole-book count of 3, and previously seen words hidden:
```bash
jpfe path/to/book.epub -n 3
```

### 2. Default Run on a PDF
```bash
jpfe path/to/book.pdf -n 3
```

### 3. Custom Output Directory
Specify an explicit output path using `-o`:
```bash
jpfe book.epub -n 3 -o my_vocab_output
```

### 4. Show All Words (Including Previously Seen Words)
Disables hiding previously seen words, outputting all vocabulary for each chapter along with the `first_appearance_chapter` column:
```bash
jpfe book.epub -n 3 --show-previous-words
```

### 5. Process Specific Chapters (e.g., Chapters 0 through 5 or 1 through 5)
```bash
# If the book has a prologue, it is chapter 0:
jpfe book.epub -c 0-5
```

### 6. Adjust Whole-Novel Minimum Count Threshold
To include words appearing at least 5 times across the whole novel (or set to 1 to include all words):
```bash
jpfe book.epub -n 3 -m 5
```

### 7. Custom Target Percentage (e.g., Top 95%)
```bash
jpfe book.epub -n 3 -p 95.0
```

### 8. Include Grammatical Particles and Auxiliaries (Optional)
By default, particles and auxiliaries are excluded to keep flashcards clean. To include them:
```bash
jpfe book.epub -n 3 --include-particles
```

### 9. Export in CSV Format with Katakana Readings
```bash
jpfe book.epub -n 3 --reading katakana --format csv
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
| `--include-particles` | | `false` | Include grammatical particles (`助詞`) and auxiliary verbs (`助動詞`) (alias: `--include-grammar`) |
| `--num-chapters` | `-n` | *(all)* | Number of chapters to export (e.g. `3`) |
| `--chapters` | `-c` | *(all)* | Specific chapters to export (e.g. `1,2,3` or `1-4`) |
| `--format` | | `tsv` | Full stats table format: `tsv` or `csv` |
| `--reading` | | `hiragana` | Reading pronunciation format: `hiragana` or `katakana` |
| `--dict-path` | | `None` | Path to optional external MeCab/Lindera dictionary directory |
