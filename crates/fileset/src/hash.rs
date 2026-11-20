//! BLAKE3 file hashing.

use std::path::Path;

/// Hashes the contents of the file at `abs` with BLAKE3.
///
/// The walk saw a regular file there. If it has been swapped since for a
/// symlink or a FIFO, this fails instead of reading another file or
/// waiting for a writer.
pub fn hash_file(abs: &Path) -> std::io::Result<[u8; 32]> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(abs)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
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

    /// A file swapped for a link or a FIFO after the walk saw it is an
    /// error: not another file's hash, and not a wait for a writer.
    #[test]
    fn hash_file_refuses_a_symlink_and_a_fifo() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::write(&real, b"hello").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(hash_file(&link).is_err());
        let fifo = dir.path().join("fifo");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `c` is a valid NUL-terminated path for the call.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let err = hash_file(&fifo).unwrap_err();
        assert!(err.to_string().contains("not a regular file"), "{err}");
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
