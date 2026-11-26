//! The WTF-8 conversion is in three crates on purpose: fileset checks what
//! irsync writes and must not share its code, and filegen stands apart from
//! both. The three copies must stay the same code; only their comments
//! differ. This fails when one is changed without the others.

use std::path::Path;

/// The code of `crates/NAME/src/wtf8.rs`: comments and blank lines left
/// out. (No string in these files holds `//`.)
fn code(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(name)
        .join("src")
        .join("wtf8.rs");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.lines()
        .map(|l| l.split_once("//").map_or(l, |(code, _)| code).trim_end())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_three_wtf8_copies_are_the_same_code() {
    let fileset = code("fileset");
    assert!(fileset.contains("pub fn encode"), "{fileset}");
    for other in ["filegen", "irsync"] {
        assert!(
            code(other) == fileset,
            "crates/{other}/src/wtf8.rs differs from crates/fileset/src/wtf8.rs: \
             change all three copies together"
        );
    }
}
