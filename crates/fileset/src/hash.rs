//! BLAKE3 file hashing.

use std::path::Path;

/// Hashes the contents of the file at `abs` with BLAKE3.
pub fn hash_file(abs: &Path) -> std::io::Result<[u8; 32]> {
    let file = std::fs::File::open(abs)?;
    let hash = blake3::Hasher::new().update_reader(file)?.finalize();
    Ok(*hash.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn hash_file_matches_blake3_crate() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"hello").unwrap();
        assert_eq!(hash_file(&p).unwrap(), *blake3::hash(b"hello").as_bytes());
    }

    #[test]
    fn hash_file_unreadable_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"secret").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&p).is_ok() {
            eprintln!("skipping: running with privileges that bypass file permissions");
            return;
        }
        assert!(hash_file(&p).is_err());
    }
}
