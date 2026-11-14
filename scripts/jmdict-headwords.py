#!/usr/bin/env python3
"""Read a jmdict-simplified JSON release on stdin and print the headwords the tokenizer uses, as TSV.

The first line is "# JMdict <version>"; the others, sorted by word, are: word, reading (hiragana), part of speech, kind.
- kind "k": a kanji headword, which tokens are merged into (神出鬼没, 魔法使い)
- kind "r": a kanji spelling JMdict marks as rarely used (遣る, 仕舞う), with the kana spelling to show instead

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


def main(version):
    data = json.load(sys.stdin)
    headwords, rare = {}, {}  # word -> (priority, reading, pos)
    for entry in data["words"]:
        codes = list(dict.fromkeys(c for s in entry["sense"] for c in s["partOfSpeech"]))
        pos = label(codes)
        kana = [k for k in entry["kana"] if not RARE_KANA & set(k["tags"])]
        kana.sort(key=lambda k: not k["common"])
        for kanji in entry["kanji"]:
            word = kanji["text"]
            reading = next((k["text"] for k in kana if "*" in k["appliesToKanji"] or word in k["appliesToKanji"]), None)
            if reading is None:
                continue
            # Common spellings first, so 今日 reads きょう rather than another entry's reading
            row = (0 if kanji["common"] else 1, reading, pos)
            target = rare if RARE & set(kanji["tags"]) else headwords
            if word not in target or row < target[word]:
                target[word] = row
    if len(headwords) < MIN_HEADWORDS:
        sys.exit(f"Only {len(headwords)} headwords in JMdict {version}: is the release broken?")
    rows = {word: (reading, pos, "r") for word, (_, reading, pos) in rare.items()}
    rows.update((word, (reading, pos, "k")) for word, (_, reading, pos) in headwords.items())
    print(f"# JMdict {version}")
    # Sorted by code point, which is the byte order of UTF-8: the tokenizer finds words by binary search
    for word, (reading, pos, kind) in sorted(rows.items()):
        print(f"{word}\t{reading}\t{pos}\t{kind}")
    print(f"JMdict {version}: {len(headwords)} headwords, {len(rare.keys() - headwords.keys())} rare spellings", file=sys.stderr)


if __name__ == "__main__":
    main(sys.argv[1])
