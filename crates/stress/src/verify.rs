//! Manifest comparison: the harness hashes the `.fileset.sqlite` files itself.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};

/// BLAKE3 of the file's contents, lowercase hex.
pub fn blake3_file(path: &Path) -> Result<String> {
    let mut f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut h = blake3::Hasher::new();
    std::io::copy(&mut f, &mut h).with_context(|| format!("hashing {}", path.display()))?;
    Ok(h.finalize().to_hex().to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestHashes {
    pub src: String,
    pub dst: String,
}

impl ManifestHashes {
    pub fn matches(&self) -> bool {
        self.src == self.dst
    }
}

/// Hashes `<src_data>/.fileset.sqlite` and `<dst_data>/.fileset.sqlite` (a
/// missing file is an error: the sync should have carried it) and writes
/// `report` as two lines `src <hex>\ndst <hex>\n`.
pub fn compare_manifests(
    src_data: &Path,
    dst_data: &Path,
    report: &Path,
) -> Result<ManifestHashes> {
    let src = blake3_file(&src_data.join(".fileset.sqlite"))?;
    let dst = blake3_file(&dst_data.join(".fileset.sqlite"))?;
    std::fs::write(report, format!("src {src}\ndst {dst}\n"))
        .with_context(|| format!("writing {}", report.display()))?;
    Ok(ManifestHashes { src, dst })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_and_different_manifests() {
        let t = tempfile::tempdir().unwrap();
        let (s, d) = (t.path().join("s"), t.path().join("d"));
        for p in [&s, &d] {
            std::fs::create_dir(p).unwrap();
            std::fs::write(p.join(".fileset.sqlite"), b"same").unwrap();
        }
        let h = compare_manifests(&s, &d, &t.path().join("r.blake3")).unwrap();
        assert!(h.matches());
        assert_eq!(
            std::fs::read_to_string(t.path().join("r.blake3")).unwrap(),
            format!("src {}\ndst {}\n", h.src, h.dst)
        );
        std::fs::write(d.join(".fileset.sqlite"), b"other").unwrap();
        assert!(!compare_manifests(&s, &d, &t.path().join("r2.blake3"))
            .unwrap()
            .matches());
        std::fs::remove_file(d.join(".fileset.sqlite")).unwrap();
        assert!(compare_manifests(&s, &d, &t.path().join("r3.blake3")).is_err());
    }
}
