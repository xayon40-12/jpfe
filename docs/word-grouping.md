# Word grouping: decisions and what remains

UniDic splits Japanese into short units: a verb and each of its endings, a number and its counter, a prefix and its noun. The tokenizer (`src/tokenizer.rs`) puts back together what belongs to one word, so that:

- word lists count **the word to learn**, under its dictionary form, once;
- the reader looks the word up **wherever it is clicked**, and says what the rest of it means (its conjugation, its number, its prefix).

This document records how each grouping rule was decided, in the order the work was done (November 2026), and the grouping problems that remain. The rules as they apply today are summed up in the README, under [Which Words Are Listed](../README.md#which-words-are-listed).

## How a grouped word is represented

`ParsedWord` keeps one word with what belongs to it:

| Field | What it holds | Example: 食べさせられた |
|---|---|---|
| `base_word` | The dictionary form: what is counted and looked up | 食べる |
| `span` | Where the whole word is written in the text: what the reader highlights and the window's title | 食べさせられた |
| `affixes` | The pieces UniDic made words of their own: a prefix before, endings and suffixes after. Counted as grammar, with `--include-particles` only | させる, られる, た |
| `forms` | What the affixes mean, shown in the dictionary window in the color of grammar | causative (させる), passive or potential (られる), past (た) |
| `written_reading` | How the whole span is read when it is more than the word: a number with its counter | (一つ: ひとつ) |
| `homograph` | The reading that tells apart words written the same (see below), part of `key()` | (ように: よう) |
| `kind` | `Word` (listed), `Grammar` (listed with `--include-particles`), `Fragment` and `Number` (never listed) | Word |

The steps run in this order in `JapaneseTokenizer::tokenize`: JMdict headword merging (`longest_headword`), then the per-token rules, then `attach_counters`, `attach_endings` and `attach_prefixes`.

## Decisions

### 1. The tokenizer reports every word; word lists choose (commit `ba86485`)

Before, the tokenizer dropped particles, auxiliaries, prefixes and suffixes itself, and kept a hand-written list of "problematic" words. That list removed real words that share their kana with contractions (とって is 取る, によって is 因る). Now every word is reported with a `WordKind`, and `ParsedWord::is_listed` decides what word lists count.

Side effect that started this work: the reader made every token clickable, including the た of あった and the て of よって, which are not words.

### 2. Conjugation endings belong to their word (commit `7ce872e`)

**Decided:** auxiliaries (られる, た, ない, ます, たい), the particles that make conjugated forms (て, ば, たり, ながら), the stems of そうだ and ようだ, and the ない after an adjective (美しくない, 行きたくない) attach to the verb, adjective or auxiliary before them. After a na-adjective, the copula attaches (静かだった, 静かな). 数えられる is counted as 数える, and the reader says "passive or potential (られる)".

**Decided to keep apart:**

- **Verbs after て** (書いて + しまう, 読まれて + いる, 見せて + もらう). They are dictionary words of their own, worth learning. Colloquial contractions that UniDic tags as auxiliaries do attach (読んでる → "ongoing (てる)", 食べちゃった → "completed (ちゃう)").
- **What follows a noun**: する (勉強 + した) and the copula (学生 + だった), because nouns do not conjugate.
- **ない after a noun** (関係ない): it stays a word.

The ない of an adjective used to be counted as the word 無い. Attaching it removed about a quarter of the occurrences of ない from the lists.

### 3. Words written the same are counted apart (commit `7ce872e`)

Found on the books: UniDic gives 様 the same lemma in エミリア様 (さま) and in ように (よう). Once よう after verbs attached to them, the entry flipped to 様 read さま with ようだ in it.

Approaches tried:

1. **UniDic's lemma reading as part of the key**: rejected. UniDic reads kanji 私 ワタクシ but kana わたし ワタシ, which would have made them two words.
2. **One word per JMdict entry**: too fine. JMdict has わたし and わたくし, なに and なん, in entries of their own.
3. **Kept: entries with no meaning in common.** A reading is another word when its JMdict entry shares no gloss with the entry of the usual reading. `scripts/jmdict-headwords.py` writes those readings as the sixth field of the headwords file, and the tokenizer adds the reading to the word's key (`様【よう】`).

The rule splits 様 (さま, よう, ざま), 中 (なか, ちゅう), 止める (とめる, やめる), 人 (ひと, にん), and kana words that were counted under the wrong kanji: どれ and いずれ (both 何れ), どう and いかが (both 如何), まとも (正面). It is conservative where meanings overlap: 方 (かた, ほう) stays one word. The key is internal: word lists and exports show the spelling and reading as before.

Bug found on the way: readings are cached by dictionary form, so a homograph takes its own reading rather than the cached one.

### 4. The dictionary window (commits `7ce872e`, `332188a`)

The conjugation is shown in the color of grammar (`palette.grammar`), like parts of speech. The window was compacted, in this order:

1. **Title**: the word as the text writes it (数えられるだろう).
2. **First line**: the word, its other spellings (with clickable kanji) and its readings.
3. **Second line**: the number it counts with its reading, for a counter (一つ【ひとつ】).
4. **Third line**: what its affixes mean (passive or potential (られる), presumptive (だろう)).
5. Then the separator and the definitions.

### 5. Numbers and counters (commits `5f635e6`, `45b1860`)

**Decided:** a number followed by a counter is one span, and **the counter is the word** (一つ is つ, 三匹 is 匹). The dictionary window shows the counter first, then the whole with its own reading (一つ【ひとつ】), because counters have many irregular readings.

The reading of the whole:

- **The number:** as UniDic reads it in context (ヒト in 一つ, ミッ in 三日, ハツ in 二十日). Numbers in digits, a single kanji digit, and kanji written digit by digit with a zero (二〇二〇) are read from their value instead, since UniDic reads digits one by one (10 → いち + ぜろ).
- **The sound changes** before the counter, which UniDic does not apply between tokens: っ after いち, はち, じゅう (and after ろく, ひゃく before k and h), which turns h into p (いっぽん, じゅっぷん, はっさい); voicing after さん, なん, せん, まん (さんびき, さんぼん); counter exceptions 分 → ぷん, 階 → がい, 足 → ぞく.
- **Whole readings** for digits: 1つ ひとつ…10 とお, 1人 ひとり, 2人 ふたり, 3日 みっか…, and 4, 7, 9 before 時, 人, 年 (よじ, しちじ, くじ).

**Decided: numbers are never listed.** They are learned as a system, not word by word, the same way conjugations are stripped down to the base. A number with no counter is one word of kind `Number` (二十三, 2020, 数十, 何百): the reader looks it up with its reading, and word lists leave it out. Numbers are recognized by UniDic's lemma too (ひと and イチ are 一). 何, 数 and 幾 alone stay words.

**第** before a number makes it an ordinal (第二話 だいにわ, 第三 だいさん), and attaches to a word that starts with a number (第一軍 is 一軍).

Guards added after the six-book runs:

- **Adjacency in the text**, not only in tokens. Line breaks have no token, so a PDF page number at the end of a line took the first word of the next line as its counter (12448 + ら). This applies to endings too.
- **Single kana UniDic takes for numbers** are not numbers (the く of the laugh くふ, the シ of シッ), nor kana it takes for 第 (ダイスキヤキ, だいぃぶ). A number of two kana or more is one (ひとつ, いくつ).
- **Decimals** keep their counter apart (1.5倍).

### 6. Derivation suffixes and plurals (commit `35ed48c`)

**Decided:**

- **Derivations:** さ, み, げ, め and がる after the stem of an adjective, a na-adjective or a noun that can be one are part of it (静かさ is 静か, "noun of degree (さ)"; 寒がった is 寒い, "shows signs of (がる), past (た)"; 子供っぽさ; 真剣さ). Since decision 12, 寒がった is JMdict's 寒がる and 子供っぽさ JMdict's 子供っぽい: their conjugated forms were not found in JMdict then.
- **Plurals:** ら, 達 and ども after a noun or pronoun are part of it, also after a noun ending in a suffix (彼ら, 子供達, ママさんたち, 候補者たち).
- **JMdict words made this way stay whole** (美しさ, 強さ, 嬉しがる, 悲しげ), because they have dictionary entries of their own.

### 7. Polite prefixes (commit `bef0f22`)

**Decided:** お and ご (or 御) are part of the word after them (お二人 is 二人, since decision 10 人; ご出席 is 出席, お聞き is 聞く), shown as "polite (お)". JMdict words with the prefix stay whole (お茶, ご飯, お母さん), and the interjection お stays apart (お、おじゃま). This is why `endings` became `affixes`.

### 8. How tokenizer changes are checked

Every tokenizer change is run on the six books in `source/` against the last commit, with `-p 100`. At the default 99%, the cutoff moves when chapter totals change, and words near the edge seem to appear or disappear. Most of the guards above were found this way: page numbers, kana misread as numbers, kana numbers, plurals after suffixes, the 様 homograph.

### 9. Viewer storage (commit `abd6fcb`)

Not a grouping rule, but decided during this work. While testing in the desktop app's browser pane, a stored book disappeared. The pane's storage for that origin had been emptied (it is not persistent), and the next book opened reused id 0. The viewer now asks the browser for persistent storage once books are stored, gives new books an id after every stored `books/<id>` folder, and keeps and reports folders missing from the book list. Test books go in an origin with no real books (127.0.0.1:8766 rather than localhost:8766).

### 10. 一人 and 二人 are counted under 人 (commit "一人 and 二人 are counted under their counter")

UniDic keeps 一人 and 二人 as one noun (lemma 一人, 二人, read ヒトリ, フタリ), also written ひとり, ふたり, １人 and ２人, while 三人 is 三 + 人. They were listed as words of their own (about 5,500 and 3,900 occurrences), against decision 5.

**Decided:** a token whose lemma is 一人 or 二人 is the counter 人【にん】, with the whole as its written reading (二人【ふたり】), like 三人【さんにん】. With an ordinal it is 二人目【ふたりめ】, also when UniDic tags that 目 as the noun "eye" (二人目を産む), which it does after 二人 but not after 三人. JMdict's 二人目 is no longer merged, like 三人目. The written reading of a counter now includes a polite prefix before it (お二人【おふたり】, お一つ【おひとつ】; it was お二人【ふたり】).

**Kept as words:** the JMdict words made of them that UniDic keeps whole or that merging finds (一人一人, 一人前, 一人暮らし, 二人組, 二人連れ, 二人がかり). A first version also kept merging from taking 二人 + any suffix, which split 二人連れ and 二人がかり into 人 and a suffix of no use alone: only the ordinal 目 is held back now.

**Found while checking:** 一つ and いくつ, listed in the table of what remained, were already counted under つ: UniDic splits them (一 + つ, いく + つ) in every context tried, and the six books list no 一つ or いくつ. 独り (alone) has its own lemma and stays a word.

On the six books, the only changes are 一人, 二人, １人, ２人 and 二人目 moving under 人【にん】 (in 無職転生, 人【にん】 goes from 1,128 to 3,616 occurrences, the sum of the six).

### 11. Grammar suffixes after a verb stem (commit "Grammar suffixes after a verb stem belong to the verb")

UniDic makes suffixes of 方 (かた), 難い (がたい, にくい), 易い (やすい) and 辛い (づらい) after the 連用形 of a verb (戦い + 方, 使い + やすい), and they were listed as words of their own.

**Decided, with the user:**

- **難い, にくい, やすい and づらい always attach** to the verb before them: 使いやすい is 使う, "easy to (やすい)"; 書きにくかった is 書く, "hard to (にくい), past (た)". This holds even where JMdict has the whole (分かりやすい, 信じ難い, 言いにくい), which JMdict merging already left apart on purpose (`longest_headword` skips them as productive endings). They also attach after a passive or causative auxiliary (誤解され + やすい), and so does 辛い when UniDic takes it for the adjective (食べ辛い, read つらい).
- **方 attaches only when JMdict does not have the whole**: 戦い方 is 戦う, "way of (方)", but JMdict's 〜方 nouns stay whole, as merging already made them (言い方, 在り方, 考え方, やり方). Many have a meaning of their own (在り方, 見方), unlike the adjectives, which only add "easy" or "hard".

**Rejected:**

- **JMdict words stay whole for all of them**, as in decision 6: 分かりやすい would be a word while わかりやすい (kana, which merging does not look up) would be 分かる, and transparent compounds such as 信じ難い would hide the verb.
- **Always attach, 方 included**: 2,350 occurrences of JMdict 〜方 nouns in five books (言い方 502, 在り方 332) would lose their entry.

Counted in five books before the change: 方 1,044 occurrences without a JMdict word (戦い方 172, 喋り方 54), 難い 485, 易い 685 and 辛い 249 without one, and 800 more for the adjectives with one. On the six books, the only list changes are these suffixes leaving the lists (in Re:ゼロ, 難い loses 692 occurrences, やすい 487, づらい 282, 方 824) and the most used spelling of 方 becoming ほう in two books, since its kanji now mostly attaches. What remains of them is right: the adjective 易い (口で言うのは易い), nouns UniDic gives (揺らぎやすい), and 方 read ほう (行った方がいい).

### 12. っぽい and めく, and conjugated JMdict words made with a suffix (commit "っぽい and めく belong to their word")

**Decided:**

- **っぽい** after a noun (メイドっぽい is メイド, "-ish (っぽい)"), or after a verb, adjective or auxiliary in any form (助かるっぽい, 来たっぽい is 来る, "past (た), -ish (っぽい)").
- **めく and めかす** after a noun (儀式めいた is 儀式, "-like (めく), past (た)"; 冗談めかして) or after a verb's 連用形 used as a noun (脅しめいた is 脅す).
- **JMdict words made this way stay whole**, as decision 6 says, **also when conjugated**. JMdict merging looked a conjugated ending up in its base form only for verbs and adjectives, so it found 嘘っぽい but not 嘘っぽかった, 嬉しがる but not 嬉しがった. It now does the same for suffixes that conjugate (UniDic gives them a conjugation type): 嘘っぽかった is 嘘っぽい, "past (た)". A suffix is still tried as written first, as before, so no earlier merge changes: 強がり (the noun) stays 強がり rather than the verb 強がる, and 人たらし is not looked up as 人たらしい.

This reaches beyond っぽい and めく: every conjugated JMdict word made with a suffix is now found (嫌がる, 怖がる, 可愛がる, 男らしい, 胡散臭い, 耐えかねる, 芝居がかる, 大人ぶる, 恩着せがましい). In Re:ゼロ, 嫌がる goes from 41 to 141 occurrences and 怖がる from 9 to 67, taken from 嫌 and 怖い. The test of decision 6 that expected 寒がった as 寒い now expects 寒がる, and uses 眩しがった (not in JMdict) for the rule.

On the six books, besides those merges, っぽい leaves the lists (350 occurrences in 蜘蛛ですが, 285 in Re:ゼロ, 186 in 無職転生), and so do めく (249 in Re:ゼロ) and めかす. What remains of っぽい follows letters UniDic does not make words of (DQNっぽい) or a particle (だけっぽい).

**Kept as words:** 的 (入門的な, ~730), productive but with many JMdict words of its own. Kana-only JMdict words are not merged (merging needs a kanji), so それっぽい is それ, "-ish (っぽい)".

## What remains

Counts are occurrences in five of the six books (the 無職転生 PDF repeats the EPUB), scanned on 2026-11-15 with commit `abd6fcb`.

| # | Problem | Examples | Occurrences | Likely approach |
|---|---|---|---|---|
| 1 | **JMdict expressions split apart**: merging stops at particles and auxiliaries (to keep phrases such as 頭を下げる apart) and never starts at a pronoun | 何もかも; それなり; 相変わらず | 555 / 534 / 318 | Merge a run across a particle, auxiliary or pronoun start only when JMdict has the whole as an expression |
| 2 | **Kanji prefixes** are listed as words of their own | 大 (大銅貨), 不, 聖 | ~1,600 for 大 | Decide whether they attach like お, with the noun as the word |
| 3 | **さ after a merged JMdict word**: the derivation rule looks at the last token only (気味), not at the word (不気味) | 不気味さ | 200 | Look at the merged word's part of speech, or accept さ after any noun |
| 4 | **The insult suffix め** stays a fragment | リア充め, この色男め | 139 | Attach to the noun, "contempt (め)" |

### Kept as they are, on purpose

- **Name suffixes:** さん (~6,500), 様 (~6,900), くん, ちゃん, 殿, たん. Words learned once, listed separately.
- **Noun suffixes with a meaning:** 族, 教, 家, 国, 領, 者, 用, 内.
- **Interjections and stutters:** あ, え, う, ん, そ、そうです, わ、わかった. Fragments, never listed.

### UniDic limits a rule cannot fix

- **Sound words and laughs** are cut into pieces (ふよふよ, ふはは). They have no dictionary entries to look up.
- **Katakana names** are split into known words (ミリス → ミ + リス, グレイ + ラット).
- **Readings chosen out of context:** 三本 alone is read as the surname みもと, 千 alone as a person's name, the 七 of the name 七陰 as a prefix, 第一 as the adverb "first of all" (第一話 is then 第一 + 話 read はなし), and あの方 as ほう.
