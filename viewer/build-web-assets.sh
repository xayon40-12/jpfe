#!/usr/bin/env bash
# Prepare viewer/assets/ for the web build:
# - the compiled UniDic dictionary, zstd-compressed (about 200 MB raw, ~35 MB compressed),
#   with its BSD license
# - Noto Sans JP, since browsers do not let the page read system fonts
set -euo pipefail
cd "$(dirname "$0")/.."

# Building the CLI compiles the dictionary into lindera-unidic's build directory
cargo build --release -p jpfe
dict=$(dirname "$(ls -t target/release/build/lindera-unidic-*/out/lindera-unidic/dict.words | head -1)")

out=viewer/assets
mkdir -p "$out/dict"
for f in metadata.json char_def.bin matrix.mtx dict.trie dict.valsidx dict.vals dict.wordsidx dict.words unk.bin; do
  zstd -19 -q -f "$dict/$f" -o "$out/dict/$f.zst"
done
tar -xzOf "$dict/../unidic-mecab-2.1.2.tar.gz" unidic-mecab-2.1.2/BSD > "$out/dict/UNIDIC-LICENSE.txt"
# The viewer keeps the unpacked dictionary in browser storage and downloads it again only when this changes
cat "$out"/dict/*.zst | { command -v sha256sum >/dev/null && sha256sum || shasum -a 256; } | cut -c1-16 > "$out/dict/version.txt"

font=https://github.com/notofonts/noto-cjk/raw/main/Sans/SubsetOTF/JP/NotoSansJP-Regular.otf
[ -f "$out/NotoSansJP-Regular.otf" ] || curl -fsSL -o "$out/NotoSansJP-Regular.otf" "$font"
du -sh "$out"/dict/* "$out"/*.otf
