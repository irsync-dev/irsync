use std::path::PathBuf;

use prost::Message;

fn main() {
    // The protos live at the workspace root, two levels above this crate.
    let root = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"),
    )
    .join("../../proto");
    let mut files: Vec<PathBuf> = ["irsync/v1", "irsync/capture/v1"]
        .iter()
        .flat_map(|sub| {
            std::fs::read_dir(root.join(sub))
                .expect("read proto dir")
                .map(|e| e.expect("dir entry").path())
                .filter(|p| p.extension().is_some_and(|e| e == "proto"))
        })
        .collect();
    files.sort();
    let fds = protox::compile(&files, [&root]).expect("compile protos");
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    std::fs::write(out_dir.join("descriptor.bin"), fds.encode_to_vec())
        .expect("write descriptor set");
    prost_build::Config::new()
        .compile_fds(fds)
        .expect("generate prost code");
    println!("cargo:rerun-if-changed={}", root.display());
}
