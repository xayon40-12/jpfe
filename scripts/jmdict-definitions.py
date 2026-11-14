#!/usr/bin/env python3
"""Read a jmdict-simplified JSON release on stdin and print the definitions the viewer's reader shows, as TSV.

Lines, after the first one ("# JMdict <version>"):
- "#tags", then for each part-of-speech or usage tag: "<code>=<description>", tab-separated
- one line per entry, numbered from 0: spellings (・-separated), readings (・-separated), then one field per
  sense: "<tags, comma-separated>|<glosses, separated by "; ">"
- "#index"
- one line per spelling or reading, sorted by it: the word, then its entries (comma-separated, the entries where
  it is a common word first)

Usage: tar -xzOf jmdict-eng-<version>.json.tgz | jmdict-definitions.py <version> > definitions.tsv
"""
import json
import re
import sys

SEARCH_ONLY = {"sK", "sk"}
MIN_ENTRIES = 150_000  # a release with far fewer is broken


def clean(text):
    """No tabs or line breaks, which separate the fields."""
    return " ".join(text.split())


def main(version):
    data = json.load(sys.stdin)
    entries, index, used_tags = [], {}, set()
    for number, entry in enumerate(data["words"]):
        kanji = [k for k in entry["kanji"] if not SEARCH_ONLY & set(k["tags"])]
        kana = [k for k in entry["kana"] if not SEARCH_ONLY & set(k["tags"])]
        senses = []
        for sense in entry["sense"]:
            tags = list(dict.fromkeys(sense["partOfSpeech"] + sense["misc"] + sense["field"] + sense["dialect"]))
            used_tags.update(tags)
            glosses = "; ".join(clean(g["text"]) for g in sense["gloss"])
            if glosses:
                senses.append(",".join(tags) + "|" + glosses)
        entries.append("\t".join(["・".join(k["text"] for k in kanji), "・".join(k["text"] for k in kana)] + senses))
        for word in entry["kanji"] + entry["kana"]:
            index.setdefault(word["text"], []).append((not word["common"], number))
    if len(entries) < MIN_ENTRIES:
        sys.exit(f"Only {len(entries)} entries in JMdict {version}: is the release broken?")
    print(f"# JMdict {version}")
    # Descriptions without their asides: "noun (common) (futsuumeishi)" -> "noun"
    tags = {code: re.sub(r"\s*\([^)]*\)", "", clean(text)) or clean(text) for code, text in data["tags"].items()}
    print("\t".join(["#tags"] + [f"{code}={tags.get(code, code)}" for code in sorted(used_tags)]))
    for line in entries:
        print(line)
    print("#index")
    # Sorted by code point, which is the byte order of UTF-8: the viewer finds words by binary search
    for word, numbers in sorted(index.items()):
        print(f"{word}\t{','.join(str(n) for _, n in sorted(set(numbers)))}")
    print(f"JMdict {version}: {len(entries)} entries, {len(index)} words", file=sys.stderr)


if __name__ == "__main__":
    main(sys.argv[1])
