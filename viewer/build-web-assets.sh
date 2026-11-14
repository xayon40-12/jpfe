#!/usr/bin/env bash
# Prepare viewer/assets/ for the web build:
# - the compiled UniDic dictionary, zstd-compressed (about 200 MB raw, ~35 MB compressed),
#   with its BSD license
# - from the JMdict and KANJIDIC2 release pinned in jmdict.lock, with their CC BY-SA 4.0 attribution:
#   the headwords (9 MB raw, ~2 MB compressed), the reader's definitions (28 MB, ~8 MB) and kanji (0.7 MB, ~0.2 MB)
# - Noto Sans JP, since browsers do not let the page read system fonts
set -euo pipefail
cd "$(dirname "$0")/.."

# Building the CLI compiles the dictionary into lindera-unidic's build directory and extracts JMdict
cargo build --release -p jpfe
# The newest build of it, wherever cargo keeps build script outputs: lindera-unidic-<hash>/out before
# cargo 1.100, lindera-unidic/<hash>/out with its new build directory layout
dict_words=""
while IFS= read -r f; do
  if [ -z "$dict_words" ] || [ "$f" -nt "$dict_words" ]; then dict_words=$f; fi
done < <(find target/release/build -path '*lindera-unidic*/out/lindera-unidic/dict.words' 2>/dev/null)
if [ -z "$dict_words" ]; then
  echo "The compiled UniDic dictionary is missing from target/release/build/" >&2
  exit 1
fi
dict=$(dirname "$dict_words")
./scripts/fetch-jmdict.sh

out=viewer/assets
mkdir -p "$out/dict"
for f in metadata.json char_def.bin matrix.mtx dict.trie dict.valsidx dict.vals dict.wordsidx dict.words unk.bin; do
  zstd -19 -q -f "$dict/$f" -o "$out/dict/$f.zst"
done
tar -xzOf "$dict/../unidic-mecab-2.1.2.tar.gz" unidic-mecab-2.1.2/BSD > "$out/dict/UNIDIC-LICENSE.txt"
zstd -19 -q -f target/jmdict/headwords.tsv -o "$out/dict/jmdict.tsv.zst"
zstd -19 -q -f target/jmdict/definitions.tsv -o "$out/dict/definitions.tsv.zst"
zstd -19 -q -f target/jmdict/kanji.tsv -o "$out/dict/kanji.tsv.zst"
jmdict_version=$(sed -n 's/^version=//p' jmdict.lock)
printf '%s\n' \
  "jmdict.tsv.zst and definitions.tsv.zst are derived from JMdict, and kanji.tsv.zst from KANJIDIC2" \
  "(release $jmdict_version of https://github.com/scriptin/jmdict-simplified)," \
  "the property of the Electronic Dictionary Research and Development Group (https://www.edrdg.org/)," \
  "used in conformance with the Group's licence (https://www.edrdg.org/edrdg/licence.html):" \
  "Creative Commons Attribution-ShareAlike 4.0 International (https://creativecommons.org/licenses/by-sa/4.0/)." \
  > "$out/dict/JMDICT-LICENSE.txt"
# The viewer keeps the dictionary in browser storage and downloads it again only when this changes
cat "$out"/dict/*.zst | { command -v sha256sum >/dev/null && sha256sum || shasum -a 256; } | cut -c1-16 > "$out/dict/version.txt"

font=https://github.com/notofonts/noto-cjk/raw/main/Sans/SubsetOTF/JP/NotoSansJP-Regular.otf
[ -f "$out/NotoSansJP-Regular.otf" ] || curl -fsSL -o "$out/NotoSansJP-Regular.otf" "$font"
du -sh "$out"/dict/* "$out"/*.otf
