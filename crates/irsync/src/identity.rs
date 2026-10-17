//! Node identities derived from OpenSSH Ed25519 keys.

use iroh::{EndpointId, SecretKey};
use rand_core::OsRng;
use ssh_key::{Algorithm, LineEnding, PrivateKey};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SSH key error: {0}")]
    Ssh(#[from] ssh_key::Error),
    #[error("private key is encrypted; a passphrase is required")]
    PassphraseRequired,
    #[error("only Ed25519 keys can be used as irsync identities")]
    NotEd25519,
    #[error("refusing to overwrite existing file {0}")]
    AlreadyExists(PathBuf),
    #[error("invalid endpoint id: {0}")]
    BadEndpointId(String),
}

fn secret_of(key: &PrivateKey) -> Result<SecretKey, IdentityError> {
    let kp = key.key_data().ed25519().ok_or(IdentityError::NotEd25519)?;
    Ok(SecretKey::from_bytes(&kp.private.to_bytes()))
}

/// Load an OpenSSH Ed25519 private key as an iroh secret key.
pub fn load_secret_key(path: &Path, passphrase: Option<&str>) -> Result<SecretKey, IdentityError> {
    let mut key = PrivateKey::read_openssh_file(path)?;
    if key.is_encrypted() {
        let pass = passphrase.ok_or(IdentityError::PassphraseRequired)?;
        key = key.decrypt(pass)?;
    }
    secret_of(&key)
}

/// Generate a new Ed25519 key, writing `path` (mode 0600) and `path.pub`.
pub fn generate_openssh(path: &Path, comment: &str) -> Result<SecretKey, IdentityError> {
    let mut pub_path = path.as_os_str().to_owned();
    pub_path.push(".pub");
    let pub_path = PathBuf::from(pub_path);
    for p in [path, pub_path.as_path()] {
        if p.symlink_metadata().is_ok() {
            return Err(IdentityError::AlreadyExists(p.to_path_buf()));
        }
    }
    let mut key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519)?;
    key.set_comment(comment);
    key.write_openssh_file(path, LineEnding::LF)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    key.public_key().write_openssh_file(&pub_path)?;
    secret_of(&key)
}

/// The iroh endpoint id for an SSH public key, if it is Ed25519.
pub fn endpoint_id_of_public_key(pk: &ssh_key::PublicKey) -> Option<EndpointId> {
    let ed = pk.key_data().ed25519()?;
    EndpointId::from_bytes(&ed.0).ok()
}

/// Parse an endpoint id in any format iroh prints (hex) or z-base-32.
pub fn parse_endpoint_id(s: &str) -> Result<EndpointId, IdentityError> {
    let s = s.trim();
    EndpointId::from_str(s)
        .or_else(|_| EndpointId::from_z32(s))
        .map_err(|_| IdentityError::BadEndpointId(s.to_string()))
}
