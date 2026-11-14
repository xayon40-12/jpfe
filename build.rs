//! With the `embed-dict` feature, makes sure the JMdict release pinned in `jmdict.lock` is extracted
//! (by `scripts/fetch-jmdict.sh`, which downloads it the first time), and points `JPFE_JMDICT_HEADWORDS`,
//! `JPFE_JMDICT_DEFINITIONS` and `JPFE_KANJIDIC` at the extracted files.

use std::path::Path;
use std::process::Command;

fn main() {
    for file in ["jmdict.lock", "scripts/fetch-jmdict.sh", "scripts/jmdict-headwords.py", "scripts/jmdict-definitions.py", "scripts/kanjidic.py"] {
        println!("cargo:rerun-if-changed={file}");
    }
    if std::env::var_os("CARGO_FEATURE_EMBED_DICT").is_none() {
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fetched = Command::new("bash").arg(root.join("scripts/fetch-jmdict.sh")).status().is_ok_and(|s| s.success());
    assert!(fetched, "Could not prepare JMdict: run scripts/fetch-jmdict.sh (it needs curl, tar and python3)");
    for (variable, file) in [
        ("JPFE_JMDICT_HEADWORDS", "headwords.tsv"),
        ("JPFE_JMDICT_DEFINITIONS", "definitions.tsv"),
        ("JPFE_KANJIDIC", "kanji.tsv"),
    ] {
        let path = root.join("target/jmdict").join(file);
        println!("cargo:rerun-if-changed={}", path.display());
        println!("cargo:rustc-env={variable}={}", path.display());
    }
}
