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
    #[error("{}: {source}", path.display())]
    File {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("SSH key error: {0}")]
    Ssh(#[from] ssh_key::Error),
    #[error("private key is encrypted; a passphrase is required")]
    PassphraseRequired,
    #[error(
        "private key {0} is encrypted and no passphrase is available: set IRSYNC_PASSPHRASE, \
         use --passphrase-file, or run from a terminal"
    )]
    NoPassphraseSource(PathBuf),
    #[error("wrong passphrase for {0}")]
    BadPassphrase(PathBuf),
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

/// Where the passphrase for an encrypted identity may come from, in order:
/// a file, the environment, then an interactive prompt.
#[derive(Debug, Clone, Default)]
pub struct PassphraseSource {
    /// `--passphrase-file`: the first line of this file.
    pub file: Option<PathBuf>,
    /// `IRSYNC_PASSPHRASE`.
    pub env: Option<String>,
    /// Prompt (without echo) on the terminal; only when stdin is a tty.
    pub prompt: bool,
}

impl PassphraseSource {
    /// `--passphrase-file`, `IRSYNC_PASSPHRASE`, and a prompt if stdin is a terminal.
    pub fn from_env(file: Option<PathBuf>) -> PassphraseSource {
        use std::io::IsTerminal;
        PassphraseSource {
            file,
            env: std::env::var("IRSYNC_PASSPHRASE").ok(),
            prompt: std::io::stdin().is_terminal(),
        }
    }

    fn passphrase(&self, key: &Path) -> Result<String, IdentityError> {
        if let Some(f) = &self.file {
            let text = std::fs::read_to_string(f)?;
            let line = text.lines().next().unwrap_or("");
            return Ok(line.to_string());
        }
        if let Some(p) = &self.env {
            return Ok(p.clone());
        }
        if self.prompt {
            return Ok(rpassword::prompt_password(format!(
                "Enter passphrase for {}: ",
                key.display()
            ))?);
        }
        Err(IdentityError::NoPassphraseSource(key.to_path_buf()))
    }
}

/// Loads an identity, asking `src` for a passphrase only if the key is
/// encrypted.
pub fn load_identity_interactive(
    path: &Path,
    src: &PassphraseSource,
) -> Result<SecretKey, IdentityError> {
    match load_secret_key(path, None) {
        Err(IdentityError::PassphraseRequired) => {}
        r => return r,
    }
    let pass = src.passphrase(path)?;
    match load_secret_key(path, Some(&pass)) {
        Err(IdentityError::Ssh(ssh_key::Error::Crypto)) => {
            Err(IdentityError::BadPassphrase(path.to_path_buf()))
        }
        r => r,
    }
}

/// Create `path` holding `contents` with `mode`, only if nothing is there:
/// an existing file is never replaced (`AlreadyExists`). The content is
/// written to a temporary file in the same directory first, so `path`
/// never appears half-written.
pub fn write_new(path: &Path, contents: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let mut tmp = tempfile::Builder::new()
        .prefix(".irsync-new")
        .permissions(std::fs::Permissions::from_mode(mode))
        .tempfile_in(dir)?;
    tmp.write_all(contents)?;
    tmp.as_file().sync_all()?;
    tmp.persist_noclobber(path).map_err(|e| e.error)?;
    Ok(())
}

/// Generate a new Ed25519 key, writing `path` (mode 0600) and `path.pub`.
/// Refuses (`AlreadyExists`) when either file exists; the private key is
/// created atomically and never written over a key that appeared meanwhile.
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
    let text = key.to_openssh(LineEnding::LF)?;
    match write_new(path, text.as_bytes(), 0o600) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(IdentityError::AlreadyExists(path.to_path_buf()))
        }
        Err(source) => {
            return Err(IdentityError::File {
                path: path.to_path_buf(),
                source,
            })
        }
    }
    let line = format!("{}\n", key.public_key().to_openssh()?);
    match write_new(&pub_path, line.as_bytes(), 0o644) {
        // Another process wrote it first: the private key is what counts.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(IdentityError::File {
                path: pub_path,
                source,
            })
        }
        Ok(()) => {}
    }
    secret_of(&key)
}

/// Loads the identity at `path`, generating it (with its parent directories)
/// when the file does not exist. The flag is `true` when it was generated
/// now. An existing key that cannot be read or unlocked is an error and is
/// never replaced; a key another process creates first is loaded.
pub fn load_or_generate(
    path: &Path,
    src: &PassphraseSource,
    comment: &str,
) -> Result<(SecretKey, bool), IdentityError> {
    if path.symlink_metadata().is_ok() {
        return Ok((load_identity_interactive(path, src)?, false));
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|source| IdentityError::File {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    match generate_openssh(path, comment) {
        Err(IdentityError::AlreadyExists(p)) if p == path => {
            Ok((load_identity_interactive(path, src)?, false))
        }
        r => Ok((r?, true)),
    }
}

/// Where a client identity came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    /// The key at the named (or default) path existed and was loaded.
    Loaded(PathBuf),
    /// No irsync key yet: the user's own ssh key was loaded (never written).
    Ssh(PathBuf),
    /// Nothing served: a key was generated at the path.
    Generated(PathBuf),
}

impl KeySource {
    pub fn path(&self) -> &Path {
        match self {
            KeySource::Loaded(p) | KeySource::Ssh(p) | KeySource::Generated(p) => p,
        }
    }
}

/// The file [`load_client_identity`] reads or creates: `ssh` when there is
/// nothing at `path` and the ssh key exists, else `path`.
pub fn client_key_path<'a>(path: &'a Path, ssh: Option<&'a Path>) -> &'a Path {
    if path.symlink_metadata().is_err() {
        if let Some(ssh) = ssh.filter(|s| s.symlink_metadata().is_ok()) {
            return ssh;
        }
    }
    path
}

/// Resolves the client identity the way ssh picks a key: the key at `path`
/// when it exists; else `ssh` (`~/.ssh/id_ed25519`) when it exists, loaded
/// as it is; else a new key generated at `path`. `ssh` is `None` when the
/// user named a path (`--identity`, or `identity` in the config): that path
/// is then the whole answer.
pub fn load_client_identity(
    path: &Path,
    ssh: Option<&Path>,
    src: &PassphraseSource,
    comment: &str,
) -> Result<(SecretKey, KeySource), IdentityError> {
    let key = client_key_path(path, ssh);
    if key != path {
        let secret = load_identity_interactive(key, src)?;
        return Ok((secret, KeySource::Ssh(key.to_path_buf())));
    }
    let (secret, generated) = load_or_generate(path, src, comment)?;
    let source = if generated {
        KeySource::Generated(path.to_path_buf())
    } else {
        KeySource::Loaded(path.to_path_buf())
    };
    Ok((secret, source))
}

/// The iroh endpoint id for an SSH public key, if it is Ed25519.
pub fn endpoint_id_of_public_key(pk: &ssh_key::PublicKey) -> Option<EndpointId> {
    let ed = pk.key_data().ed25519()?;
    EndpointId::from_bytes(&ed.0).ok()
}

/// The OpenSSH public key line (`ssh-ed25519 AAAA... comment`) for `secret`,
/// derived from the key itself rather than any `.pub` file.
pub fn public_key_line(secret: &SecretKey, comment: &str) -> String {
    let data = ssh_key::public::KeyData::Ed25519(ssh_key::public::Ed25519PublicKey(
        *secret.public().as_bytes(),
    ));
    ssh_key::PublicKey::new(data, comment)
        .to_openssh()
        .expect("an Ed25519 public key encodes")
}

/// Parse an endpoint id in any format iroh prints (hex) or z-base-32.
pub fn parse_endpoint_id(s: &str) -> Result<EndpointId, IdentityError> {
    let s = s.trim();
    EndpointId::from_str(s)
        .or_else(|_| EndpointId::from_z32(s))
        .map_err(|_| IdentityError::BadEndpointId(s.to_string()))
}
