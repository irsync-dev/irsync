//! Minisign signature check, with the irsync release keys built in.

use minisign_verify::{PublicKey, Signature};
use std::io::Read;
use std::path::{Path, PathBuf};

/// What a good signature told us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    /// The key that signed, as uppercase hex.
    pub key_id: String,
    pub trusted_comment: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SignatureError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("malformed signature or key: {0}")]
    Malformed(String),
    #[error("signed by key {key_id}, which is not one of the trusted keys")]
    UnknownKey { key_id: String },
    #[error("the signature does not match the file")]
    Bad,
    #[error("the signature is for \"{got}\", not \"{want}\"")]
    CommentMismatch { want: String, got: String },
    #[error("the signature is in the old format, which irsync does not accept")]
    Legacy,
}

/// The keys that sign irsync releases. They live in
/// `packaging/signing/release-keys.pub` and are built into the binary.
pub fn release_keys() -> Vec<PublicKey> {
    parse_keys(include_str!("../../../packaging/signing/release-keys.pub"))
        .expect("packaging/signing/release-keys.pub is not a valid key file")
}

/// Reads one or more minisign public key files, one after the other.
/// Every line must be empty, an `untrusted comment:` line or a key.
pub fn parse_keys(text: &str) -> Result<Vec<PublicKey>, SignatureError> {
    let mut keys = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("untrusted comment:") {
            continue;
        }
        let key = PublicKey::from_base64(line)
            .map_err(|e| SignatureError::Malformed(format!("public key: {e}")))?;
        keys.push(key);
    }
    Ok(keys)
}

/// Checks that `sig` is a good signature of `file` by one of `keys`.
/// With `trusted_comment`, the signature must carry exactly that comment.
/// Only the newer, prehashed signatures are accepted.
pub fn verify_file(
    file: &Path,
    sig: &Path,
    keys: &[PublicKey],
    trusted_comment: Option<&str>,
) -> Result<Verified, SignatureError> {
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| SignatureError::Io { path, source }
    };
    let sig_text = std::fs::read_to_string(sig).map_err(io(sig))?;
    let signature = Signature::decode(&sig_text)
        .map_err(|e| SignatureError::Malformed(format!("signature: {e}")))?;
    // minisign-verify does not expose key ids, so read it from the text.
    let key_id = signature_key_id(&sig_text)
        .ok_or_else(|| SignatureError::Malformed("signature: bad key id".into()))?;

    let mut verifier = None;
    for key in keys {
        match key.verify_stream(&signature) {
            Ok(v) => {
                verifier = Some(v);
                break;
            }
            Err(minisign_verify::Error::UnexpectedKeyId) => {}
            Err(minisign_verify::Error::UnsupportedLegacyMode) => {
                return Err(SignatureError::Legacy);
            }
            Err(e) => return Err(SignatureError::Malformed(e.to_string())),
        }
    }
    let mut verifier = verifier.ok_or(SignatureError::UnknownKey {
        key_id: key_id.clone(),
    })?;

    let mut f = std::fs::File::open(file).map_err(io(file))?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(io(file))?;
        if n == 0 {
            break;
        }
        verifier.update(&buf[..n]);
    }
    verifier.finalize().map_err(|_| SignatureError::Bad)?;

    let got = signature.trusted_comment();
    if let Some(want) = trusted_comment {
        if want != got {
            return Err(SignatureError::CommentMismatch {
                want: want.to_string(),
                got: got.to_string(),
            });
        }
    }
    Ok(Verified {
        key_id,
        trusted_comment: got.to_string(),
    })
}

/// The key id in a signature file, as uppercase hex. The id is bytes 2 to 9
/// of the decoded second line, so only the first 16 base64 characters matter.
fn signature_key_id(sig_text: &str) -> Option<String> {
    let line = sig_text.lines().nth(1)?.as_bytes();
    let mut bytes = Vec::with_capacity(12);
    for chunk in line.get(..16)?.chunks(4) {
        let mut n = 0u32;
        for &c in chunk {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => return None,
            };
            n = (n << 6) | u32::from(v);
        }
        bytes.extend_from_slice(&n.to_be_bytes()[1..]);
    }
    // The key id is stored little-endian; minisign prints it reversed.
    let id = bytes.get(2..10)?;
    Some(id.iter().rev().map(|b| format!("{b:02X}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/minisign")
            .join(name)
    }

    fn key(name: &str) -> PublicKey {
        let text = std::fs::read_to_string(fx(name)).unwrap();
        parse_keys(&text).unwrap().remove(0)
    }

    fn check(
        sig: &str,
        keys: &[PublicKey],
        comment: Option<&str>,
    ) -> Result<Verified, SignatureError> {
        verify_file(&fx("data.txt"), &fx(sig), keys, comment)
    }

    #[test]
    fn accepts_good_signature() {
        let v = check("data.txt.a.minisig", &[key("a.pub")], Some("irsync v1.2.3")).unwrap();
        assert_eq!(v.key_id, "C1DD76094F15E1FD");
        assert_eq!(v.trusted_comment, "irsync v1.2.3");
    }

    #[test]
    fn accepts_any_comment_when_none_is_wanted() {
        let v = check("data.txt.old.minisig", &[key("a.pub")], None).unwrap();
        assert_eq!(v.trusted_comment, "irsync v1.2.2");
    }

    #[test]
    fn accepts_second_key() {
        let keys = [key("a.pub"), key("b.pub")];
        let v = check("data.txt.b.minisig", &keys, Some("irsync v1.2.3")).unwrap();
        assert_eq!(v.key_id, "2AED585271522910");
    }

    #[test]
    fn rejects_unknown_key() {
        let err = check("data.txt.b.minisig", &[key("a.pub")], None).unwrap_err();
        match err {
            SignatureError::UnknownKey { key_id } => assert_eq!(key_id, "2AED585271522910"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn rejects_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        let changed = dir.path().join("data.txt");
        std::fs::write(&changed, "something else\n").unwrap();
        let err =
            verify_file(&changed, &fx("data.txt.a.minisig"), &[key("a.pub")], None).unwrap_err();
        assert!(matches!(err, SignatureError::Bad), "{err:?}");
    }

    #[test]
    fn rejects_wrong_trusted_comment() {
        let err = check(
            "data.txt.old.minisig",
            &[key("a.pub")],
            Some("irsync v1.2.3"),
        )
        .unwrap_err();
        match err {
            SignatureError::CommentMismatch { want, got } => {
                assert_eq!(want, "irsync v1.2.3");
                assert_eq!(got, "irsync v1.2.2");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn rejects_legacy_signature() {
        let err = check("data.txt.legacy.minisig", &[key("a.pub")], None).unwrap_err();
        assert!(matches!(err, SignatureError::Legacy), "{err:?}");
    }

    #[test]
    fn rejects_corrupt_signature() {
        let err = check("data.txt.corrupt.minisig", &[key("a.pub")], None).unwrap_err();
        assert!(matches!(err, SignatureError::Malformed(_)), "{err:?}");
    }

    #[test]
    fn missing_signature_file_is_io_error() {
        let err = check("no-such.minisig", &[key("a.pub")], None).unwrap_err();
        assert!(matches!(err, SignatureError::Io { .. }), "{err:?}");
    }

    #[test]
    fn parse_keys_skips_comments_and_reads_two_keys() {
        let text = format!(
            "{}\n{}",
            std::fs::read_to_string(fx("a.pub")).unwrap(),
            std::fs::read_to_string(fx("b.pub")).unwrap()
        );
        assert_eq!(parse_keys(&text).unwrap().len(), 2);
        assert!(parse_keys("untrusted comment: x\nnot a key\n").is_err());
    }

    #[test]
    fn release_keys_is_not_empty() {
        assert!(!release_keys().is_empty());
    }
}
