#!/usr/bin/env bash
# Download the JMdict release pinned in jmdict.lock, check its SHA-256, and extract the headwords the
# tokenizer merges tokens into, into target/jmdict/headwords.tsv.
#
#   scripts/fetch-jmdict.sh            fetch the pinned release (nothing to do when already extracted)
#   scripts/fetch-jmdict.sh --update   pin the latest release in jmdict.lock, then fetch it
#
# Releases come from https://github.com/scriptin/jmdict-simplified, which publishes JMdict (EDRDG,
# CC BY-SA 4.0) as JSON every week and keeps the older releases, so a version can be pinned.
# Needs curl, tar and python3. Building the CLI runs this when the headwords are missing.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
lock="$root/jmdict.lock"
out="$root/target/jmdict"
repo=scriptin/jmdict-simplified

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }
url() { echo "https://github.com/$repo/releases/download/${1//+/%2B}/jmdict-eng-${1//+/%2B}.json.tgz"; }
lock_value() { sed -n "s/^$1=//p" "$lock"; }

mkdir -p "$out"
if [ "${1:-}" = "--update" ]; then
  previous=$(lock_value version 2>/dev/null || true)
  version=$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" | python3 -I -c 'import json, sys; print(json.load(sys.stdin)["tag_name"])')
  archive="$out/jmdict-eng-$version.json.tgz"
  [ -f "$archive" ] || curl -fsSL -o "$archive" "$(url "$version")"
  cat > "$lock" <<EOF
# JMdict release used to merge tokens into dictionary words (see scripts/fetch-jmdict.sh)
version=$version
sha256=$(sha256 "$archive")
EOF
  echo "jmdict.lock: ${previous:-none} -> $version"
fi

version=$(lock_value version)
expected=$(lock_value sha256)
headwords="$out/headwords.tsv"
if [ -f "$headwords" ] && [ "$(head -1 "$headwords")" = "# JMdict $version" ]; then
  exit 0
fi

archive="$out/jmdict-eng-$version.json.tgz"
if [ ! -f "$archive" ] || [ "$(sha256 "$archive")" != "$expected" ]; then
  echo "Downloading JMdict $version" >&2
  curl -fsSL -o "$archive.part" "$(url "$version")"
  actual=$(sha256 "$archive.part")
  if [ "$actual" != "$expected" ]; then
    rm -f "$archive.part"
    echo "JMdict $version: SHA-256 is $actual, jmdict.lock expects $expected" >&2
    exit 1
  fi
  mv "$archive.part" "$archive"
fi
tar -xzOf "$archive" | python3 -I "$root/scripts/jmdict-headwords.py" "$version" > "$headwords.part"
mv "$headwords.part" "$headwords"
# Keep only the pinned archive
find "$out" -name 'jmdict-eng-*.json.tgz' ! -name "$(basename "$archive")" -delete
