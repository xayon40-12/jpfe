# Decisions and history

How the project got where it is: what was decided, why, what was tried and dropped, and what remains. Read in this order, which is the order the work was done:

1. [Extraction, statistics and the viewer](decisions-extraction-and-charts.md) (October to 14 November 2026): the CLI and its outputs, PDF chapters, the statistics and their charts, the viewer on the desktop and the web, browser storage, and the dictionary work up to JMdict merging.
2. [The reader and its dictionaries](decisions-reader.md) (14 November 2026): the reader, its word and kanji lookups and their JMdict and KANJIDIC data, vertical text, PDF paragraphs, and the rules for words JMdict lacks.
3. [Word grouping](word-grouping.md) (14 to 20 November 2026): conjugation endings, homographs, numbers and counters, derivations, plurals and prefixes, the viewer's storage fix, then the problems a scan of the books found (一人 and 二人, grammar and derivation suffixes, kanji prefixes, JMdict expressions, さ and め), the full-width space and the readings in context (found by the survey of the reader's colors), and **the UniDic limits that remain**.
4. [Cloud sync and word highlighting](plans-sync-and-highlighting.md) (18 November 2026, planned): syncing settings, reading places and books through the user's Dropbox (and why not the other clouds or iCloud yet), and highlighting the parts of every word in the reader.

When a decision is taken or an open item is resolved, the matching document is updated in the same change: the item moves from "What remains" to the decisions, with its commit.
