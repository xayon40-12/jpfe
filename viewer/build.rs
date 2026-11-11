//! Sets `JPFE_BUILD_ID` to a hash of the code that produces the statistics, so cached
//! analyses are recomputed after any change to it (in practice: with every new web build).

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;

fn hash_dir(dir: &Path, hasher: &mut DefaultHasher) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            hash_dir(&path, hasher);
        } else if path.extension().is_some_and(|e| e == "rs") {
            path.file_name().hash(hasher);
            std::fs::read(&path).unwrap_or_default().hash(hasher);
        }
    }
}

fn main() {
    let mut hasher = DefaultHasher::new();
    for dir in ["src", "../src"] {
        println!("cargo:rerun-if-changed={dir}");
        hash_dir(Path::new(dir), &mut hasher);
    }
    // Dependency versions (lindera, the PDF and EPUB readers) affect the results too
    println!("cargo:rerun-if-changed=../Cargo.lock");
    std::fs::read("../Cargo.lock").unwrap_or_default().hash(&mut hasher);
    println!("cargo:rustc-env=JPFE_BUILD_ID={:016x}", hasher.finish());
}
