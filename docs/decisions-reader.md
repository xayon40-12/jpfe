# The reader and its dictionaries: decisions and what remains

The viewer started as a statistics tool: charts of the words to learn in each chapter of several books. This document records how it gained a **reader** (the book's text, full screen, where any word can be clicked to see its dictionary entry and the kanji it is written with), and the changes to the data and the tokenizer that the reader brought along. Decisions are listed in the order they were taken (14 November 2026), with their commit, what was decided and why; then the approaches that were rejected, and what is left open.

The rules for grouping a word with its endings, numbers and prefixes, which came right after this work, are in [word-grouping.md](word-grouping.md). The rules as they apply today are summed up in the README.

## Starting point: JMdict, pinned (commit `61d6f99`, before this work)

UniDic splits text into short units, so the tokenizer merges consecutive tokens that form a JMdict headword (神出鬼没, 冒険者). JMdict comes from [jmdict-simplified](https://github.com/scriptin/jmdict-simplified), which publishes it as JSON every week and keeps old releases: `jmdict.lock` pins one release and the SHA-256 of its archive, and `scripts/fetch-jmdict.sh` downloads it, refuses it if the checksum differs, and extracts what the code needs into `target/jmdict/`. Nothing of the dictionary is committed. The extracted files stay sorted TSV searched by binary search, because maps of 200,000+ words take tens of MB, which matters in a browser on a phone. Everything below builds on this.

## Decisions

### 1. A reader with word and kanji lookups (commit `b3ed8b8`)

**The reader.** A small button at the very top left switches between the statistics and the reader (📖 to read, 📊 to go back). The reader takes the whole window: a thin bar (book name, previous and next chapter, a chapter list, settings) above the text.

- **Where it opens.** The request was "the currently selected book at the currently selected chapter", but the viewer had no selected chapter. It opens at the book's *Learned up to* chapter (the first chapter before any), the chapter whose words the reader has just learned. If that setting has not changed since the reader was last open, it reopens where it was left instead, so switching to the statistics and back does not lose the place. (Replaced by decision 7: the reader now always opens where it was left, and *Learned up to* no longer moves it.)
- **Font size.** Pinching, Shift + mouse wheel, Cmd/Ctrl + wheel, and a slider in the settings window (10–48 pt, default 20). It is saved apart from the word-list settings, because changing those rebuilds every book's word lists.
- **Finding the words.** Analyses only keep counts, not where words are, so the reader tokenizes the chapter it shows when it opens it. `ParsedWord` gained a `span`: where the word is written in the text (bytes), its surface even when conjugated.

**The dictionary data.** Two files are extracted from the same pinned release, beside the headwords:

- `definitions.tsv` (`scripts/jmdict-definitions.py`): every JMdict entry (spellings, readings, senses with their part-of-speech and usage tags), then an index of every spelling and reading. 28 MB raw, 7.9 MB compressed.
- `kanji.tsv` (`scripts/kanjidic.py`), from **KANJIDIC2** (also published by jmdict-simplified, release for release): on, kun and name readings, meanings, stroke count, school grade, old JLPT level and newspaper frequency. 0.7 MB raw, 0.2 MB compressed. Its checksum is pinned in `jmdict.lock` as `kanjidic_sha256`.

`src/dictionary.rs` reads both by binary search, like the headwords. The desktop app embeds them (parsing takes about 50 ms at startup). The web app downloads them with the UniDic files, under the same `version.txt`, so there is one dictionary version to cache and invalidate: the download grows by about 8 MB, and the definitions take about 28 MB of memory in the browser.

**The windows.** Clicking a word opens a dictionary window next to it, clicking a kanji of the word in that window opens a kanji window; Escape or a click beside the words closes them. Parts of speech are shown with short names for the common tags (noun, godan verb, usually in kana), because JMdict's own descriptions are long ("noun or participle which takes the aux. verb suru"), and only when they change from one sense to the next.

**Also fixed:** the viewer's two existing UI tests failed on `main` because egui 0.37 asserts when a test drops the frame's texture changes; the tests now clear them.

### 2. Vertical text on a grid (commit `dab5062`)

Feedback on the first reader: half-width characters broke the alignment, PDF page numbers ("40") appeared among the text, and Japanese is normally read top to bottom, right to left.

- **Vertical text by default**, horizontal as a setting. egui only lays out horizontal text, so the reader places every character itself, **one character per cell of a grid**, in both directions (horizontal text uses the grid too, so it stays aligned).
- **Full width.** Half-width ASCII is shown full width (`40` → `４０`), so every character fills one cell. The mapping is one character for one, so word positions stay valid.
- **Vertical forms.** Long vowel marks, dashes, ellipses and brackets are drawn turned a quarter (egui can rotate a text shape); 、。 and small kana move to the top right of their cell.
- **Line breaking (禁則).** Punctuation and closing brackets hang at the end of a line rather than start the next one (two at most); an opening bracket moves to the next line with what it opens.
- **Scrolling.** In vertical text the content scrolls sideways and starts on the right; the mouse wheel moves on through the columns. The place in the chapter is kept as a fraction when the text is laid out again (zoom, window resize, direction).
- **Phones.** The top bar is laid out from the right (settings, next chapter, then the chapter list in the room left), and the book name is hidden on narrow screens.

**PDF paragraphs.** The PDFs keep every printed line break, which made paragraphs out of each line ("実に興⏎味深い") and, worse, cut words in two for the tokenizer. The PDF extractor now joins lines back into paragraphs:

- A line continues on the next one when it reached the full line width and did not end a sentence, unless the next line is indented. The full width is measured per book (the most common length of long lines: 30 characters in all six test books).
- Every page ends with an empty line and its page number (in a Latin font): the empty line is dropped, and a page number met inside a paragraph moves to just after it. The reader shows page numbers of PDFs half size, in grey, at the end of a line of their own.
- A line cut in two by furigana is joined back when both parts fit on one line.

The 無職転生 PDF's word list differed from its EPUB's by about 1,200 words, mostly fragments cut at line breaks (況, 与, ディ, ーン). After joining, they differ by 120, mostly Latin words the two editions write differently.

**First general verb rule.** 読み進めて was looked up as 読み進める, which JMdict lacks (it has 読み進む): a verb JMdict does not have, ending in -eru, maps to its godan verb when JMdict has that one (め→む, せ→す, れ→る).

### 3. General rules for words JMdict lacks (commit `6e197ed`)

**Survey first.** Tokenizing the six books found 511 verbs and adjectives (3,993 occurrences) whose base form is not in JMdict. They fell into patterns, each now a general rule, tried in this order for a verb or adjective JMdict does not have:

1. **The base form as written, or UniDic's lemma**: UniDic's lemma may use other kanji than the text (旨い for 上手く, which the old code turned into "上い"; 悔しい for 口惜しい).
2. **Expressions only used in the negative**, when the next token is ない, ず or ぬ: UniDic makes up a positive verb (思い掛ける + ない is 思い掛けない; also 計り知れない, 言い知れぬ, 居た堪れない). JMdict lists some only as rarely used spellings, so any JMdict spelling counts.
3. **A word written in kana**: its base form in kana (よろめく rather than UniDic's 蹌踉く).
4. **Causative and passive forms**: their verb (思わす → 思う, 付き合わせる → 付き合う, 魅入られる → 魅入る), then the -eru → godan rule above.
5. **Compound verbs JMdict does not list**: the two verbs they are made of (握り直す → 握る + 直す, 消えかける → 消える + 掛ける), each clickable in the reader.

This **replaced the hand-written `DERIVED_LEMMAS` table**: all nine of its entries come out of these rules. About 100 words (338 occurrences) remain unmapped, mostly rare ones (鷲掴む, 青む).

**One word, the book's usual spelling.** A word the text writes both in kana and in kanji (できる and 出来る, こと and 事) was counted twice. It is now counted once under its dictionary form (`base_word`, which identifies it in counts and across books) and listed in the spelling its book uses most (`spelling`, per book in `BookWord::spellings`). The kana form counts as a spelling of the kanji word only when **JMdict lists it as one of the word's readings**:

| The kana form is | Result | Examples |
|---|---|---|
| a JMdict reading of the dictionary form | the same word | できる / 出来る, ほんと / 本当, みんな / 皆 |
| not a reading, but a JMdict word of its own | its own word (hiragana form first, same part of speech for verbs and adjectives) | いい (an entry apart from 良い), すっごい, やっぱ; イイ joins いい |
| neither | a verb or adjective form counts as its verb; a noun stays its own word | かえれる counts as the verb; おめぇ stays apart from お前 |

To decide this, the headwords file gained every reading of each kanji word, and JMdict's kana-only words (kind `a`): 9 MB → 13 MB raw, still about 2.1 MB compressed. `fetch-jmdict.sh` now also rebuilds the extracted files when the script that makes them changes, not only for a new release.

This **replaced the hand-written `KANA_LEMMAS` table** (する, いる, なる, ある, ない, いい). The viewer keeps dictionary forms in its word lists, so a word learned in a book that writes 出来る is left out of a book that writes できる; the zip download shows each book's spelling. Lists shrank by about 3% (無職転生: 25,602 → 24,759 distinct words at `-m 1 -p 100`), mostly words that used to be counted twice.

### 4. The dictionary window (commit `df8e980`)

- **First line:** the word, large, with clickable kanji, and its readings in 【】 (the one used in the text first). The entry matching the text's reading comes first without a header of its own, so the word and its readings are no longer repeated below; other entries spelled the same way keep theirs.
- **Colors**, like a code editor, so the parts can be told apart at a glance: clickable kanji blue, readings orange, parts of speech purple, sense numbers teal, notes and brackets grey. In the kanji window, on'yomi teal and kun'yomi orange with dimmed okurigana (い.きる). The palette has text shades darker than the chart colors in light mode and lighter in dark mode (`Palette::reading`, `grammar`, `marker`).
- **Wrapping:** readings are kept whole (a reading that does not fit moves to the next line); kanji window rows wrap within the window.
- **Placement:** a window opens beside the word (on its left in vertical text) or beside the dictionary window, and below when there is no room there (phones). The next chapter button fits beside short vertical chapters.

The window was later compacted further by the word-grouping work (commit `332188a`): titled with the word as the text writes it, other spellings on the first line, and the meaning of the conjugation instead of the "Also written" notes.

### 5. The tokenizer reports every word; word lists choose (commit `ba86485`)

The user noticed that 家 (in グレイラット家) and 冊 (in ５冊) could not be clicked: UniDic tags them as suffixes, and the tokenizer dropped suffixes for every use. A survey of 10.2 million tokens showed what was dropped: suffixes (174,000 occurrences: 的, 者, 性, 様, 家, 歳, 枚, 冊), prefixes (38,000: 不, 無, 超, 最, 再), single hiragana (16,000, mostly interjections but also the conjunctions が and で), and a hand-written list of "problematic" words (12,700) that, matching by spelling, also removed real words: とって (取る, 2,851), によって (因る, ~2,900), マジ, じゃあ.

**Decided:** the tokenizer reports every word it finds with a `WordKind` (a word, grammar, or a fragment), and word lists decide what to count; the particle setting moved from the tokenizer to the counting. The contractions the list targeted (てる, とく, ちゃう, とる) are auxiliaries for UniDic, so the grammar rule already covers them, and the list was removed. Default word lists grew by 1–2% per book. This is decision 1 of [word-grouping.md](word-grouping.md), which continues from here.

### 6. Entries of the word's part of speech first (commit "Verbs in じる as written, and the particle は in the reader")

Clicking the particle は showed the noun 歯 "tooth" first: the window looks up every entry spelled or read は, most common first, then moves the first one read as in the text to the top, and 歯, 葉 and the particle are all read は.

**Decided:** `Definitions::lookup_as` puts first the entries with a sense of the word's part of speech, as the tokenizer gives it (UniDic's 助詞 is JMdict's `prt`, 助動詞 `aux` or `cop`, 動詞 any `v` code…; JMdict's own label for a merged word), keeping the most common first among them; the window then takes the first of those read as in the text. The particle は comes first, and 歯 follows as another word written the same. A part of speech JMdict does not tell apart (a symbol, a number) keeps the usual order. Checked on は, が, に, を, の and で against the embedded JMdict.

### 7. Pages, and the place saved per book (commit "Pages in the reader, and the place saved per book")

The reader scrolled through a chapter, and forgot where it was when the app restarted.

**Pagination, on by default** (a *Paginate* setting in the settings window, saved like the font size and direction; off is the scrolling reader as before).

- **A page is whole columns** (whole lines in horizontal text, which now pages too instead of scrolling down): as many as fit between the margins, never a cut one. A page starts at any line, not at fixed boundaries, so the page can start with the saved place's line after a relayout. Turning forward starts the next page after the last whole line, skipping the empty lines between paragraphs. Turning back goes to the last page, counted from the chapter's start, that starts before the current one, so going back shows the pages going forward showed and the first page keeps the chapter title (the first version took the full page ending just before the current one, which could start just after the title and hide it). The next chapter button is the last item of the last page (on a page of its own when the last page is full).
- **Turning by dragging**, which feels natural on a phone and works with click-and-drag on a computer. The drag follows the reading direction, as when turning a paper book's pages: vertical text reads to the left, so the next page is pulled in from the left, by **dragging toward the right**; horizontal text reads to the right, so its next page comes by **dragging toward the left**. While the pointer (or finger) is down, the page shown follows how far the drag went from where it was pressed, between **two pages only**: the page shown when the drag started, and one other page, fixed by the first direction the drag goes past 6% of the view's width (24 to 64 points): the next page if it first goes toward the next page, the previous one if it first goes the other way. For the rest of the drag, back within the threshold shows the starting page again, and past it in that same direction the other page again; far the opposite way stays on the starting page, never a third one. Whole pages switch at once, without following the pointer, so moving back and forth alternates between the two, as when peeking at the next page of a paper book and letting go of it. The drag lasts until the button or finger is up, measured from where it was pressed, so it never starts over from the page it shows. A page shown stays until the drag comes back to 70% of the threshold, so that it does not flicker there. The page the drag started on keeps its place while the drag goes on; on release, the page shown stays and its place is saved. When the next or previous page is in another chapter, the app opens that chapter and the drag goes on there, so it can still come back. egui tells a click from a drag (6 points), so a tap still opens a word's window, and a drag never does.
- **Arrow keys**, by reading direction too: ← is the next page in vertical text, → in horizontal text. They are left alone while a widget has the keyboard (a text field, or keyboard navigation).
- **The mouse wheel does nothing** while paginated (Shift or Cmd/Ctrl + wheel and pinching still zoom): scrolling a wheel to turn pages was not asked for, and trackpads send many small wheel events that would turn several pages.
- Past the last page, the next chapter's first page; before the first page, the previous chapter's last page. Turning a page closes the word and kanji windows, whose word is no longer in view.

**The place saved per book.** The reader keeps an **anchor**: the start of the sentence the page (or, when scrolling, the view) starts in, in bytes of the chapter text. The sentence goes back from the first character of the page's first line to just after 。！？ and the closing brackets after them, within the paragraph. The anchor moves only when the reader turns a page or scrolls; when the text is laid out again (zoom, window size, direction, pagination), the page starts with the anchor's line (scrolling: that line at the start of the view). Moving it on relayout would drift back a little at each zoom. This replaced the fraction of the chapter kept before, which also landed elsewhere when the text changed length.

It is saved in the state (`Key::State`, which keeps the book list and settings) as `reader_books`: per book id, one `BookReaderState` with the chapter, the anchor's offset, its first 16 characters, and when it last changed (milliseconds since 1970). It is one plain struct per book with a timestamp so that settings and places can later be synced between devices by keeping the newest one. The 16 characters find the sentence again (the occurrence nearest the old offset) if the chapter's text changes, for example when a new build extracts it differently. Old state files without it load with no saved places and pagination on.

- **When it is saved:** right after a page turn, 0.6 s after scrolling stops (and not while a finger is down), when a chapter is opened, and when leaving the reader. The state file is written only when the place changed, not every frame.
- **Where the reader opens:** at the book's saved place, always; a book never read opens at its first chapter (chapter 0, which may be a prologue). Removing a book forgets its place.
- **The view is saved too:** the state keeps `view`, a `ViewState` with the book shown in the reader (`None` for the statistics) and when that last changed, so it merges by newest like the places. The app reopens the reader on that book, at its saved place, once the book is ready after a reload or a restart; otherwise it shows the statistics. Old state files load with the statistics. Nothing is drawn but the background until the state is read (on the web it comes from storage, asynchronously, a few frames after the start), and while the book is restored or analyzed again the reader's bar shows above "Opening the book…", so the statistics never show for a moment before the reader. If the book is gone, or fails to load (or the dictionary does), the statistics show instead.
- **Startup keeps redrawing.** On the web the dictionary is unpacked from its zstd files a little each frame, in what is left of the frame's 25 ms (the largest file, `matrix.mtx`, took 225 to 300 ms in one frame), and the app reads one stored book per frame (an analysis decoded, 25 to 60 ms, or a file parsed). With the load order below, the second book's analysis arrived in the frame that unpacked `matrix.mtx`, which made a frame of about 300 ms in the browser pane; the user saw the window freeze for over a second, with the spinners stopped. Measured on a reload with two books stored (the browser's long tasks): 289, 143, 95, 84, 57 and 51 ms before, 66 and 63 ms after. Parsing a book's file when a new build analyzes it again still takes one frame (about 270 ms for 無職転生's EPUB).
- **The last selected book loads first:** the book being read, or else the one shown in the statistics. Its analysis is read from storage alone, before the others' are; when a new build analyzes the books again, it is analyzed first, and the others follow by id (their order on screen), whatever order their files come in. Before, the analyses were all read at once and the books analyzed in the order their files were read, so the reader could wait for every other book after a new build.

**Changed after the user tried it** (same day, before committing):

- **No drag animation.** The first version moved the page with the pointer and turned it on release. The user wanted a drag to switch whole pages at once, like an arrow key; the following code was removed. A second version turned the page once, as soon as the drag crossed the threshold; the user then asked for the page to follow the drag's net movement while it is held, so that a drag can be taken back. That third version could reach three pages in one drag (the next page, then back past the start to the previous one); the user wanted two, the starting page and the first one reached (above).
- **The view across reloads.** Reloading while reading went back to the statistics; the user wanted to come back to the same book and page, and to the statistics when they were showing. It is saved with the settings so the planned sync can carry it. The first version showed the statistics until the book was ready, which flashed them on every reload; and the book being read could load last.
- **Learned up to no longer moves the reader.** Readers often learn the words of chapters ahead of where they read, so opening at that chapter (decision 1, then the first version of this one) took them away from their place. The setting is no longer stored with the place: old places carrying it still load, and it is ignored.

## Rejected or replaced approaches

- **Egui's own window scrolling** for the dictionary windows: it always takes the maximum height, leaving empty space under short entries. Replaced by a scroll area that shrinks to its content.
- **Vertical presentation forms** (﹁, ︒) for vertical text: fonts may lack them; rotating and shifting the normal characters works with any font.
- **Joining PDF lines from their positions on the page:** the extractor does not track text positions; the line-length rule needed no new parsing and gave paragraphs close to the EPUB's.
- **JMdict's "usually written in kana" tag** to replace `KANA_LEMMAS`: it does not match the hand-written list (it tags 1 of 4 senses of 居る, 1 of 6 of 無い, none of 良い), and would have switched 371 other words to kana. The user chose the book's usual spelling instead.
- **Keeping the six hand-written kana rules** (the other option offered): kept rules that are not general, and counted words twice when a book uses both spellings.
- **A length check for kana spellings** (the kana form as long as the dictionary form's reading, to exclude potential forms such as かえれる): the first version of decision 3. The user asked whether JMdict could decide instead; the JMdict readings rule replaced it and also merges variants of other lengths (ほんと / 本当).
- **A "keep every word" mode for the reader only**, to make 家 and 冊 clickable: written, then removed at the user's request. The reader must show exactly what the tokenizer finds, since that is how problems in the tokenizer get noticed. The fix went into the tokenizer instead (decision 5).

## Investigated, not a problem

- **Clicks landing one column off in vertical text:** measured at full resolution, the pointer was on a column boundary; the hit test is correct.
- **A stored book disappearing in the desktop app's browser pane:** reproduced by changing the dictionary version and reloading, which kept the book. The pane's storage had been emptied. The viewer was later made to protect stored books anyway (commit `abd6fcb`, see word-grouping.md).

## What remains

### Open

- **Words in the word lists are not marked in the text.** The reader could color the words of the current chapter's list, or the words not yet learned.
- **No furigana** in the reader.
- **Turning back after a relayout** (or a restored place) gives a page counted from the chapter's start, which may repeat a few lines of the current page, since the current page then starts at the saved sentence rather than on a page boundary.
- **Parsing a book's file blocks one frame** (about 270 ms for 無職転生's EPUB) when a new build analyzes the stored books again, or a book is opened; the EPUB and PDF extraction would need to run in parts, or in a worker, to avoid it.
- **A sentence longer than a page** saves a place a page back (the sentence's start), which the reader then reopens at.
- **に於いて and に於ける** come out as the verb おく, written in kana (UniDic's lemma 於く is not a JMdict word). They are particle phrases, better treated as expressions (において, における); see the expressions item in word-grouping.md.
- **Phrases UniDic misreads as one kana verb** show in kana (役にたちそう as たちそう, from 立ち添う); they were wrong before too, under a kanji lemma.
- **PDF paragraphs** whose sentence ends exactly at the end of a full line are not joined to the next line when it is not indented, which some PDFs do for continuations (陰の実力者になりたくて).
- **On phones, the chapter list** in the reader's top bar is cut short (e.g. "0: プ…"); it still works.
- **A scheduled JMdict update** (a pull request moving `jmdict.lock` to the latest release) is not set up; `scripts/fetch-jmdict.sh --update` does it by hand.

### Kept as they are, on purpose

- **The web download** grows by about 8 MB for the definitions, and invalidates the stored dictionary whenever `version.txt` changes.
- **Columns fade at the edges** of the screen while scrolling: egui's scroll area does it.
- **Kana forms that are not JMdict readings:** a noun keeps its own entry (おめぇ apart from お前), as before the spelling rule, since a noun's kana form is not an inflection.
- **Every viewer code change re-analyzes stored books once**, since cached analyses are keyed on a hash of the code that computes them.
