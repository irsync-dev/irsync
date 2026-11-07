use std::path::PathBuf;

fn main() {
    // The protos live at the workspace root, two levels above this crate.
    let root = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"),
    )
    .join("../../proto");
    let dir = root.join("irsync/v1");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read proto dir")
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "proto"))
        .collect();
    files.sort();
    let fds = protox::compile(&files, [&root]).expect("compile protos");
    prost_build::Config::new()
        .compile_fds(fds)
        .expect("generate prost code");
    println!("cargo:rerun-if-changed={}", root.display());
}
