#!/usr/bin/env bash
# Download the JMdict and KANJIDIC2 release pinned in jmdict.lock, check their SHA-256, and extract into target/jmdict/:
# - headwords.tsv: the headwords the tokenizer merges tokens into
# - definitions.tsv: the definitions of every word, for the viewer's reader
# - kanji.tsv: readings and meanings of every kanji, for the viewer's reader
#
#   scripts/fetch-jmdict.sh            fetch the pinned release (nothing to do when already extracted)
#   scripts/fetch-jmdict.sh --update   pin the latest release in jmdict.lock, then fetch it
#
# Releases come from https://github.com/scriptin/jmdict-simplified, which publishes JMdict and KANJIDIC2 (EDRDG,
# CC BY-SA 4.0) as JSON every week and keeps the older releases, so a version can be pinned.
# Needs curl, tar and python3. Building the CLI runs this when the headwords are missing.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
lock="$root/jmdict.lock"
out="$root/target/jmdict"
repo=scriptin/jmdict-simplified

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }
# url <version> <file prefix>
url() { echo "https://github.com/$repo/releases/download/${1//+/%2B}/$2-${1//+/%2B}.json.tgz"; }
lock_value() { sed -n "s/^$1=//p" "$lock"; }

mkdir -p "$out"
if [ "${1:-}" = "--update" ]; then
  previous=$(lock_value version 2>/dev/null || true)
  version=$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" | python3 -I -c 'import json, sys; print(json.load(sys.stdin)["tag_name"])')
  for prefix in jmdict-eng kanjidic2-en; do
    [ -f "$out/$prefix-$version.json.tgz" ] || curl -fsSL -o "$out/$prefix-$version.json.tgz" "$(url "$version" "$prefix")"
  done
  cat > "$lock" <<EOF
# JMdict release used to merge tokens into dictionary words, and for the reader's definitions and kanji
# (see scripts/fetch-jmdict.sh)
version=$version
sha256=$(sha256 "$out/jmdict-eng-$version.json.tgz")
kanjidic_sha256=$(sha256 "$out/kanjidic2-en-$version.json.tgz")
EOF
  echo "jmdict.lock: ${previous:-none} -> $version"
fi

version=$(lock_value version)
# Extracted files are only made again for another release, or when the script making them changed
up_to_date() { [ -f "$1" ] && [ "$(head -1 "$1")" = "# $2 $version" ] && [ "$1" -nt "$root/scripts/$3" ]; }
if up_to_date "$out/headwords.tsv" JMdict jmdict-headwords.py && up_to_date "$out/definitions.tsv" JMdict jmdict-definitions.py \
  && up_to_date "$out/kanji.tsv" KANJIDIC2 kanjidic.py; then
  exit 0
fi

# fetch <file prefix> <expected SHA-256>: the archive's path, downloaded and checked
fetch() {
  local archive="$out/$1-$version.json.tgz"
  if [ ! -f "$archive" ] || [ "$(sha256 "$archive")" != "$2" ]; then
    echo "Downloading $1 $version" >&2
    curl -fsSL -o "$archive.part" "$(url "$version" "$1")"
    local actual
    actual=$(sha256 "$archive.part")
    if [ "$actual" != "$2" ]; then
      rm -f "$archive.part"
      echo "$1 $version: SHA-256 is $actual, jmdict.lock expects $2" >&2
      exit 1
    fi
    mv "$archive.part" "$archive"
  fi
  # Keep only the pinned archive
  find "$out" -name "$1-*.json.tgz" ! -name "$(basename "$archive")" -delete
  echo "$archive"
}
# extract <archive> <script> <output>
extract() {
  tar -xzOf "$1" | python3 -I "$root/scripts/$2" "$version" > "$out/$3.part"
  mv "$out/$3.part" "$out/$3"
}

jmdict=$(fetch jmdict-eng "$(lock_value sha256)")
kanjidic=$(fetch kanjidic2-en "$(lock_value kanjidic_sha256)")
extract "$jmdict" jmdict-headwords.py headwords.tsv
extract "$jmdict" jmdict-definitions.py definitions.tsv
extract "$kanjidic" kanjidic.py kanji.tsv
