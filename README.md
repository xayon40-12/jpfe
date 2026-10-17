# `jpfe`: Japanese Novel Word Frequency Extractor

A fast, precision Rust CLI tool designed for Japanese language learners and NLP workflows to extract per-chapter vocabulary frequencies from **EPUB** and **PDF** books and light novels.

---

## Features

- **Accurate Lemmatization (辞書形 / 原形)**: Converts all inflected verbs, adjectives, and auxiliaries back to their base elementary dictionary forms (e.g., `追い出された` → `追い出す`, `教わった` → `教わる`, `死んでしまった` → `死ぬ`, `美しかった` → `美味しい`).
- **True Root Verb Extraction**: Accurately maps potential forms (可能動詞 / 可能形 like `吸える` → `吸う`, `起こせる` → `起こす`, `行ける` → `行く`, `話せる` → `話す`, `読める` → `読む`, `見れる` → `見る`), passive forms, and causative forms back to their elementary root dictionary verbs, avoiding redundant headwords.
- **Modern Comprehensive Dictionary (UniDic)**: Powered by NINJAL's modern **UniDic** dictionary, providing accurate lexical analysis with strict separation of lemmas (*goiso*) from conjugated word forms, with optional external dictionary support via `--dict-path`.
- **Both EPUB and PDF Support**:
  - **EPUB**: Automatically parses container, manifest, spine, and TOC; safely strips `<rt>` and `<rp>` ruby tags so furigana annotations do not duplicate or pollute word tokens.
  - **PDF**: Multi-threaded page extraction using `rayon` and `lopdf` with support for Japanese PDF font encodings (including 2-byte `UniJIS-UTF16-H` UTF-16BE streams) and automatic chapter boundary detection.
- **Punctuation & Symbol Filtering**: Non-word sequences, ellipses, and punctuation (e.g., `「――――`, `　………`, `―――`, `！　`, `　『`, `ーーー`) are strictly excluded from token counts and output files.
- **Visually Aligned Columns**: Automatically pads columns taking into account East Asian display widths (Kanji/Kana = width 2, Latin/digits = width 1), ensuring all columns align vertically in terminal pagers and text editors.
- **Clean Percentages**: Redundant decimal frequencies are omitted, showing clean, readable percentage metrics (`chapter_percent`, `cumulative_percent`, and `book_percent`).
- **Whole-Novel Min Count (`-m`, default: 3)**: Filters out hapax legomena and words that occur less than 3 times across the entire novel, focusing your study effort on recurring vocabulary.
- **Target Vocabulary Coverage (Default: 99.0%)**: Calculates running cumulative frequency percentages and filters down to any chosen threshold (e.g., 99%, 95%, 90%) to prioritize high-yield words.
- **Hiding Previously Seen Words (Default: Active)**: Automatically hides words that already appeared in earlier chapters while preserving the word's true chapter rank (creating natural rank "holes"). This allows learners to focus strictly on new vocabulary introduced in each chapter.
- **Two Parallel Output Formats (`full/` and `words/`)**:
  - `full/`: Comprehensive, visually aligned TSV/CSV tables with full statistics for analysis.
  - `words/`: Single-column `.txt` files containing only the target words (one per line) ready for instant import into Anki, yomitan, or flashcard apps.
- **Clean Vocabulary Filtering (No Spurious Grammar / Contractions)**: By default, `jpfe` filters out non-content grammar particles (`助詞`), auxiliaries (`助動詞`), bound prefixes/suffixes (`接頭辞`, `接尾辞`), and 5 categories of colloquial contractions and noise fragments (e.g., `てる` which causes flashcard dictionaries to falsely look up `照る`). This filtering happens globally prior to frequency and rank calculation, keeping both `full/` tables and `words/` lists focused purely on genuine vocabulary.
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

---

## Output Structure

By default, files are exported to a folder adjacent to the input file named `<filename>_frequency/`:

```text
<input_file>_frequency/
├── full/
│   ├── chapter_000_prologue.tsv
│   ├── chapter_001_chapter_one.tsv
│   └── chapter_002_chapter_two.tsv
└── words/
    ├── chapter_000_prologue.txt
    ├── chapter_001_chapter_one.txt
    └── chapter_002_chapter_two.txt
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

---

## Vocabulary Filtering & Excluded Categories (The 5 Filtered Groups)

When preparing vocabulary for flashcard systems (Anki, Yomitan, 10ten, Migaku), automatic dictionary lookups match text strings directly against dictionary headwords. In raw Japanese tokenization, colloquial contractions, bound affixes, and single-kana grammatical particles often get extracted as standalone dictionary words, causing dictionaries to match rare, irrelevant kanji homophones (e.g., `食べてる` extracted as `てる`, which dictionaries look up as **照る** "to shine").

To prevent this, `jpfe` performs clean vocabulary filtering **globally prior to all occurrence and frequency calculations**. This ensures that token counts, percentages, ranks, and output files (`full/` tables and `words/` lists) are clean and accurate.

The following 5 categories are excluded by default:

### Category 1: Colloquial Verb Contractions (口語縮約形・補助動詞)
Contractions of compound verb forms (`〜て/で + 補助動詞`). The root verb (e.g. `食べる`, `折れる`, `死ぬ`, `言う`, `飛ぶ`, `待つ`) is properly preserved and lemmatized, while the contracted helper ending is excluded to prevent false homophone matches:

| Extracted Word | Source Phrase Example | Real Underlying Grammar | False Dictionary Lookup in Yomitan / Anki |
| :--- | :--- | :--- | :--- |
| **`てる`** | `肋骨が折れてる` / `食べてる` | `〜ている` (progressive) | **照る** (to shine / be sunny) |
| **`でる`** | `落ち込んでる` / `死んでる` | `〜でいる` (voiced progressive) | **出る** (to exit / leave / appear) |
| **`とく`** | `言っとく` / `しとく` | `〜ておく` (to do in advance) | **解く** (to solve/untie), **説く** (explain), **徳** |
| **`どく`** | `置いとく` / `読んどく` | `〜でおく` (voiced `ておく`) | **退く** (to step aside / retreat) |
| **`ちゃう`** | `いっちゃう` / `しちゃう` | `〜てしまう` (regret/completion) | **違う** (Kansai dialect: no / wrong) |
| **`ちまう`** | `早死にしちまう` / `やっちまう` | `〜てしまう` (colloquial `てしまう`) | *(Slang auxiliary)* |
| **`じゃう`** | `読んじゃった` / `死んじゃう` | `〜でしまう` (voiced `てしまう`) | *(Slang auxiliary)* |
| **`じまう`** | `死んじまった` | `〜でしまう` (voiced colloquial) | *(Slang auxiliary)* |
| **`でく`** | `イタイのイタイのとんでけ` | `〜でいく` (`飛んで行け`) | **木偶** (puppet / blockhead / dummy) |
| **`てく`** | `連れてく` / `置いてけない` | `〜ていく` (`連れて行く`) | *(Slang / obscure entry)* |
| **`とる`** | `待っとる` / `知っとる` | `〜ておる` (`待っている`) | **取る** (to take / grasp), **採る** |
| **`とれる`** | `待っとれよ` | `〜ておれる` (potential/imperative) | **取れる** (to come off / be removed) |
| **`とける`** | `置いとけない` | `〜ておける` (`置いておけない`) | **解ける** (to come undone) / **溶ける** (melt) |
| **`たげる`** | `もうやめたげてよお` | `〜てあげる` (`やめてあげて`) | *(Nonexistent / slang)* |
| **`てらっしゃる`** | `行ってらっしゃい` | `〜ていらっしゃる` (honorific) | *(Honorific auxiliary)* |
| **`てらしゃる`** | `何をしてらしたのか` | `〜ていらした` (colloquial honorific) | *(Honorific auxiliary)* |
| **`でらっしゃる`** | `急いでらっしゃいますの` | `〜でいらっしゃる` (honorific) | *(Honorific auxiliary)* |
| **`しめる`** | `知らしめてやる` | Verb `知る` + causative suffix `しめる` | **閉める** / **締める** / **占める** (to close / tighten) |
| **`よる`** | `集まってきよる` | Kansai/dialect aspect auxiliary | **寄る** (to approach) / **夜** (night) |
| **`はる`** | `してくれはった` | Kansai honorific auxiliary | **張る** (to stretch) / **春** (spring) |
| **`やがる`** | `動きしやがる` / `破壊しやがった` | Derogatory subsidiary verb | *(Derogatory auxiliary)* |
| **`たがる`** | `来たがらない` | Auxiliary expressing desire | *(Auxiliary)* |

*(Note: When full kanji verbs like `出る`, `照る`, `取る`, `解く`, `退く`, `閉める` appear as genuine main verbs in the text, their kanji lemmas are fully preserved).*

---

### Category 2: Colloquial Slang & Speech Particles (口語短縮・音変化)
Spoken dialogue contractions and slurred phonetic forms that get segmented as independent words:

| Extracted Word | Source Phrase Example | Real Underlying Grammar | False Dictionary Lookup in Yomitan / Anki |
| :--- | :--- | :--- | :--- |
| **`もん`** | `そんなもん` / `〜だもん` | Contraction of `もの` | **門** (gate) |
| **`っつう` / `つう`** | `悪ガキつったな` / `するってえと` | Slurred colloquial `と言う` | **通** (connoisseur / counter for letters) |
| **`っちゅう` / `ちゅう`** | `なんちゅう` / `構うっちゅーねん` | Slurred colloquial `と言う` | **中** (inside/middle) / **昼** |
| **`ちゃ`** | `しちゃいけない` | Contraction of `〜ては` | **茶** (tea) |
| **`じゃ`** | `とてもじゃないが` | Contraction of `では` (or archaic copula) | **蛇** (snake) |
| **`ん`** | `行くんだ` / `死んでるん` | Explanatory nominalizer `の` | **ん** (kana symbol / noise) |
| **`っす`** | `そうっすね` | Slang contraction of `です` | *(Slang)* |
| **`でちゅ`** | `パパでちゅよー` | Baby-talk inflection of `ですよ` | *(Baby talk)* |
| **`みてえ` / `みたく`** | `女みてえな` / `父様みたく` | Colloquial variation of `みたい` | *(Slang `みたい`)* |
| **`そりゃ` / `こりゃ` / `ありゃ`** | `そりゃそうだ` / `こりゃまずい` | `それ/これ/あれ + は` | *(Contraction)* |
| **`トコ` / `とこ`** | `ノトスん所` / `〜したとこ` | Colloquial shortening of `ところ` | **床** (floor / bed) |
| **`っけ`** | `なんて言うんでしたっけ` | Sentence-ending recall particle | *(Particle)* |
| **`っと`** | `おっと` / `〜っと` | Filler sound / quotation contraction | *(Filler particle)* |

---

### Category 3: Archaic & Classical Grammar Auxiliaries (文語助動詞・古典語尾)
Literary or archaic auxiliaries appearing in set phrases, whose modern base forms resolve awkwardly:

| Extracted Word | Source Phrase Example | Real Underlying Grammar | False Dictionary Lookup in Yomitan / Anki |
| :--- | :--- | :--- | :--- |
| **`まじ`** | `あるまじき行為` | Classical negative potential `まじ` | Modern slang **マジ** (for real / serious)! |
| **`む`** | `言わんばかり` (`言わ` + `ん`) | Classical auxiliary `む` (撥音便 `ん`) | Kana letter **む** |
| **`べし`** | `練習に励むべきだ` | Classical auxiliary `べし` (attributive `べき`) | **べし** (archaic auxiliary) |
| **`まい`** | `同じ失敗はすまい` | Negative volitional auxiliary | **まい** |
| **`ぬ` / `ず`** | `知らぬ` / `知らず` | Classical negative auxiliary | **図** (diagram, for ず) |
| **`り`** | Classical auxiliary `〜り` | Classical perfective auxiliary | **理** (logic) / **里** |
| **`ど`** | `違えども` (`違え` + `ど` + `も`) | Classical conjunctive particle `ど` | **度** / **土** |

---

### Category 4: Bound Suffixes & Prefix Fragments (接尾辞・接頭辞・語の分断)
Bound morphemes that cannot stand alone as words, as well as split kinship stems:

| Extracted Word | Source Phrase Example | Real Nature | False Dictionary Lookup in Yomitan / Anki |
| :--- | :--- | :--- | :--- |
| **`っぽい` / `ぽい`** | `高校生っぽい` | Suffix "-ish / -like" | *(Suffix)* |
| **`やすい`** | `教えやすい` | Verbal suffix "-easy to do" | **安い** (cheap) or **易い** (easy) |
| **`にくい`** | `聞き取りにくい` | Verbal suffix "-hard to do" | **難い** (difficult) or **憎い** (hateful) |
| **`づらい` / `がたい`** | `出しづらくって` / `捨てがたい` | Verbal suffixes "-hard to do" | *(Suffix)* |
| **`くさい`** | `面倒くさい` | Suffix "-smelling of / -like" | **臭い** (stinky / foul-smelling) |
| **`げ`** | `優しげな顔` | Suffix "-seeming / with an air of" | **毛** (hair) |
| **`だらけ`** | `泥だらけ` | Suffix "covered in / full of" | *(Suffix)* |
| **`っぱなし`** | `開きっぱなし` | Suffix "left open / as is" | *(Suffix)* |
| **`ぶり`** | `久しぶり` / `三年ぶり` | Suffix "after an interval of" | **鰤** (yellowtail fish) or **振り** |
| **`め`** | `やつめ` | Derogatory noun suffix | **目** (eye) |
| **`さ`** | `高さ` / `寒さ` | Nominalizing suffix (or sentence particle) | **差** (difference) |
| **`み`** | `深み` / `重み` | Nominalizing suffix | **実** (fruit) or **身** (body) |
| **`がる` / `ぶる`** | `嫌がる` / `正義感ぶる` | Verb-forming suffixes | *(Suffix)* |
| **`ら` / `たち`** | `兄弟たち` / `やつら` | Plural suffixes | **等** (for `ら`) |
| **`ちゃん` / `さん` / `さま`**| `メイドさん` / `おじいちゃん` | Name title suffixes | *(Suffix)* |
| **`お` / `ご`** | `お父さん` / `ごめんなさい` | Honorific prefixes | **尾** (tail) / **碁** (Go game) |
| **`じい`** | `おじいちゃん` (`お` + `じい` + `ちゃん`)| Kinship fragment from prefix/suffix split | **爺** (old man) |
| **`とう`** | `おとうさん` (`お` + `とう` + `さん`)| Kinship fragment | **塔** (tower), **党** (party), **父** |
| **`おじ` / `おば`** | `おじさん` / `おばさん` | Kinship fragments | **伯父** / **伯母** |
| **`ま`** | `あっという間に` / `いつのまにか` | Fragment of set phrase (`間` = interval) | **間** or **魔** (demon / magic) |
| **`い`** | `行くのかい` / `そうかい` | Spoken question particle `い` | **胃** (stomach) or **井** (well) |

---

### Category 5: Grammatical Particles & Auxiliaries (助詞・助動詞)
Grammatical particles (`助詞`) and auxiliary verbs (`助動詞`) are excluded by default. In single-column flashcard word lists, bare 1-character particles match rare kanji homophones instead of the intended grammatical function:
- `た` $\to$ **田** (rice field) or **他**
- `て` $\to$ **手** (hand)
- `に` $\to$ **荷** (cargo) or **二**
- `の` $\to$ **野** (field)
- `は` $\to$ **歯** (tooth), **葉** (leaf), **羽** (feather)
- `と` $\to$ **戸** (door), **都**
- `が` $\to$ **蛾** (moth)
- `で` $\to$ **出**
- `も` $\to$ **藻** (seaweed) or **喪**
- `か` $\to$ **蚊** (mosquito) or **科**
- `ば` $\to$ **場** (place) or **刃**
- `な` $\to$ **菜** (greens)
- `よ` $\to$ **世** or **夜**
- `ね` $\to$ **根** (root) or **値**
- `し` $\to$ **死** (death) or **市**
- `へ` $\to$ **屁** (fart)
- `や` $\to$ **矢** (arrow) or **屋**
- `わ` $\to$ **輪** (ring) or **和**

*(If you ever wish to include grammatical particles and auxiliaries, pass `--include-particles`).*

---

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
| `--content-only` | | `false` | Deprecated alias (content words only is now the default) |
| `--dict-path` | | `None` | Path to optional external MeCab/Lindera dictionary directory |
| `--scan-scope` | | `full` | `full` (scans whole novel for global counts) or `selected` |
