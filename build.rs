//! With the `embed-dict` feature, makes sure the JMdict release pinned in `jmdict.lock` is extracted
//! (by `scripts/fetch-jmdict.sh`, which downloads it the first time) and points `JPFE_JMDICT_HEADWORDS` at it.

use std::path::Path;
use std::process::Command;

fn main() {
    for file in ["jmdict.lock", "scripts/fetch-jmdict.sh", "scripts/jmdict-headwords.py"] {
        println!("cargo:rerun-if-changed={file}");
    }
    if std::env::var_os("CARGO_FEATURE_EMBED_DICT").is_none() {
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fetched = Command::new("bash").arg(root.join("scripts/fetch-jmdict.sh")).status().is_ok_and(|s| s.success());
    assert!(fetched, "Could not prepare the JMdict headwords: run scripts/fetch-jmdict.sh (it needs curl, tar and python3)");
    let headwords = root.join("target/jmdict/headwords.tsv");
    println!("cargo:rerun-if-changed={}", headwords.display());
    println!("cargo:rustc-env=JPFE_JMDICT_HEADWORDS={}", headwords.display());
}
