//! The release binary for a server whose OS or CPU differs from this
//! machine's: which file it is, and downloading, checking and unpacking it
//! here on the client. Knows nothing about ssh.

use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::build_info::BuildInfo;
use crate::signature::{release_keys, verify_file};

/// Where GitHub serves a release's files, without the tag.
const RELEASES: &str = "https://github.com/irsync-dev/irsync/releases/download";

/// The checksum list every release carries.
const SUMS: &str = "sha256.sum";

/// The signature of [`SUMS`].
const SUMS_SIG: &str = "sha256.sum.minisig";

/// The release target for a server platform, as `parse_uname` names it.
pub fn target_for(os: Option<&str>, arch: Option<&str>) -> Option<&'static str> {
    match (os?, arch?) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        _ => None,
    }
}

/// `v<version>` if this build can have a release; `None` for a development
/// build: uncommitted changes, an unknown commit, or built with `netsim`.
pub fn release_tag(info: &BuildInfo) -> Option<String> {
    if cfg!(feature = "netsim") || info.dirty == Some(true) || info.commit == "unknown" {
        return None;
    }
    Some(format!("v{}", info.version))
}

/// The hex digest `sums` lists for `file_name`, lowercased. Takes both
/// `<hex> *<name>` and `<hex>  <name>` lines.
pub(crate) fn expected_sha256(sums: &str, file_name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hex, name) = line.trim().split_once(char::is_whitespace)?;
        let name = name.trim_start();
        let name = name.strip_prefix('*').unwrap_or(name);
        (name == file_name).then(|| hex.to_ascii_lowercase())
    })
}

/// Test builds only: a URL that stands in for `RELEASES/<tag>`, from
/// `IRSYNC_TEST_RELEASE_URL`. May be a `file://` URL.
#[cfg(feature = "test-hooks")]
pub(crate) fn test_url() -> Option<String> {
    std::env::var("IRSYNC_TEST_RELEASE_URL")
        .ok()
        .filter(|u| !u.is_empty())
}

/// Test builds only: the keys a release must be signed with, from the
/// key file `IRSYNC_TEST_RELEASE_KEYS` names, instead of the release keys.
#[cfg(feature = "test-hooks")]
fn test_keys() -> Result<Option<Vec<minisign_verify::PublicKey>>> {
    let Some(path) = std::env::var_os("IRSYNC_TEST_RELEASE_KEYS").filter(|p| !p.is_empty()) else {
        return Ok(None);
    };
    let path = PathBuf::from(path);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let keys = crate::signature::parse_keys(&text)
        .with_context(|| format!("bad key file {}", path.display()))?;
    Ok(Some(keys))
}

/// The keys a release must be signed with.
fn keys() -> Result<Vec<minisign_verify::PublicKey>> {
    #[cfg(feature = "test-hooks")]
    if let Some(keys) = test_keys()? {
        return Ok(keys);
    }
    Ok(release_keys())
}

/// The URL prefix of `tag`'s files, and whether `file://` is allowed.
fn source(tag: &str) -> (String, bool) {
    #[cfg(feature = "test-hooks")]
    if let Some(url) = test_url() {
        return (url, true);
    }
    (format!("{RELEASES}/{tag}"), false)
}

/// A downloaded, checked and unpacked release. The directory that holds
/// it is removed on drop.
#[derive(Debug)]
pub struct Fetched {
    dir: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Fetched {
    /// The unpacked `irsync-<target>/` directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
impl Fetched {
    /// A release unpacked by hand: `dir` lives inside `tmp`.
    pub fn for_test(tmp: tempfile::TempDir, dir: PathBuf) -> Fetched {
        Fetched { dir, _tmp: tmp }
    }
}

/// Download `irsync-<target>.tar.xz`, `sha256.sum` and its signature for
/// `tag` with the system's `curl` or `wget`. The signature (made for
/// exactly `tag`) is checked before the tarball is downloaded; then the
/// tarball's hash is checked and the tarball is unpacked with `tar`. Fails
/// unless the unpacked directory holds `irsync` as a regular file.
pub fn fetch(tag: &str, target: &str) -> Result<Fetched> {
    let (base, allow_file) = source(tag);
    let dir = tempfile::Builder::new()
        .prefix("irsync-release.")
        .tempdir()
        .context("cannot make a temporary directory for the download")?;
    let file = format!("irsync-{target}.tar.xz");
    let tarball = dir.path().join(&file);
    let sums = dir.path().join(SUMS);
    download(&format!("{base}/{SUMS}"), &sums, tag, allow_file)?;
    let sums_sig = dir.path().join(SUMS_SIG);
    let url = format!("{base}/{SUMS_SIG}");
    if let Some((tool, code)) = try_download(&url, &sums_sig, allow_file)? {
        // The server answered with an error. sha256.sum just downloaded,
        // so a 404 (no signature) is the likely cause, but an outage or a
        // rate limit answers the same way.
        if is_missing(tool, code) {
            bail!(
                "the release {tag} has no signature for {SUMS}, or GitHub answered with an error; \
                 refusing to use the download"
            );
        }
        bail!("{}", download_failure(tool, code, tag, &url));
    }
    let keys = keys()?;
    if let Err(e) = verify_file(&sums, &sums_sig, &keys, Some(&format!("irsync {tag}"))) {
        bail!("the release {tag} failed the signature check: {e}");
    }

    // The two small files are checked first, so a refused release costs
    // two small downloads, not the whole tarball.
    download(&format!("{base}/{file}"), &tarball, tag, allow_file)?;
    let listed = std::fs::read_to_string(&sums).context("cannot read the downloaded sha256.sum")?;
    let Some(want) = expected_sha256(&listed, &file) else {
        bail!("{SUMS} does not list {file}");
    };
    if sha256_of(&tarball)? != want {
        bail!("the download of {file} is damaged (SHA-256 does not match); try again");
    }

    let member = format!("irsync-{target}/irsync");
    let out = Command::new("tar")
        .arg("-xJf")
        .arg(&tarball)
        .arg("-C")
        .arg(dir.path())
        .output()
        .context("cannot run tar to unpack the release")?;
    if !out.status.success() {
        bail!(
            "cannot unpack {file}: tar failed (it must be able to read .xz): {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let unpacked = dir.path().join(format!("irsync-{target}"));
    // A symlink is not the binary, whatever it points at.
    let is_file = std::fs::symlink_metadata(unpacked.join("irsync")).is_ok_and(|m| m.is_file());
    if !is_file {
        bail!("{file} does not hold {member}");
    }
    Ok(Fetched {
        dir: unpacked,
        _tmp: dir,
    })
}

fn sha256_of(path: &Path) -> Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("cannot read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Curl,
    Wget,
}

/// Fetch `url` into `to` with curl, or wget when there is no curl. Their
/// own error output stays on the terminal.
fn download(url: &str, to: &Path, tag: &str, allow_file: bool) -> Result<()> {
    match try_download(url, to, allow_file)? {
        None => Ok(()),
        Some((tool, code)) => bail!("{}", download_failure(tool, code, tag, url)),
    }
}

/// Like [`download`], but a tool that ran and failed is not an error: it
/// comes back as the tool and its exit code.
fn try_download(url: &str, to: &Path, allow_file: bool) -> Result<Option<(Tool, Option<i32>)>> {
    let protocols = if allow_file { "=file,https" } else { "=https" };
    let mut curl = Command::new("curl");
    curl.args(["-fsSL", "--proto", protocols, "--proto-redir", "=https"])
        .arg("-o")
        .arg(to)
        .arg(url);
    let mut wget = Command::new("wget");
    wget.arg("-q").arg("-O").arg(to).arg(url);
    for (tool, mut cmd) in [(Tool::Curl, curl), (Tool::Wget, wget)] {
        match cmd.status() {
            Ok(status) if status.success() => return Ok(None),
            Ok(status) => return Ok(Some((tool, status.code()))),
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| format!("cannot run {tool:?} to download {url}"))
            }
        }
    }
    bail!("need curl or wget on this machine to download the release binary")
}

/// Whether `tool`'s exit `code` says the file is not there: the server
/// answered with an error (curl 22, wget 8), or, for a `file://` URL that
/// only test builds allow, curl could not read the file (37).
fn is_missing(tool: Tool, code: Option<i32>) -> bool {
    matches!(
        (tool, code),
        (Tool::Curl, Some(22 | 37)) | (Tool::Wget, Some(8))
    )
}

/// The message for a download that `tool` gave up on with exit `code`.
/// curl's 22 and wget's 8 mean the server answered with an error: a
/// missing release (404), but also an outage or a rate limit, so the
/// message does not claim to know which.
fn download_failure(tool: Tool, code: Option<i32>, tag: &str, url: &str) -> String {
    match (tool, code) {
        (Tool::Curl, Some(22)) | (Tool::Wget, Some(8)) => format!(
            "GitHub answered with an error for {url}. If there is no release {tag}, this \
             irsync is a development build and cannot install on another OS or CPU; \
             otherwise try again later"
        ),
        _ => format!("could not download {url}; check the network and try again"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets() {
        assert_eq!(
            target_for(Some("linux"), Some("x86_64")),
            Some("x86_64-unknown-linux-musl")
        );
        assert_eq!(
            target_for(Some("linux"), Some("aarch64")),
            Some("aarch64-unknown-linux-musl")
        );
        assert_eq!(
            target_for(Some("macos"), Some("aarch64")),
            Some("aarch64-apple-darwin")
        );
        assert_eq!(target_for(Some("macos"), Some("x86_64")), None);
        assert_eq!(target_for(Some("linux"), None), None);
        assert_eq!(target_for(None, Some("x86_64")), None);
    }

    #[test]
    #[cfg(not(feature = "netsim"))]
    fn tags() {
        let b = |sha, dirty| BuildInfo::from_parts("0.1.0-beta.1", sha, dirty);
        assert_eq!(
            release_tag(&b(Some("ae5ebe5"), Some("false"))).as_deref(),
            Some("v0.1.0-beta.1")
        );
        assert_eq!(
            release_tag(&b(Some("ae5ebe5"), None)).as_deref(),
            Some("v0.1.0-beta.1")
        );
        assert_eq!(release_tag(&b(Some("ae5ebe5"), Some("true"))), None);
        assert_eq!(release_tag(&b(None, Some("false"))), None);
    }

    #[test]
    #[cfg(feature = "netsim")]
    fn a_netsim_build_has_no_release() {
        let b = BuildInfo::from_parts("0.1.0", Some("ae5ebe5"), Some("false"));
        assert_eq!(release_tag(&b), None);
    }

    #[test]
    fn sums() {
        let s = "AAAA *irsync-a.tar.xz\r\nbbbb  irsync-b.tar.xz\ncccc *source.tar.gz\n";
        assert_eq!(
            expected_sha256(s, "irsync-a.tar.xz").as_deref(),
            Some("aaaa")
        );
        assert_eq!(
            expected_sha256(s, "irsync-b.tar.xz").as_deref(),
            Some("bbbb")
        );
        assert_eq!(expected_sha256(s, "irsync-c.tar.xz"), None);
        // A name that is only the tail of another does not match.
        assert_eq!(expected_sha256(s, "a.tar.xz"), None);
    }

    #[test]
    fn missing_means_the_server_said_no() {
        assert!(is_missing(Tool::Curl, Some(22)));
        assert!(is_missing(Tool::Curl, Some(37)));
        assert!(is_missing(Tool::Wget, Some(8)));
        assert!(!is_missing(Tool::Curl, Some(6)));
    }

    #[test]
    fn a_missing_release_is_told_apart_from_a_failed_download() {
        let url = "https://example.invalid/v1/f.tar.xz";
        for (tool, code) in [(Tool::Curl, 22), (Tool::Wget, 8)] {
            let m = download_failure(tool, Some(code), "v1", url);
            assert!(m.contains("answered with an error"), "{m}");
            assert!(m.contains("If there is no release v1"), "{m}");
            assert!(m.contains(url), "{m}");
        }
        for (tool, code) in [
            (Tool::Curl, Some(6)),
            (Tool::Curl, Some(8)),
            (Tool::Wget, Some(4)),
            (Tool::Wget, Some(22)),
            (Tool::Curl, None),
        ] {
            let m = download_failure(tool, code, "v1", url);
            assert!(m.starts_with(&format!("could not download {url}")), "{m}");
        }
    }
}
