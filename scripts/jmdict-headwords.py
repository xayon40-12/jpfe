#!/usr/bin/env python3
"""Read a jmdict-simplified JSON release on stdin and print the headwords the tokenizer uses, as TSV.

The first line is "# JMdict <version>"; the others, sorted by word, are: word, reading (hiragana), part of speech,
kind, every reading of the word in any entry (・-separated; empty when the reading is the only one), and the readings
that are other words written the same (see `homographs`).
- kind "k": a kanji headword, which tokens are merged into (神出鬼没, 魔法使い)
- kind "r": a kanji spelling JMdict marks as rarely used (遣る, 仕舞う), with the kana spelling to show instead
- kind "a": a word of an entry written only in kana (いい, which is not a reading of 良い)

Usage: tar -xzOf jmdict-eng-<version>.json.tgz | jmdict-headwords.py <version> > headwords.tsv
"""
import json
import sys

RARE = {"rK", "sK", "oK", "iK"}
RARE_KANA = {"rk", "sk", "ok", "ik"}
MIN_HEADWORDS = 150_000  # a release with far fewer is broken


def label(codes):
    """The part of speech shown in the word lists, from JMdict's codes (expressions use their other code)."""
    code = next((c for c in codes if c != "exp"), "exp")
    if code.startswith("v"):
        return "動詞"
    return {"adj-i": "形容詞", "adj-ix": "形容詞", "adv": "副詞", "adv-to": "副詞", "int": "感動詞", "exp": "表現"}.get(code, "名詞")


def homographs(entries, main):
    """Readings of other words written the same as the word of entry `main` (よう and ざま for 様, whose usual
    reading さま is another entry): those of entries that share no gloss with it. Entries that do are the same
    word read another way (JMdict has わたし and わたくし, なに and なん, in entries of their own)."""
    main_readings, main_glosses = entries[main]
    others = []
    for i, (readings, glosses) in enumerate(entries):
        if i != main and not glosses & main_glosses:
            others.extend(r for r in readings if r not in main_readings and r not in others)
    return others


def main(version):
    data = json.load(sys.stdin)
    headwords, rare, kana_words = {}, {}, {}  # word -> (priority, reading, pos)
    readings = {}  # kanji word -> its readings in every entry
    entries = {}  # kanji word -> (readings, glosses) of each of its entries
    main_entry = {}  # kanji word -> index in entries[word] of the entry its row comes from
    for entry in data["words"]:
        codes = list(dict.fromkeys(c for s in entry["sense"] for c in s["partOfSpeech"]))
        pos = label(codes)
        kana = [k for k in entry["kana"] if not RARE_KANA & set(k["tags"])]
        kana.sort(key=lambda k: not k["common"])
        if not entry["kanji"]:
            for k in kana:
                row = (0 if k["common"] else 1, k["text"], pos)
                if k["text"] not in kana_words or row < kana_words[k["text"]]:
                    kana_words[k["text"]] = row
        for kanji in entry["kanji"]:
            word = kanji["text"]
            applying = [k["text"] for k in kana if "*" in k["appliesToKanji"] or word in k["appliesToKanji"]]
            known = readings.setdefault(word, [])
            known.extend(r for r in applying if r not in known)
            reading = next(iter(applying), None)
            if reading is None:
                continue
            word_entries = entries.setdefault(word, [])
            word_entries.append((applying, {g["text"] for s in entry["sense"] for g in s["gloss"]}))
            # Common spellings first, so 今日 reads きょう rather than another entry's reading
            row = (0 if kanji["common"] else 1, reading, pos)
            target = rare if RARE & set(kanji["tags"]) else headwords
            if word not in target or row < target[word]:
                target[word] = row
                main_entry[(word, target is rare)] = len(word_entries) - 1
    if len(headwords) < MIN_HEADWORDS:
        sys.exit(f"Only {len(headwords)} headwords in JMdict {version}: is the release broken?")
    rows = {word: (reading, pos, "a") for word, (_, reading, pos) in kana_words.items()}
    rows.update((word, (reading, pos, "r")) for word, (_, reading, pos) in rare.items())
    rows.update((word, (reading, pos, "k")) for word, (_, reading, pos) in headwords.items())
    print(f"# JMdict {version}")
    # Sorted by code point, which is the byte order of UTF-8: the tokenizer finds words by binary search
    for word, (reading, pos, kind) in sorted(rows.items()):
        # Other readings, only when there are some
        others = readings.get(word, [reading])
        main = main_entry.get((word, kind == "r"))
        other_words = homographs(entries[word], main) if main is not None else []
        print(f"{word}\t{reading}\t{pos}\t{kind}\t{'・'.join(others) if others != [reading] else ''}\t{'・'.join(other_words)}")
    print(
        f"JMdict {version}: {len(headwords)} headwords, {len(rare.keys() - headwords.keys())} rare spellings, "
        f"{len(rows) - len(headwords.keys() | rare.keys())} kana words",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main(sys.argv[1])
