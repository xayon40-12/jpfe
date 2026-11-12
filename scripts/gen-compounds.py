#!/usr/bin/env python3
"""Generate src/compounds.csv: kanji compounds missing from UniDic, as a lindera user dictionary.

UniDic splits text into short units, so idioms and compounds such as 神出鬼没 or 図書館 come out as
pieces (神出 + 鬼没). This takes the kanji common nouns of 3 or more characters from IPADIC that UniDic
lacks and writes them as UniDic rows, so the tokenizer keeps them whole.

Compounds written with okurigana (魔法使い, 人見知り) are taken too, when their last kanji and its
okurigana form a word on their own (使い, 知り). Compounds ending with a verb stem whose okurigana was
dropped (魔法使, 手間取) are left out: they would cut 魔法使っ or 魔法使い in the wrong place.
Endings that are words spelled without okurigana on their own (具合 in 不具合) are kept.

Usage: gen-compounds.py <unidic lex.csv> <IPADIC directory> <output csv>
Both sources are downloaded by lindera's build scripts:
  target/release/build/lindera-unidic-*/out/unidic-mecab-2.1.2.tar.gz (lex.csv)
  target/release/build/lindera-ipadic-*/out/mecab-ipadic-2.7.0-20250920.tar.gz (Noun*.csv)
"""
import glob
import re
import sys

MIN_LENGTH = 3
COST = 5000
KANJI = re.compile(r"^[一-鿿][一-鿿々]+$")
OKURIGANA = re.compile(r"^[ぁ-ゖ]{1,2}$")
VOICED = str.maketrans("ガギグゲゴザジズゼゾダヂヅデドバビブベボパピプペポ", "カキクケコサシスセソタチツテトハヒフヘホハヒフヘホ")
HIRAGANA = "".join(chr(c) for c in range(0x3041, 0x3097))
# Compounds IPADIC lacks, whose pieces would otherwise be taken apart by another entry
# (近距離 would leave 至 alone in 至近距離): (word, IPADIC noun class, reading, pronunciation)
EXTRA = [("至近距離", "一般", "シキンキョリ", "シキンキョリ")]
# IPADIC noun class -> UniDic 普通名詞 subclass and its context ID
CLASSES = {"一般": ("一般", 5146), "サ変接続": ("サ変可能", 5133), "形容動詞語幹": ("形状詞可能", 5159), "副詞可能": ("副詞可能", 5150)}


def ends_with(pron, suffix):
    """`pron` ends with `suffix`, allowing rendaku on its first kana (取り トリ in 手間取り テマドリ)."""
    if not 0 < len(suffix) < len(pron) or not pron.endswith(suffix[1:]):
        return False
    return pron[-len(suffix)].translate(VOICED) == suffix[0].translate(VOICED)


def main(lex, ipadic, out):
    known = set()
    stems = {}  # kanji -> pronunciations of its verb 連用形 with okurigana: 使 -> {"ツカイ", "ツカッ", ...}
    for line in open(lex, encoding="utf-8"):
        f = line.rstrip("\n").split(",")
        known.add(f[0])
        if len(f) > 13 and f[4] == "動詞" and f[9].startswith("連用形") and KANJI.match(f[0][:1] + "々") and OKURIGANA.match(f[0][1:]):
            stems.setdefault(f[0][0], set()).add(f[13])
    rows = {}
    for word, cls, reading, pron in EXTRA:
        subclass, cid = CLASSES[cls]
        rows[word] = f"{word},{cid},{cid},{COST},名詞,普通名詞,{subclass},*,*,*,{reading},{word},{word},{pron},{word},{pron},漢,*,*,*,*"
    for path in sorted(glob.glob(f"{ipadic}/Noun*.csv")):
        for line in open(path, encoding="utf-8"):
            f = line.rstrip("\n").split(",")
            word = f[0]
            if f[4] != "名詞" or f[5] not in CLASSES or len(word) < MIN_LENGTH or word in known or word in rows:
                continue
            reading = f[11]
            pron = f[12] if len(f) > 12 else reading
            if KANJI.match(word):
                # (an ending spelled that way on its own, like 具合 in 不具合, is fine)
                if word[-2:] not in known and any(ends_with(pron, p) for p in stems.get(word[-1], ())):
                    continue
            else:
                kanji = word.rstrip(HIRAGANA)
                if not (KANJI.match(kanji) and OKURIGANA.match(word[len(kanji):]) and word[len(kanji) - 1:] in known):
                    continue
            subclass, cid = CLASSES[f[5]]
            rows[word] = f"{word},{cid},{cid},{COST},名詞,普通名詞,{subclass},*,*,*,{reading},{word},{word},{pron},{word},{pron},漢,*,*,*,*"
    with open(out, "w", encoding="utf-8") as o:
        o.write("\n".join(sorted(rows.values())) + "\n")
    print(f"{len(rows)} entries -> {out}", file=sys.stderr)


if __name__ == "__main__":
    main(*sys.argv[1:4])
