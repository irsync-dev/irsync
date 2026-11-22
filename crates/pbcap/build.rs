use std::path::PathBuf;

use prost::Message;

fn main() {
    let root = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"),
    )
    .join("proto");
    let files = [root.join("pbcap/v1/envelope.proto")];
    let fds = protox::compile(&files, [&root]).expect("compile protos");
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    std::fs::write(out_dir.join("descriptor.bin"), fds.encode_to_vec())
        .expect("write descriptor set");
    prost_build::Config::new()
        .compile_fds(fds)
        .expect("generate prost code");
    println!("cargo:rerun-if-changed={}", root.display());
}
