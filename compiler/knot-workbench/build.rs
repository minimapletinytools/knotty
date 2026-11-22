//! Fingerprint compiler inputs at build time: hashing the running executable on
//! every CLI invocation costs more than rebuilding these small libraries.
use std::{
    hash::{Hash, Hasher},
    path::Path,
};
fn collect(path: &Path, root: &Path, hash: &mut impl Hasher) {
    if path.is_dir() {
        if path
            .file_name()
            .is_some_and(|n| n == "target" || n == ".git")
        {
            return;
        }
        if matches!(
            path.file_name().and_then(|n| n.to_str()),
            Some("src" | "lib")
        ) {
            println!("cargo:rerun-if-changed={}", path.display());
        }
        let mut paths: Vec<_> = std::fs::read_dir(path)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        paths.sort();
        for p in paths {
            collect(&p, root, hash);
        }
    } else if path
        .extension()
        .is_some_and(|e| e == "rs" || e == "toml" || e == "lock" || e == "knot")
    {
        println!("cargo:rerun-if-changed={}", path.display());
        path.strip_prefix(root).unwrap().hash(hash);
        std::fs::read(path).unwrap().hash(hash);
    }
}
fn main() {
    let package = std::env::var_os("CARGO_MANIFEST_DIR").unwrap();
    let root = Path::new(&package).parent().unwrap();
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    "knot-core-cache-schema-v1".hash(&mut hash);
    collect(root, root, &mut hash);
    for name in [
        "PROFILE",
        "TARGET",
        "HOST",
        "CARGO_ENCODED_RUSTFLAGS",
        "OPT_LEVEL",
        "DEBUG",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
        std::env::var_os(name).hash(&mut hash);
    }
    let mut features: Vec<_> = std::env::vars()
        .filter(|(k, _)| k.starts_with("CARGO_FEATURE_"))
        .collect();
    features.sort();
    features.hash(&mut hash);
    let rustc = std::env::var_os("RUSTC").unwrap();
    let version = std::process::Command::new(rustc)
        .arg("--version")
        .output()
        .unwrap();
    version.stdout.hash(&mut hash);
    println!(
        "cargo:rustc-env=KNOT_CORE_CACHE_BUILD_ID={:016x}",
        hash.finish()
    );
}
