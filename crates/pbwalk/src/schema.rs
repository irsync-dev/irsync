//! The message definitions: irsync's, built in, or read from `.proto`
//! files at startup.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context};
use prost_reflect::{DescriptorPool, MessageDescriptor};

/// A set of message definitions.
pub struct Schema {
    pool: DescriptorPool,
}

impl Schema {
    /// irsync's protos, from `irsync_proto::FILE_DESCRIPTOR_SET`.
    pub fn builtin() -> Schema {
        // The set is built from the repo's protos at compile time, and a
        // test decodes it, so this cannot fail at run time.
        let pool = DescriptorPool::decode(irsync_proto::FILE_DESCRIPTOR_SET)
            .expect("the built-in descriptor set decodes");
        Schema { pool }
    }

    /// Every `.proto` file under each directory, each directory an include root.
    pub fn from_dirs(dirs: &[PathBuf]) -> anyhow::Result<Schema> {
        let mut files = Vec::new();
        for dir in dirs {
            let before = files.len();
            find_protos(dir, &mut files)?;
            if files.len() == before {
                bail!("{}: no .proto files", dir.display());
            }
        }
        let fds = protox::compile(&files, dirs).map_err(|e| anyhow!("{e:?}"))?;
        let pool = DescriptorPool::from_file_descriptor_set(fds)
            .context("the .proto files do not form a valid set")?;
        Ok(Schema { pool })
    }

    /// The message type with this full name, such as `irsync.v1.Hello`.
    pub fn message(&self, full_name: &str) -> anyhow::Result<MessageDescriptor> {
        self.pool
            .get_message_by_name(full_name)
            .ok_or_else(|| anyhow!("no message type named {full_name}"))
    }
}

/// Every `.proto` file under `dir`, in a stable order.
fn find_protos(dir: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let mut entries = std::fs::read_dir(dir)
        .with_context(|| format!("{}: cannot read the directory", dir.display()))?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("{}: cannot read the directory", dir.display()))?;
    entries.sort();
    for path in entries {
        if path.is_dir() {
            find_protos(&path, files)?;
        } else if path.extension().is_some_and(|x| x == "proto") {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_has_the_record() {
        let record = Schema::builtin()
            .message("irsync.capture.v1.Record")
            .unwrap();
        assert_eq!(record.full_name(), "irsync.capture.v1.Record");
    }

    #[test]
    fn from_dirs_loads_the_repo_protos() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../proto");
        let schema = Schema::from_dirs(&[dir]).unwrap();
        assert!(schema.message("irsync.v1.Hello").is_ok());
    }

    #[test]
    fn unknown_type_names_the_type() {
        let err = Schema::builtin().message("no.such.Type").unwrap_err();
        assert!(err.to_string().contains("no.such.Type"), "{err}");
    }

    #[test]
    fn empty_dir_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = Schema::from_dirs(&[dir.path().to_path_buf()])
            .err()
            .unwrap();
        let text = format!("{err:#}");
        assert!(text.contains(&dir.path().display().to_string()), "{text}");
        assert!(text.contains("no .proto files"), "{text}");
    }

    #[test]
    fn syntax_error_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("bad.proto"),
            "syntax = \"proto3\";\nmessage {\n",
        )
        .unwrap();
        let err = Schema::from_dirs(&[dir.path().to_path_buf()])
            .err()
            .unwrap();
        let text = format!("{err:#}");
        assert!(text.contains("bad.proto"), "{text}");
    }
}
