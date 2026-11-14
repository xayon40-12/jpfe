#!/usr/bin/env python3
"""Read a jmdict-simplified KANJIDIC2 JSON release on stdin and print the kanji the viewer's reader shows, as TSV.

The first line is "# KANJIDIC2 <version>"; the others, sorted by kanji, are: kanji, on readings, kun readings,
name readings (each 、-separated), meanings ("; "-separated), stroke count, school grade, old JLPT level and
newspaper frequency rank (empty when unknown).

Usage: tar -xzOf kanjidic2-en-<version>.json.tgz | kanjidic.py <version> > kanji.tsv
"""
import json
import sys

MIN_KANJI = 10_000  # a release with far fewer is broken


def clean(text):
    """No tabs or line breaks, which separate the fields."""
    return " ".join(str(text).split())


def main(version):
    data = json.load(sys.stdin)
    rows = {}
    for character in data["characters"]:
        groups = (character.get("readingMeaning") or {}).get("groups", [])
        readings = [r for g in groups for r in g["readings"]]
        on = [clean(r["value"]) for r in readings if r["type"] == "ja_on"]
        kun = [clean(r["value"]) for r in readings if r["type"] == "ja_kun"]
        nanori = [clean(n) for n in (character.get("readingMeaning") or {}).get("nanori", [])]
        meanings = [clean(m["value"]) for g in groups for m in g["meanings"] if m["lang"] == "en"]
        misc = character["misc"]
        numbers = [misc["strokeCounts"][0] if misc["strokeCounts"] else None, misc["grade"], misc["jlptLevel"], misc["frequency"]]
        rows[character["literal"]] = ["、".join(on), "、".join(kun), "、".join(nanori), "; ".join(meanings)] + [
            "" if n is None else str(n) for n in numbers
        ]
    if len(rows) < MIN_KANJI:
        sys.exit(f"Only {len(rows)} kanji in KANJIDIC2 {version}: is the release broken?")
    print(f"# KANJIDIC2 {version}")
    # Sorted by code point, which is the byte order of UTF-8: the viewer finds kanji by binary search
    for literal, fields in sorted(rows.items()):
        print("\t".join([literal] + fields))
    print(f"KANJIDIC2 {version}: {len(rows)} kanji", file=sys.stderr)


if __name__ == "__main__":
    main(sys.argv[1])
