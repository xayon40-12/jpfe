# Extraction, statistics and the viewer: decisions and what remains

This document records how `jpfe` grew from a command-line word extractor into a CLI plus a viewer app that runs on the desktop and on the web. It covers the work of 11 to 14 November 2026: the statistics and their charts, the clean-up of the CLI, PDF chapter detection, the viewer and its web deployment on GitHub Pages, browser storage (Safari included), and the dictionary work that came before word grouping (compounds, then JMdict). Decisions are in the order they were taken, with their commits, the reasons given for them, and the approaches that were tried and dropped.

The reader and its dictionary window are covered in [decisions-reader.md](decisions-reader.md); word grouping (commit `ba86485` and later), numbers, and the viewer's storage fix (`abd6fcb`) in [word-grouping.md](word-grouping.md). How things work today is described in the [README](../README.md); this document links to it rather than repeating it.

Measurements come from the three books in `source/` that existed at the time: the 無職転生 EPUB (286 chapters), the same book as a PDF, and the 最果てのパラディン PDF.

## Starting point (October 2026)

The CLI was written on 2026-10-17 with another assistant (commits `45ed9f4` to `245f32c`):

- **`45ed9f4`**: EPUB (zip + roxmltree) and PDF (lopdf, pages read in parallel with rayon) readers, lindera with the IPADIC dictionary, per-chapter frequency tables in TSV or CSV.
- **`64e7c64`**: conjugated verbs were appearing as words, so UniDic became the default dictionary (IPADIC still selectable). It also added the two outputs: `full/` tables and `words/` lists ready to import into flashcards, the whole-book minimum count (`-m 3`) and the 99% coverage target.
- **`d5abfb9`**: IPADIC dropped, so only UniDic remains. Columns are padded by East Asian display width, and the decimal frequency columns were removed in favour of percentages.
- **`5df7abe`**: the binary is called `jpfe`.
- **`a16f55b`**: a leading prologue is chapter 0. File names are zero-padded to the number of chapters (`chapter_000` … `chapter_285`), so they sort correctly.
- **`245f32c`**: prefixes and suffixes are dropped, and so is a hand-written list of colloquial contractions, archaic auxiliaries and single kana (later removed by `ba86485`, see [word-grouping.md](word-grouping.md)).

At the start of this work, another assistant had written a statistics table and a hand-made SVG chart (about 975 lines in `src/statistics.rs`), not committed yet. It had overlapping plots, hover tooltips, a summary header, and four scale flags that could contradict each other (`--linear-ratio`, `--log-ratio <bool>`, `--log-chars`, `--linear-chars`).

## Statistics and the SVG chart

### 1. Stacked plots drawn with plotters (commit `689ee63`)

**Asked for:** a cleaner version that uses existing Rust libraries, separate plots instead of overlapping ones (characters at the bottom, with its axis on the left), and the number of words as a second plot in log scale.

**Decided:**

- The hand-written SVG was replaced by the `plotters` crate, without default features, so it needs no system font libraries. `statistics.rs` went from 975 to about 375 lines.
- Three stacked plots share the chapter axis: the normalized vocabulary ratio (log), the number of words exported per chapter (log, new), and the character count (linear).
- The four scale flags became three that cannot contradict each other: `--ratio-scale`, `--words-scale` and `--chars-scale`, each `log` or `linear`.
- The aligned TSV/CSV writer, which had been copied, became one `write_table` shared with the chapter tables.
- The baseline for the normalized ratio is always the first chapter. Both readers number chapters the same way, so the separate search for the prologue went away.

Problems met:

- **Missing labels:** plotters labels nothing on a log axis that spans less than a factor of 10 (seen on a 6-chapter run). The code gives it explicit 1-2-5 ticks, or 1 to 9 when the range is narrower.
- **Zero values:** chapters with 0 new words cannot be drawn on a log scale and are left off the log plots.

### 2. Hover tooltips and the summary header came back (commit `3745278`)

The user pointed out that the previous SVG was interactive. Rather than going back to hand-written SVG, the plotters output gets an overlay. Plotters reports where each point lands in pixels, so a full-height band per chapter lines up exactly with the plots, highlights the chapter's points in all three, and shows its title and numbers. Hovering anywhere in the band works, so with 286 chapters there is no need to aim at a tiny point. The header shows the chapter count and the min, average and max of each plot. The full-book SVG was about 230 KB.

## Clean-up of the CLI

### 3. A whole-code review, applied in full (commit `7a96b68`)

A review agent looked for code that a crate already handles, duplicated or dead code, and bugs. It applied every change to a scratch copy and checked that the output stayed byte-identical (tables, word lists, SVG, console log) for EPUB and PDF across many option combinations. The user chose "Apply all".

- **Size:** `src/` went from about 1,900 to 1,340 lines.
- **EPUB:** read with the `epub` crate instead of zip + roxmltree. It costs about 9 extra crates, but it fixes links with `../` or `%` escapes, which had been skipped silently.
- **Frequency counts:** the four frequency maps became one `BookWord` map. Tokenizing went from about 1.45 s to 1.23 s.
- **Removed:**
  - `src/lib.rs`, which nothing used and which made `cargo test` compile everything twice
  - the unused `serde` dependency
  - two options that did nothing: `--scan-scope` and `--content-only`
  - a duplicate `OutputFormat` enum
- **Word lists** are written in one buffered call instead of one system call per word.

**Crates considered and rejected:**

- **`wana_kana`** changes the readings: it turns ー into vowels (スーパー → すうぱあ) and converts romaji.
- **`sanitize-filename`** does not map spaces or `：`.
- **`comfy-table` and `tabled`** would change the console preview.
- **`num-format`** has had no release since 2022.
- **roxmltree for the XHTML text** fails on the undeclared `&nbsp;`.
- **lopdf's `extract_text`** decodes differently from the UTF-16BE path.

### 4. The review's bugs, fixed (commit `7a96b68`)

The user asked for these to be fixed. Each fix has a test, and neither book changed, since none of them hit these cases.

- **New words were tracked by chapter title.** Two chapters with the same title made their words count as new again. They are now tracked by chapter position.
- **The filter against a word made only of ー never triggered,** because Rust counts ー and 々 as letters. Now ー or 々 alone is rejected, while スーパー and 人々 are kept.
- **Numeric entities** (`&#8212;`, `&#x2014;`) are decoded in EPUB text, in one pass, so `&amp;lt;` becomes `&lt;` and not `<`.
- **UTF-16 PDF text was found by the font name `"F9"`,** which fitted only one PDF and also matched F90 or F19. Text is now read as UTF-16 when the font's declared encoding is UTF16 or UCS2.
- **Chapter selection:** `-c` together with `-n` is now an error (before, `-c` won silently), and so is a reversed range like `5-3` (before, it selected nothing).

## PDF chapters

### 5. Headings found by their font, anywhere on a page (commit `00d3eb3`)

A new PDF (最果てのパラディン) came out as a single chapter. Its episode titles are bare full-width numbers (`１`, `２`), and the old code only knew names like `第N話` or `プロローグ`, and only in the first 6 lines of a page. The user added that chapters can start in the middle of a page.

**Decided:**

- **Headings by font:** the body style is the font and size with the most characters. Headings are lines in another Japanese (UTF-16) font at the body size or larger. In both PDFs every episode title is drawn that way, so detection depends neither on wording nor on position.
- **Names as a fallback:** PDFs with a single font fall back to heading names on any line (now also 最終話, 間章, 幕間, 閑話, 番外, あとがき, full-width numbers). Phrases such as 閑話休題。 are skipped.
- **Forewords and afterwords:** an episode's （前書き） and （後書き） sections stay in the episode.
- **Front matter:** the cover and 注意事項 pages are skipped. The cover would otherwise have been chapter 1, and the first chapter was the baseline of the normalized ratio.

**Result:**

- 最果てのパラディン: 157 episodes.
- 無職転生 PDF: 286 chapters, the same as the EPUB (only full-width ｖｓ versus vs differs in 4 titles). Before, it had 351: 70 of them were tiny foreword and afterword fragments, and it had missed the last episode and four others.

### 6. Furigana dropped by size (commit `00d3eb3`)

PDF furigana ended up in the text as words of their own (katakana glosses over kanji, mostly). The user asked to remove it, like `<rt>` in EPUBs. Text at 60% of the body size or smaller is dropped (in these PDFs furigana is the body font at half size, 7 pt over 14 pt).

| | Words in the text | Distinct words | Removed from the lists |
|---|---|---|---|
| 最果てのパラディン | 204,322 → 202,604 | 16,731 → 16,339 | 129 |
| 無職転生 PDF | 777,063 → 776,108 | 23,406 → 23,303 | 59 |

## The viewer

### 7. One library, a CLI and an egui app (commit `8141eee`)

**Asked for:** a tool showing the same charts live for several books. Each book gets a chapter selector in a left panel ("learned up to, included"). The central panel shows one book, without the words learned in the other books, with a vertical line at the reader's chapter, and a button downloads the filtered word lists as a zip. The user suggested Rust with egui, also as a static web app on GitHub Pages.

**Decided:**

- **Crate layout:** the crate became a library shared by the CLI and `viewer/`. The CLI moved behind a default `cli` feature, the embedded dictionary behind `embed-dict`, and threads behind `parallel` (lopdf's rayon is off for the web). EPUB and PDF loaders read bytes, and the tokenizer can be built from dictionary files given as bytes. CLI output stayed byte-identical. `src/lib.rs`, removed a few hours earlier because nothing used it, came back as a real library.
- **Tokenizing in the background:** a few chapters at a time, within 25 ms per frame (`WORK_PER_FRAME`), because the web version has no threads and the window must stay responsive.
- **The zip** has one `words/chapter_….txt` per chapter, named as in the CLI's output.
- **The density plot** at first stayed relative to the shown book's first chapter, after filtering. When other books removed many of that chapter's words, the whole curve shifted up. Decision 24 removed this.

### 8. Progress starts at "Not started" (commit `8141eee`)

The first request said the selector should default to the first chapter. The user corrected it while the work was in progress: the convention is that the selected chapter is already learned, so a default of the first chapter would mark the first chapter of every loaded book as learned. The selector starts at **Not started**.

### 9. Current libraries and a macOS look (commit `8141eee`)

- **Versions:** the newest eframe and egui_plot needed Rust 1.95, and the local toolchain was 1.93, so the first plan was the newest pair that worked on 1.93 (egui 0.35). The user updated rustup and asked for up-to-date libraries: eframe 0.37 and egui_plot 0.38, which share egui 0.37. lopdf went from 0.42 to 0.47 for the CLI too (`get_page_content` no longer returns a `Result`).
- **Look:** the user asked for a look like a native Apple app, with the colours and shapes but not the new glass effect. The result has macOS system colours (blue accent, orange progress line), a grey sidebar, rounded cards with thin borders, and light or dark mode following the system.
- **Fonts:** on the desktop, system fonts (San Francisco and Hiragino on macOS, Noto Sans CJK on Linux). The web build downloads Noto Sans JP (about 4 MB).
- **Axis labels:** egui_plot's defaults were too dense or missing, so the app spaces grid lines itself (`nice_grid`, and `log_grid` on 1-2-5 steps). The number of chapter labels follows the width.

### 10. The web version downloads its dictionary (commit `8141eee`)

The compiled UniDic is about 200 MB (`dict.words` 106 MB, `matrix.mtx` 72 MB), too big to embed, and GitHub Pages rejects files over 100 MB. `viewer/build-web-assets.sh` compresses it with zstd -19 to about 35 MB, which the page downloads and unpacks (ruzstd) in the browser. It ships UniDic's BSD license with it. Everything runs locally, so books are never uploaded. The wasm file was 6.7 MB, and loading 無職転生 took 15 to 20 s in the browser at first.

### 11. New words first, scales switched by clicking (commit `d871592`)

After trying it, the user said the new-words plot matters most and should be on top, and density least, so it should go last ("the division of the 2 first"). Each plot's "log scale" / "linear scale" label switches its scale live. Order: new words, chapter length, density.

### 12. Settings and the words left after other books (commit `0fba28f`)

**Asked for:** a settings button, since only the defaults were used, and each book's count of words left once the other books' learned words are removed. The density title was also too long; the formula now shows when hovering "Vocabulary density".

**Decided:**

- **The settings** mirror the CLI options with the same defaults: coverage target, minimum count, only words new to the book, particles.
- **Recomputing:** books keep their per-chapter word counts (`ChapterCounts`, `analyze_counts` in the library), so lists are rebuilt without tokenizing again. Only the particles option tokenizes the books again. Changes apply when a slider is released, not while it is dragged.
- **Remaining words:** for example, with 無職転生 learned up to chapter 9, 最果てのパラディン showed 5,165 of its 7,646 words left.

### 13. Katakana-only words left out by default (commit `03bb6bd`)

The user asked for an option, on by default, to leave out words written only in katakana (mostly loanwords and names). Words that mix scripts, such as ドン引き, stay. The filter only removes words from the lists, so the 99% cut-off does not move. 最果てのパラディン goes from 7,646 to 7,228 words. The user checked that the zip follows it: the download is built from the same lists as the charts, so it always matches what is shown. It is **viewer only**: making it the CLI's default would have changed the CLI's output.

### 14. Charts react to hover only; scrolling; phones (commit `4132fef`)

The user asked for the plot controls to be turned off (no dragging inside a plot, no dragging axis ticks to zoom), and for both panels to scroll when the window is too short, "especially important to make it work on phones".

- **Plots:** they only sense hover. A drag that starts on a chart goes to the page, which is what lets a finger scroll on a phone.
- **Scrolling:** charts fill the window but stop shrinking at 170 px each, and the panel scrolls below that. The whole sidebar scrolls, its header included.
- **Narrow screens:** below 760 px wide, the books and the charts stack in one column, headers wrap, and the settings window fits the screen.

### 15. Whole cards and list rows are clickable (commit `a3c5baa`)

Only the text of a book card or of a list row took clicks. The user asked for:

- **Book cards:** the whole card selects the book, except the chapter selector, so that another book's progress can be changed without switching the shown book.
- **Lists:** rows without gaps between them, in both "Learned up to" and "Statistics for".

Rows now fill the list's width with no spacing. Labels are no longer selectable text, as in native macOS apps, because selectable labels took clicks meant for what was under them.

## Web deployment and browser storage

### 16. GitHub Actions, built incrementally (commit `9e6471c`)

`.github/workflows/pages.yml` (from `8141eee`) builds the site on GitHub. It uses a prebuilt trunk binary rather than compiling trunk, and Pages is set once to "Source: GitHub Actions". The user asked whether builds could be incremental:

- **Compiled dependencies:** `Swatinem/rust-cache` was already there. It deliberately leaves out the project's own crates, which would bloat the cache for little gain.
- **Dictionary and font:** `viewer/assets/` is now cached with `actions/cache`, keyed on `Cargo.lock` (the lindera version) and the assets script. On a hit, the native CLI build, the 140 MB UniDic download and the zstd compression are skipped.
- **Path filter:** a push deploys only when it touches the code, Cargo files or the workflow (and `jmdict.lock` and `scripts/` since `61d6f99`). A README-only commit no longer deploys, and a run can still be started by hand.

**Rejected: an orphan `gh-pages` branch built and pushed locally.** The user asked whether it would be faster. Measured runs took 75 to 140 s with caches, and 210 to 295 s when the dictionary was rebuilt. A pushed branch only saves the build, because Pages publishes a branch through its own Actions job, in the same queue. In exchange:

- every deploy commits the 6.7 MB wasm file (and 35 MB of dictionary when it changes);
- whatever sits in the local `dist/` gets published, uncommitted changes and test files included;
- deploying becomes a manual step.

The user chose "Keep Actions". The runs queued at that moment were stuck by a GitHub Pages outage; a branch would not have avoided it, since publishing a branch also goes through Pages. A run started by hand the next day deployed.

### 17. Books, progress and the dictionary are remembered (commit `6d0cb6c`)

**Asked for:** an OPFS cache. The dictionary should be downloaded again only when there is a new one; imported books and their statistics should stay; a page reload that fetches a new wasm build should recompute the statistics. While the work was in progress, the user added that a new dictionary changes the tokenizer, so it must invalidate the statistics too.

**Decided:**

- **Dictionary version:** CI publishes `assets/dict/version.txt`, a hash of the compressed files. On each load only that file is fetched. When it changes, the dictionary is downloaded again, with the version in its URLs so the HTTP cache cannot serve old files. Offline, the stored copy is used.
- **What is stored:** each book's original file, its analysis (chapters and per-chapter counts, encoded with postcard), and `state.json` (book list, progress, settings, scales). The state is never written before the saved one has been read, so it cannot be overwritten with an empty list. On the desktop, the same files go in the user data folder.
- **When an analysis is reused:** its cache key is the viewer build, the dictionary version and the particles setting. The build ID is a hash of the viewer's and the library's source and `Cargo.lock`, set by `viewer/build.rs`, so any code or dependency change triggers a new analysis. Progress is kept, and clamped if a book now has fewer chapters.
- **Writes:** the dictionary version is written only after all its files, so an interrupted download is never taken as complete.

## Safari and touch screens

### 18. Storage that Safari can handle (commit `5514f5c`)

The user reported that storage did not work in Safari. It took three rounds, each driven by what Safari showed:

1. **"Books cannot be saved":** the writes used `createWritable()`, which Safari only added in version 26. When it is missing or fails, writes now go through a small Web Worker using a sync access handle. Storage errors were then shown in the sidebar and logged to the console, so the next failure could be read.
2. **`UnknownError … (e.g. out of memory)` writing `dict/matrix.mtx` (71 MB):** all nine dictionary files (200 MB) were written at once, each in a single block, with an extra copy in the app's memory. Now every write and removal goes through one queue, in 4 MB chunks, copied out of wasm memory once. Imported books are saved first, then parsed, instead of being cloned.
3. **"This webpage was reloaded because it was using too much memory":** reading the 200 MB of unpacked dictionary back held it twice in memory. The cache now keeps the **compressed** files as downloaded (about 40 MB stored instead of 205 MB). They are read one at a time and unpacked on every load into buffers of exactly the right size (zstd frames give the size). Old unpacked files are deleted. This departs from the request to cache the decoded dictionary, and costs some unpacking on every load, but keeping it unpacked is what overloaded Safari.

The user asked to check online for known problems with OPFS and caching in Safari. What was found supported the design:

- **Memory:** Safari reloads pages whose WebAssembly memory grows too large; one report puts the reliable limit on iOS around 300 MB (WebKit bugs 269777 and 222097).
- **Concurrent access:** Safari does not support concurrent access to an OPFS file (bug 283959), hence the single queue.
- **Eviction:** under WebKit's storage policy, data can be evicted, and `navigator.storage.persist()` is granted by heuristics.

The user then confirmed that reloads and the long press (decision 19) worked on an iPhone 13 in Safari.

**Offered and left for later:**

- **Asking for persistent storage:** this was done later, in `abd6fcb` (see [word-grouping.md](word-grouping.md), decision 9).
- **A trimmed dictionary** keeping only the four UniDic fields in use, which would roughly halve memory. The user declined: "maybe not now, as we will probably use more of the dictionary in the future".

### 19. Long press to inspect a chart on touch screens (commit `5514f5c`)

**Asked for:** on a phone, a long press starts the hover mode, and dragging moves the hovered point until the finger is lifted. After that, dragging scrolls again, until the next long press.

**Decided:**

- **Starting:** a finger held still for 0.4 s (moving less than 8 points) on a chart starts inspecting. A mouse press never does.
- **The browser's own gestures:** its long-press menu, magnifier and text selection are turned off on the canvas (CSS in `viewer/index.html`).
- **Drawing the hover:** the first version detected the press but showed no hover, because egui never counts a widget as hovered while another one (here the scroll area) holds the pointer. The app now converts the finger's position to a chapter itself, and draws the line across all the charts and the tooltip above the finger. Mouse hover on the desktop is unchanged.
- **Tests:** unit tests feed egui touch events at chosen times. The in-app browser pane, while hidden, throttled frames and made the first touch tests inconclusive, so it was shown for the tests.

### 20. Cached books before the dictionary; load times behind a setting (commit `dd4a0ae`)

Reloads were slow on the iPhone, so the steps were measured. On the Mac, in Chrome, everything was ready 0.7 s after start:

- unpacking the dictionary took 0.5 s;
- reading the stored files took 0.6 s, mostly overlapping the unpacking;
- building the tokenizer took under 0.1 s;
- restoring the two books took 0.1 s.

The user measured about 2 s for the dictionary on an iPhone 13 (an M2 iPad Pro was about as fast as the Mac). The user then pointed out that the books waited for the dictionary, and that the opposite order would fix the problem. A cached analysis needs only the dictionary **version**, which is known within a moment, so cached books are now restored while the dictionary is still unpacking. On the Mac they show at 0.1 s instead of 0.7 s, and the commit counts about 2 s gained on the iPhone. Only books that must be analyzed again wait.

The timing line was first always shown in the sidebar. The user found it "clutter" and asked for a debug entry at the bottom of the settings, off by default. **Show debug information** shows the load times and raw storage errors, and is remembered. The short "Books cannot be saved" warning always shows.

**Not pursued:** gzip with the browser's native `DecompressionStream` (46 MB instead of 35), offered in case unpacking was still the bottleneck; changing the order of the restore was enough.

## Dictionaries: compounds and JMdict

### 21. Derived verbs, okurigana and a compound list from IPADIC (commit `ef0c758`)

The user saw compound verbs left in a causative form, and 神出 in a chapter's list where the book has 神出鬼没. An investigation agent found these causes:

- **Derived verbs:** UniDic lists some derived forms as verbs of their own (研ぎ澄ませる, 張り巡らせる, 魅入られる), and invents positive verbs for words used only in the negative (計り知れる, 言い知れる, 居た堪れる). A general rule such as せる → す was **rejected**: most of the 79 causative-looking UniDic verbs are real dictionary words (合わせる, 震わせる, 寝かせる). **Fix:** a `DERIVED_LEMMAS` table of nine verbs mapped to the dictionary word, with its reading (研ぎ澄ませる → 研ぎ澄ます, 計り知れる → 計り知れない).
- **Split compounds:** UniDic is a short-unit dictionary. It has 神出 and 鬼没 but not 神出鬼没, and only about 1,259 four-kanji entries. 無職転生 had 140 four-kanji idioms split this way. Suffix compounds lost their suffix, which was being dropped (親衛隊 counted as 親衛, 107 times). **Fix:** a lindera user dictionary, `src/compounds.csv`, of about 6,500 kanji compounds missing from UniDic, generated from IPADIC's nouns by `scripts/gen-compounds.py`. It was built in memory, so the web viewer got it too, and it lived under `src/`, so editing it changed the build ID.
- **Wrong okurigana:** `adapt_kanji` put the text's kanji onto the lemma's okurigana (抑さえる 139 times, 捉らえる, 囚らわれる). **Fix:** keep UniDic's base spelling when its kanji and pronunciation match, which leaves potential forms alone (治せる stays 治す).

Details decided with the user:

- **魔法使い is a word.** The agent had called it a false merge with the 3-kanji list. In fact IPADIC spells it 魔法使 (no い), which cut the text after the third kanji. Entries got their okurigana back (魔法使い, 人見知り, 不意打ち), and bare stems that cut 魔法使っ or 魔法使用 in the wrong place were dropped.
- **至近距離:** the new entry 近距離 left a stray 至. Raising the entry cost from 5000 to 6500 fixed it but split 一時期 (17 times) and some 小学生, so the cost stayed and 至近距離 was added by hand.
- **Accepted trade-off:** a word seen only inside a compound is no longer listed on its own (詩人 in 吟遊詩人).

List words went from 12,534 to 12,671 (無職転生) and from 7,646 to 7,681 (最果てのパラディン).

**Rejected:**

- **Switching to IPADIC, NEologd or Sudachi:** different part-of-speech names and fields, so every filter would be rewritten, and UniDic's lemma normalization would be lost.
- **lindera's `Mode::Decompose`:** it splits more, not less.
- **Merging neighbouring tokens across all parts of speech:** it merged ていた and してき.
- **Other UniDic fields (orthBase, lForm):** the derived verbs have the derived form in every field.

### 22. Looking for a dictionary that needs no tuning per book (discussion, 2026-11-12)

The user wanted most books to work without more fine tuning, and asked what dictionaries tools like 10ten use.

- **How 10ten works:** it uses JMdict. It never segments the whole text. At the cursor, it tries the longest string first, undoes conjugations, and keeps only real headwords.
- **Why not copy it:** a greedy longest match over a whole book glues particles onto the next word (はいい read as 拝 + いい). UniDic finds word boundaries better; it is weaker at deciding what counts as a word.
- **Options compared:**
  - Sudachi in mode C: long units, but a dictionary of about 200 MB against 35 MB, and every filter rewritten.
  - NEologd: abandoned since 2020.
  - UniDic for the splitting plus JMdict for the words: proposed.

**Prototype:** each run of up to 6 tokens is checked against JMdict's 207,000 kanji headwords, with conjugated endings matched in their dictionary form (気に入っ → 気に入る). A first unrestricted version merged phrases and inflections (知らない, 頭を下げる, その時, 三人), so a run is not merged when:

- it contains a particle;
- it ends in an auxiliary or a productive ending (〜始める, 〜すぎる, 〜なる, plurals);
- it starts with a pronoun, determiner, adverb or conjunction, or is a verb or adjective before a noun;
- it is a number with a counter (UniDic tags 度 and 人 as "possible counters", and 手 too, as in 二手);
- it is a noun plus ない where the noun is a word of its own (関係ない).

Results:

- **Precision:** in a sample of 240 new words, only a handful were questionable.
- **Lost:** a few numeric idioms the IPADIC list had kept (一対一, 一日中).
- **Hidden parts:** compounds with a productive suffix (授業中, 成功率) became one word.

The user decided to make it permanent "next time", with automatic download, and to discuss the compromises first, since "automatically updating a dictionary without checking if everything is working might be dangerous". Three options were laid out:

- a pinned version updated by a reviewed pull request;
- the latest JMdict at build time, guarded by checks;
- a scheduled job that opens the update pull request.

### 23. JMdict merging made permanent, the release pinned (commit `61d6f99`)

The user asked for the version to be pinned, so that only the version is committed, not the dictionary, and CI fetches and caches it.

- **Where releases come from:** EDRDG serves only the latest JMdict, so it cannot be pinned there. [jmdict-simplified](https://github.com/scriptin/jmdict-simplified) publishes numbered weekly releases and keeps the old ones.
- **What is committed:** `jmdict.lock`, the release and the SHA-256 of its archive.
- **The fetch script:** `scripts/fetch-jmdict.sh` downloads, verifies and extracts in under 3 s, refuses a wrong checksum (tested), and does nothing when the data is already extracted. `--update` pins the latest release. `scripts/jmdict-headwords.py` stops if a release has fewer than 150,000 headwords.
- **Builds:** a root `build.rs` runs the script for builds with the embedded dictionary, so a fresh `cargo build` works. The web viewer downloads `jmdict.tsv.zst` (1.6 MB) with UniDic. A new release changes the dictionary version, so books are analyzed again. The CI cache key and triggers include `jmdict.lock` and `scripts/`.
- **Removed:** the IPADIC list, its generator and its license file. Kept: the derived-verb table and the okurigana fix.
- **Rare kanji spellings:** a verb written in kana keeps its kana when JMdict marks the kanji spelling as rarely used (遣る → やる, 呉れる → くれる, 仕舞う → しまう). This was the "small follow-up" agreed on in decision 22.
- **Memory:** the 230,000 headwords stay as the sorted file and are searched by binary search, with about 1.8 MB of line offsets. A map would take tens of MB, which matters for Safari on a phone.
- **Speed:** the CLI went from 1.5 s to about 2.2 s on 無職転生, mostly because a chapter's tokens are now kept so neighbours can be merged. The first version took 2.6 s; profiling led to storing each line's start and end, and to running the cheap checks before the rare-spelling lookup.
- **Credits and settings:** JMdict's CC BY-SA credit is in the README, in `assets/dict/JMDICT-LICENSE.txt` and in the settings window, which now scrolls on short screens.
- **Results:** list words went from 12,670 to 13,424 (無職転生 EPUB) and from 7,681 to 7,707 (最果てのパラディン). 魔法陣, 冒険者 and お嬢様 became whole words, and fragments such as 具, 森林 and 存知 disappeared.

How to update the pin is in the README under [Dictionaries and updating JMdict](../README.md#dictionaries-and-updating-jmdict). Later sessions built word grouping on top of this merging ([word-grouping.md](word-grouping.md)).

## Density and the plots

### 24. The actual cost instead of a ratio to the first chapter; words learned so far (commit `8f46816`)

The user asked to stop normalizing the density: "Not normalizing would show the actual cost, which is more useful". The axis adapts instead, and a log scale keeps the curve's shape. The user also asked for a plot of cumulative learned words, to know how many words one has learned at a given chapter. **Words learned so far** counts the distinct words to learn from the start of the book up to each chapter, without those learned in other books; on 無職転生 it climbs to about 11,700. Plot scales are now saved as a list, so states saved with three plots still load (tested with an old state).

### 25. The density unit: 1,000 characters, then 100 characters, then 100 words (commits `8f46816`, `4572d8f`, `1c93087`)

1. **Per 1,000 characters:** chosen first only so the numbers would be above 1.
2. **Per 100 characters:** the user asked why 1,000, since 100 characters "would be closer to an average sentence". Committed in `8f46816`, with a hint saying "about a sentence".
3. **The numbers looked low:** the user was surprised by only about 5 new words per 100 characters in early chapters. Measured on 無職転生:
   - 100 characters hold only about 22 counted words: 9% of the text is spaces and line breaks, 10% punctuation, and half the rest is hiragana (particles, okurigana, endings), which is not counted. Chapter 1 has 1,114 counted words in 5,078 characters.
   - Of those words, about 27% are new in chapter 1, and close to half in the prologue. The filters remove only 10 to 20% of them, and 74% of chapter 1's new words appear only once in it.
   - Sentences average 23 characters in this book (much short dialogue), so "about a sentence" was wrong: 100 characters is about 4 sentences.
4. **Per 100 words, with a toggle:** the user proposed per 100 words, "a kind of percentage of new words", with a toggle for characters (`4572d8f`). On 無職転生 the prologue is at 39.7 per 100 words, against 9.4 per 100 characters.
5. **Per 100 words only:** the user found it "makes a lot more sense" and removed the per-character variant and its toggle (`1c93087`, about 55 lines). A saved unit setting is ignored.

A word here is a counted word of the chapter, the same ones the word lists are built from (particles only with that setting).

### 26. Log scales and the final plot order (commit `4572d8f`)

At the user's request, every plot starts in log scale, and the order is new words, density, words learned so far, chapter length. A saved choice of scales is kept. Tooltips follow the same order and were capped at 300 points wide: egui's default of 500 is wider than a phone, and tooltips were cut off.

## Retiring the SVG

### 27. The viewer replaces the SVG chart (commit `64999b7`)

The user: "now that we have the viewer, we can retire the old svg output to have a cleaner code". The chart code, its test, the `--ratio-scale`, `--words-scale` and `--chars-scale` options and the `plotters` crate were removed, about 470 lines (`statistics.rs` went from 400 to 57). `statistics/vocabulary_ratio.tsv` was **kept**, since it does not depend on plotters and its numbers can still be useful. The axis and number helpers (`chart.rs`) moved into the viewer, their only user.

## CI

### 28. Cargo's new build directory layout (commit `cdd015e`)

After `61d6f99`, the Pages build failed: `build-web-assets.sh` could not find the compiled UniDic. CI installs the latest stable Rust (cargo 1.101), and since cargo 1.100 build script outputs live in `build/<pkg>/<hash>/out` instead of `build/<pkg>-<hash>/out`. Locally, Rust was 1.99. The script now looks for the newest build under either layout, and stops with a clear message if there is none. Before, it went on and failed later in zstd. The user confirmed that CI passed.

## How changes were checked

- **Refactors and new features in the CLI:** compared byte for byte with the output from before the change, for the EPUB and both PDFs (`full/`, `words/`, statistics, console log without timings). One measurement was wrong at first because the binary had not been rebuilt after an edit (`cargo test` does not rebuild `jpfe`).
- **Dictionary changes:** word list counts and diffs before and after, on the books, plus random samples of the new words.
- **The viewer:** tested in the desktop app's browser pane with simulated file drops, clicks and touches, at desktop and phone sizes. Test books were copied into `viewer/dist/` only while testing. Safari, the iPhone and the iPad were tested by the user. The native window could not be driven or captured from the session, so the desktop build was only checked to start.

## What remains

| # | Open item | Notes |
|---|---|---|
| 1 | **Scheduled update of `jmdict.lock`** | A workflow that fetches the newest jmdict-simplified release and opens a pull request with a word-list diff, for review. Discussed in decision 22, not built. Updating is manual for now (README). |
| 2 | **The CLI's `vocabulary_ratio.tsv` still has the normalized ratio** | Its `raw_ratio` is per character and `normalized_ratio` is relative to the first chapter, while the viewer shows new words per 100 words. Bringing the table in line with the viewer was mentioned but not decided. |
| 3 | **Repeated chapter titles** | 最果てのパラディン restarts its numbering in each arc, so `first_appearance_chapter` can say `１` without the arc. New words are tracked by position, so counts are right. Naming the arc would need this author's arc markers, so it was left. |
| 4 | **Dictionary load time on phones** | About 2 s of unpacking on an iPhone 13. Cached books no longer wait for it, but importing a new book does. Native gzip decompression and a trimmed dictionary were both left aside (decisions 18 and 20). |
| 5 | **Okurigana left wrong by `adapt_kanji`** | When the fix in decision 21 was made, 躾付ける, 抑さえ込む, 感る and 這る still came out wrong. Not rechecked since the JMdict and grouping work. |
| 6 | **The in-app browser pane lost stored files between turns** | Seen several times while testing the viewer (the state or every file gone, the folders still there). The viewer was not at fault; `abd6fcb` later made storage safe against it (see [word-grouping.md](word-grouping.md), decision 9). |

## Kept as they are, on purpose

- **GitHub Actions for deployment,** rather than a self-built `gh-pages` branch (decision 16).
- **The dictionary is cached compressed and unpacked on every load,** because keeping 200 MB unpacked made Safari reload the page (decision 18).
- **The full UniDic is kept,** untrimmed, because more of it will probably be used.
- **JMdict is pinned,** never fetched as "latest" at build time: an update is a reviewed commit of `jmdict.lock`, and a build either uses exactly the pinned data or fails.
- **The katakana-only filter is viewer only,** so the CLI's output did not change.
- **Charts react to hover only,** with no pan or zoom, so drags scroll the page on phones; on touch screens, the long press inspects.
- **Progress starts at "Not started",** since the selected chapter counts as learned.
- **Compounds hide their parts:** a word seen only inside a JMdict compound is not listed on its own (詩人 in 吟遊詩人, 授業 in 授業中).
- **Density is per 100 words only;** the per-character variant was removed for simplicity.
