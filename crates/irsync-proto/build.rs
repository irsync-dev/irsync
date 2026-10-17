use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from("../../proto/irsync/v1");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read proto dir")
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "proto"))
        .collect();
    files.sort();
    let fds = protox::compile(&files, ["../../proto"]).expect("compile protos");
    prost_build::Config::new()
        .compile_fds(fds)
        .expect("generate prost code");
    println!("cargo:rerun-if-changed=../../proto");
}
