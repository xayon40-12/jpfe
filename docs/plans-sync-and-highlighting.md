# Cloud sync and word highlighting: decisions and plan

This document records two features decided on 18 November 2026, before any code was written: **syncing the viewer through the cloud** and **highlighting the parts of every word in the reader**. It covers what was decided and why, what was rejected, and the order of the work. When the work lands, its decisions move into their commits' history here (or into [decisions-reader.md](decisions-reader.md) for the reader), as the other documents do.

Both features build on the paginated reader with a saved place per book: decision 7 of [decisions-reader.md](decisions-reader.md) and its follow-up.

## Order of the work

1. **Finish the paginated reader** (in progress, not committed). The user tried the first version and asked for three fixes:
   - **No drag animation:** a drag switches the page at once, like the arrow keys.
   - **Restored on reload:** the app remembers its view. If the reader was showing a book, reloading reopens the reader on that book at the same page. This is part of the saved settings, so it will be synced.
   - **Where the reader opens:** a book opens at its saved place if it has one, otherwise at its first chapter, which may be chapter 0, a prologue. It no longer depends on *Learned up to*, since readers usually learn a few chapters ahead.
2. **Commit and push** the working reader.
3. **Cloud sync and word highlighting in parallel.** Each runs in its own worktree on its own branch, since both touch `viewer/src/app.rs`: sync adds to the saved state, highlighting adds a reader setting.

## Cloud sync

### Decided: the user's own Dropbox, in an app folder

The settings, the reader's place in each book, the view the app was in, and the books themselves are synced through the user's Dropbox, in a folder only this app can see.

**Why Dropbox:**

- **No server to run, and no cost.** The viewer is a static site on GitHub Pages plus a desktop app, and stays that way.
- **The books stay in the user's own storage.** They are copyrighted, bought files: storing them on a server of ours would mean hosting every user's books.
- **It stays signed in.** Sign-in uses OAuth with PKCE, which needs no secret key, so it works from the web page and from the desktop app. An access token lasts 4 hours, and the app renews it with a refresh token that has no fixed expiry and can be reused, until the user revokes the app. The user only signs in again if they revoke it, or if the browser clears the site's storage, where the token is kept.
- **The app folder permission** limits what a stolen token could reach to that folder.
- **It is fast enough.** Settings and places are one small JSON file, read or written in a single request. Dropbox can report whether anything changed since a cursor (`list_folder/continue`), so checking costs almost nothing. Books are 2–23 MB each, sent once per book per device.
- **It works on every platform**, an iOS or Android app included, with the same HTTP code.

### Rejected or deferred

How long each option keeps the user signed in, for an app that runs in the browser:

| Option | Signed in for | Why not |
|---|---|---|
| **Dropbox** (chosen) | until the app is revoked | |
| OneDrive | 24 hours, fixed (renewing doesn't extend it) | signing in every day |
| Google Drive, `appDataFolder` | about 1 hour without a server of ours | renewing every hour; files are found by id, not path; desktop sign-in is harder |
| iCloud (CloudKit JS) | 30 minutes, or 2 weeks with "Keep me signed in" | see below |
| A server of ours (e.g. Cloudflare Workers, R2, D1) | as long as we choose | we build sign-in and store everyone's books; only reasonable for a single user |

**iCloud, for a future iOS app.** It can be shared between the web and an iOS app through CloudKit, not iCloud Drive. Both use the same container: the iOS app natively, the web page through CloudKit JS, with the data in the user's private database. Deferred, not rejected:

- **Sign-in:** the web version must sign in again at least every 2 weeks.
- **Cost:** it needs the Apple Developer Program ($99 a year).
- **Apple ID required:** Android and Windows users need one.
- **Storage:** the books count against the user's iCloud storage.
- **Tooling:** CloudKit JS is old, with recent forum reports of sign-in bugs on the web.

It can be added later as a second storage provider (see the design below).

### Design to follow

- **Hide the provider behind a small interface:** read, write and list files by name, plus "what changed since last time". Dropbox is its first implementation, and CloudKit could be a second one.
- **Books need an id from their content,** such as a SHA-256 hash of the original file. Today's ids are counters local to each device (`next_id` in `SavedState`, `viewer/src/app.rs`), so the same book has different ids on two devices.
- **What is synced:** the settings, each book's reader state (`BookReaderState` in `viewer/src/reader.rs`), the view state from step 1, and the original book files.
- **What is not synced:** the analyses, which each device rebuilds (they are cached by a hash of the code anyway), and the dictionary.
- **Merging:** each synced record carries a `modified` timestamp, and two copies merge record by record, keeping the newest. Places merge book by book, so reading book A on the phone and book B on the computer keeps both.

### Open

- Where the sign-in button goes, and how sync status and errors are shown.
- What happens when a book is removed on one device: remove it everywhere, or only locally.
- How a book's local id maps to its content hash in states saved before sync.

## Word highlighting in the reader

### Decided

**A setting in the reader** highlights every word. It draws the rounded rectangle the reader already draws under the hovered word under every word, split into colored regions by the word's parts. The setting is off by default (not confirmed by the user yet).

**No tokenizer change is needed.** Each `ParsedWord` (`src/tokenizer.rs`) already carries:

- **`span`:** the whole word in the text, endings included (数えられる).
- **`kind`:** a word, grammar (particles and auxiliaries), a fragment, or a number.
- **`pos`:** UniDic's part of speech.
- **`affixes`:** the parts UniDic makes words of their own but that belong to the word, each with its own span, part of speech and kind:
  - the polite prefix before it (the お of お二人);
  - conjugation endings after it (られる, た, て, the ない of 美しくない);
  - suffixes (静かさ, 彼ら, the 家 of グレイラット家).

**Colors:**

- **The stem:** by part of speech (noun, verb, adjective, adverb, counter…).
- **Prefix, conjugation endings and suffixes:** one color for each type of part.
- **Particles and other grammar:** a quiet color, so that words stand out.
- **Numbers and fragments:** grey.
- Colors come from the theme's palette, light and dark (`viewer/src/theme.rs`).

**A word stays one rectangle when clicked:** its regions are only colors. Compound verbs JMdict lacks (握り直す → 握る + 直す) are already two words, so they show as two rectangles, as they click today.

**Line breaks:** a word cut by the end of a line or column gets one rectangle per line. Each rectangle has **square corners on the side of the cut** and rounded corners only at the word's real start and end, so it reads as one shape continued on the next line. The **hover and selection rectangle** follows the same rule (egui can round each corner separately).

### Possible later

- Shading the okurigana inside a stem differently (the べ of 食べ). This needs a split of the stem the tokenizer doesn't give yet.

### Survey: colors against the dictionary window (19 November 2026, nothing decided yet)

The first version colored a word's stem by UniDic's tag (first level, plus `proper_noun`), while the dictionary window picks a JMdict entry (`Definitions::lookup_as`, then the first entry read as in the text). The user saw them disagree (家 and 心 colored as suffixes, 伊達 as a name in 伊達に), and asked for a survey before any fix. **The user's rule:** a word is colored by the dictionary entry it actually is in the sentence: by default the first entry of its base form, but a later one when the sentence uses that one. The window marks that entry. A word on its own can never be a prefix or a suffix.

The survey ran the six books (8,139,899 words) through the tokenizer and the viewer's JMdict definitions. Scratch code and full tables: the session scratchpad's `survey/` folder (not kept).

**Tokenizer errors, found on the way (they affect word lists too), both fixed** (commit "Read the word after a full-width space, and each word in its sentence": decisions 18 and 19 of [word-grouping.md](word-grouping.md)):

- **Paragraph indents** (fixed: every non-ASCII space reaches UniDic as ASCII spaces, also inside a paragraph, after ！ and ？). UniDic reads the full-width space starting a paragraph (418,384 of them) as a noun, and the word after it then becomes a suffix with a wrong reading: 　心は → 心【しん】, 　家で → 家【か】, 　目が → 目 接尾辞, 　人を → 人【にん】. 8,046 words are affected (目 1,127, 人 705, 手 434, 家 210…). Fix: keep the indent away from UniDic, keeping byte offsets.
- **Readings cached by dictionary form** (fixed: each occurrence takes UniDic's reading in its sentence; the kana spelling, homograph and じる rules are kept). `resolve_reading`, across chapters: one misreading sticks to later occurrences. 1.81% of single-token words get another reading than UniDic gives in context: 心 しん for こころ 3,886 times, 傷 しょう for きず 1,600, 鬼 き for おに 776. Some differences are intended (いい for よい). The window picks its entry by this reading, so it shows the wrong entry too.

**Agreement today:** 92.4% of words get the color of the window's first entry's first sense; 3.2% have no JMdict entry (names, numbers); 4.4% disagree. Largest disagreements:

| Today vs JMdict | Count | Examples |
|---|---|---|
| noun vs な-adjective | 59,481 | UniDic 名詞-形状詞可能: 必要, 駄目, 無理, 不思議, 元気 |
| grammar vs other | 50,100 | the window's entry is wrong: のだ's ん shows 【ん】 "yes" 30,570 times |
| suffix vs noun | 31,967 | 族, 中, 目, 者, 国 |
| counter vs noun | 24,251 | 度, 年, 日: JMdict's first sense is a noun, but these are number + counter |
| noun vs pronoun | 16,868 | 自分 |
| noun vs adverb | 13,999 | 副詞可能 nouns: 絶対, 結局, 殆ど |
| grammar vs noun or suffix | 20,757 | ほど, くらい, さ, ずつ: the particle sense isn't JMdict's first |

Taking JMdict's first part of speech as-is would make grammar and counters worse: the rule must choose a **sense**, not just an entry.

**Choosing the sense used.** The following token, as UniDic tags it, decides reliably for nouns that can be な-adjectives: な, に, で, だ tagged 助動詞 (copula) mean the な-adjective, a 助詞 means the noun (完璧: な 126, に 123, だ 41, で 23 as copula; と 6 as particle). 「心は完璧に折れていた」 has に as copula, so 完璧 is the な-adjective there. No tokenizer change is needed: the next word in the reader's list carries it. Not reliable: だ/です (the same for both), adverbial nouns (絶対, 今) with a dropped particle, and words UniDic mis-tags.

**Standalone suffixes and prefixes:** 75,976 suffixes and 2,070 prefixes are words of their own; 66,891 of the 78,046 come right after another word (real suffixes the tokenizer doesn't group), most of the rest are the indent errors. The user's rule works for noun-like ones (族, 国, 者, 団). It fails for honorifics and name suffixes (さん 8,843, 様 9,563, 君, ちゃん, 殿, 教, 流): JMdict has them only as suffixes, or only as an unrelated word read the same (さん → 酸 "acid"). They need a fixed fallback, or grouping into the previous word (a tokenizer change that alters word lists).

**Proper nouns** (157,559 occurrences): 76% have no JMdict entry (real names). Of the 36,752 with one, kanji ones are mostly UniDic mis-tagging common words (悪魔, 竜, 王, 日本, 伊達), katakana ones mostly real names that happen to be loanwords (エリス, マナ, レム). JMdict's "common" flag separates the two almost perfectly (exceptions: ローズ, アイリス), but `definitions.tsv` doesn't keep it yet (a change to `scripts/jmdict-definitions.py`). Invented katakana names UniDic doesn't know (ロズワール, ルイジェルド: 45,912 occurrences) are tagged as common nouns with no JMdict entry; UniDic also splits many into fragments (グレイ+ラット, ミ+リス). ハロワ can't be told apart: JMdict has it as a noun.

**Other findings:** counters color the number with them (十年 is one word, read じゅうねん), which the viewer could split into a grey number and a teal counter; 完璧な is two rectangles while 静かな is one; interjection fragments (あ, え) are grey but JMdict has them as interjections; 魔法陣 shows 魔方陣 "magic square".

**Proposed rule (from the survey):** one function gives each word its entry, sense and color, used by both the highlight and the window, which marks that sense ("as used here"):

1. Grammar keeps the grammar color; the window picks the particle or auxiliary sense (fixes ん, ほど, くらい).
2. Numbers grey; number + counter keeps the counter color, the window shows the counter sense.
3. Words: the part of speech of the chosen sense: な-adjective before a copula, noun before a particle, adverb when the entry has one and no particle follows, otherwise JMdict's first sense, UniDic breaking ties.
4. A standalone suffix or prefix: the first part of speech of an entry read as in the text that isn't an affix; noun when there's none (さん, 様).
5. Proper nouns: UniDic's 固有名詞 and no common JMdict entry with that reading.
