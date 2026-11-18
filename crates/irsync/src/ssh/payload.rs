//! The files the ssh install sends to the server: the program, and the
//! `fileset` program and man pages when there are any, packed with the
//! installer script into one plain tar. The server unpacks it with `tar -xf`
//! and runs the script on `files/`, which has the release tarball's layout.

use std::os::unix::fs::{symlink, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::{installer, release};

/// The three man pages, relative to a man directory.
const PAGES: [&str; 3] = ["man1/irsync.1", "man1/fileset.1", "man5/irsync.toml.5"];

/// What the staged files hold besides `irsync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Contents {
    pub fileset: bool,
    pub man: bool,
}

/// The files to install, in the tarball's layout, at `files()`.
#[derive(Debug)]
pub struct Staged {
    /// Holds `files`, a directory or a link to one.
    root: tempfile::TempDir,
    /// Keeps a downloaded release alive while `files` links to it.
    _release: Option<release::Fetched>,
}

impl Staged {
    /// The files of a downloaded release: only the five known names, each
    /// checked to be a regular file under real directories. A symlink on
    /// any of those paths is an error, never skipped, so a release cannot
    /// make the client send some other file. Anything else in the release
    /// is left out.
    pub fn from_release(fetched: release::Fetched) -> Result<Staged> {
        let root = temp_root()?;
        let files = root.path().join("files");
        std::fs::create_dir(&files).context("cannot make the staging directory")?;
        let base = fetched.dir();
        let top = std::fs::symlink_metadata(base)
            .with_context(|| format!("cannot read {}", base.display()))?;
        if !top.is_dir() {
            bail!("the release holds its top directory as something other than a directory (a symlink?)");
        }
        if !release_member(base, "irsync")? {
            bail!("the release does not hold irsync");
        }
        link(&base.join("irsync"), &files.join("irsync"))?;
        if release_member(base, "fileset")? {
            link(&base.join("fileset"), &files.join("fileset"))?;
        }
        let mut pages = Vec::new();
        for page in PAGES {
            let member = format!("man/{page}");
            if release_member(base, &member)? {
                pages.push(member);
            }
        }
        if pages.len() == PAGES.len() {
            for member in pages {
                let to = files.join(&member);
                std::fs::create_dir_all(to.parent().unwrap())
                    .context("cannot make the staging directory")?;
                link(&base.join(&member), &to)?;
            }
        }
        Ok(Staged {
            root,
            _release: Some(fetched),
        })
    }

    /// This machine's files: `exe` as `irsync`; `fileset` beside it if a
    /// regular file; the three man pages from `<exe dir>/man`, else
    /// `<exe dir>/../share/man`, all three or none. Symlinks are followed
    /// on purpose: package managers use them, and the user ran this binary.
    pub fn from_local(exe: &Path) -> Result<Staged> {
        let exe =
            std::path::absolute(exe).with_context(|| format!("cannot find {}", exe.display()))?;
        let exe_dir = exe.parent().context("the program has no directory")?;
        let root = temp_root()?;
        let files = root.path().join("files");
        std::fs::create_dir(&files).context("cannot make the staging directory")?;
        link(&exe, &files.join("irsync"))?;
        let fileset = exe_dir.join("fileset");
        if std::fs::metadata(&fileset).is_ok_and(|m| m.is_file()) {
            link(&fileset, &files.join("fileset"))?;
        }
        let man = [exe_dir.join("man"), exe_dir.join("../share/man")]
            .into_iter()
            .find(|dir| PAGES.iter().all(|p| is_file(&dir.join(p))));
        if let Some(man) = man {
            for page in PAGES {
                let to = files.join("man").join(page);
                std::fs::create_dir_all(to.parent().unwrap())
                    .context("cannot make the staging directory")?;
                link(&man.join(page), &to)?;
            }
        }
        Ok(Staged {
            root,
            _release: None,
        })
    }

    fn files(&self) -> PathBuf {
        self.root.path().join("files")
    }

    pub fn contents(&self) -> Contents {
        let files = self.files();
        Contents {
            fileset: is_file(&files.join("fileset")),
            man: PAGES.iter().all(|p| is_file(&files.join("man").join(p))),
        }
    }

    /// A plain tar holding `files/…` (the staged files) and `installer.sh`.
    pub fn pack(&self) -> Result<Packed> {
        let script_dir = tempfile::Builder::new()
            .prefix("irsync-installer.")
            .tempdir()
            .context("cannot make a temporary directory for the installer")?;
        std::fs::write(script_dir.path().join("installer.sh"), installer::script())
            .context("cannot write the installer script")?;
        let out = tempfile::Builder::new()
            .prefix("irsync-payload.")
            .suffix(".tar")
            .tempfile()
            .context("cannot make a temporary file for the files to send")?;
        let mut tar = Command::new("tar");
        // No `._` entries for extended attributes on macOS.
        tar.env("COPYFILE_DISABLE", "1");
        // macOS's bsdtar also writes extended attributes as records of their
        // own: GNU tar on the server warns about each, and a Mac server may
        // put quarantine back on the program. Only there: BusyBox tar and GNU
        // tar before 1.27 refuse the flag.
        if cfg!(target_os = "macos") {
            tar.arg("--no-xattrs");
        }
        tar.arg("-chf")
            .arg(out.path())
            .arg("-C")
            .arg(self.root.path())
            .arg("files")
            .arg("-C")
            .arg(script_dir.path())
            .arg("installer.sh");
        let result = tar.output().context("cannot run tar to pack the files")?;
        if !result.status.success() {
            bail!(
                "cannot pack the files to send: tar failed: {}",
                String::from_utf8_lossy(&result.stderr).trim()
            );
        }
        let len = std::fs::metadata(out.path())
            .context("cannot read the packed files")?
            .len();
        Ok(Packed { file: out, len })
    }
}

/// The packed tar, in a temporary file removed on drop.
#[derive(Debug)]
pub struct Packed {
    file: tempfile::NamedTempFile,
    len: u64,
}

impl Packed {
    pub fn path(&self) -> &Path {
        self.file.path()
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

fn temp_root() -> Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("irsync-stage.")
        .tempdir()
        .context("cannot make a temporary directory for the files to send")
}

/// A regular file, or a link to one.
fn is_file(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_file())
}

/// Whether `rel` (like `man/man1/irsync.1`) is in the unpacked release at
/// `base`: `Ok(false)` if something on the way is absent. Errors if the
/// leaf is not a regular file with one link, or a directory on the way is
/// not a real directory (a symlink, say).
fn release_member(base: &Path, rel: &str) -> Result<bool> {
    let parts: Vec<&str> = rel.split('/').collect();
    let mut path = base.to_path_buf();
    for (i, part) in parts.iter().enumerate() {
        path.push(part);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        let ok = if i + 1 == parts.len() {
            meta.is_file()
        } else {
            meta.is_dir()
        };
        if !ok {
            let what = parts[..=i].join("/");
            bail!("the release holds {what} as something other than a regular file or directory (a symlink?)");
        }
        if i + 1 == parts.len() && meta.nlink() != 1 {
            bail!("the release holds {rel} as a file with more than one name (a hard link)");
        }
    }
    Ok(true)
}

fn link(from: &Path, to: &Path) -> Result<()> {
    symlink(from, to).with_context(|| format!("cannot stage {}", from.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A directory with a small stand-in `irsync` (named `exe_name`).
    fn bin_dir(exe_name: &str) -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let exe = d.path().join(exe_name);
        std::fs::write(&exe, b"fake irsync").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        (d, exe)
    }

    fn put_pages(man: &Path) {
        for p in PAGES {
            let f = man.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, b".TH x 1\n").unwrap();
        }
    }

    /// The names in the packed tar, and the tar unpacked into a directory.
    fn unpack(staged: &Staged) -> (Vec<String>, tempfile::TempDir, Packed) {
        let packed = staged.pack().unwrap();
        let list = Command::new("tar")
            .arg("-tf")
            .arg(packed.path())
            .output()
            .unwrap();
        assert!(list.status.success());
        let mut names: Vec<String> = String::from_utf8(list.stdout)
            .unwrap()
            .lines()
            .map(|l| l.trim_end_matches('/').to_string())
            .collect();
        names.sort();
        let out = tempfile::tempdir().unwrap();
        let st = Command::new("tar")
            .arg("-xf")
            .arg(packed.path())
            .arg("-C")
            .arg(out.path())
            .status()
            .unwrap();
        assert!(st.success());
        (names, out, packed)
    }

    #[test]
    fn local_alone() {
        let (_d, exe) = bin_dir("irsync");
        let s = Staged::from_local(&exe).unwrap();
        assert_eq!(
            s.contents(),
            Contents {
                fileset: false,
                man: false
            }
        );
        let (names, _out, _p) = unpack(&s);
        assert_eq!(names, ["files", "files/irsync", "installer.sh"]);
    }

    #[test]
    fn local_with_fileset() {
        let (d, exe) = bin_dir("irsync");
        std::fs::write(d.path().join("fileset"), b"fake fileset").unwrap();
        let s = Staged::from_local(&exe).unwrap();
        assert_eq!(
            s.contents(),
            Contents {
                fileset: true,
                man: false
            }
        );
        let (names, out, _p) = unpack(&s);
        assert!(names.contains(&"files/fileset".to_string()), "{names:?}");
        assert_eq!(
            std::fs::read(out.path().join("files/fileset")).unwrap(),
            b"fake fileset"
        );
    }

    #[test]
    fn local_pages_beside() {
        let (d, exe) = bin_dir("irsync");
        put_pages(&d.path().join("man"));
        let s = Staged::from_local(&exe).unwrap();
        assert!(s.contents().man);
        let (names, _out, _p) = unpack(&s);
        for p in PAGES {
            assert!(names.contains(&format!("files/man/{p}")), "{names:?}");
        }
    }

    #[test]
    fn local_pages_in_share() {
        let top = tempfile::tempdir().unwrap();
        let bin = top.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let exe = bin.join("irsync");
        std::fs::write(&exe, b"fake irsync").unwrap();
        put_pages(&top.path().join("share/man"));
        let s = Staged::from_local(&exe).unwrap();
        assert!(s.contents().man);
        let (names, _out, _p) = unpack(&s);
        for p in PAGES {
            assert!(names.contains(&format!("files/man/{p}")), "{names:?}");
        }
    }

    #[test]
    fn local_two_pages_is_none() {
        let (d, exe) = bin_dir("irsync");
        let man = d.path().join("man");
        put_pages(&man);
        std::fs::remove_file(man.join(PAGES[2])).unwrap();
        let s = Staged::from_local(&exe).unwrap();
        assert!(!s.contents().man);
        let (names, _out, _p) = unpack(&s);
        assert!(!names.iter().any(|n| n.contains("man")), "{names:?}");
    }

    #[test]
    fn exe_with_another_name() {
        let (_d, exe) = bin_dir("irsync-abc123");
        let s = Staged::from_local(&exe).unwrap();
        let (names, out, _p) = unpack(&s);
        assert!(names.contains(&"files/irsync".to_string()), "{names:?}");
        assert_eq!(
            std::fs::read(out.path().join("files/irsync")).unwrap(),
            b"fake irsync"
        );
    }

    #[test]
    fn packed_len_is_file_size() {
        let (_d, exe) = bin_dir("irsync");
        let p = Staged::from_local(&exe).unwrap().pack().unwrap();
        assert!(!p.is_empty());
        assert_eq!(p.len(), std::fs::metadata(p.path()).unwrap().len());
    }

    #[test]
    fn packed_irsync_is_the_file_not_a_link() {
        let (_d, exe) = bin_dir("irsync");
        let s = Staged::from_local(&exe).unwrap();
        let (_names, out, _p) = unpack(&s);
        let got = out.path().join("files/irsync");
        let meta = std::fs::symlink_metadata(&got).unwrap();
        assert!(meta.is_file(), "not a regular file");
        assert_eq!(std::fs::read(&got).unwrap(), b"fake irsync");
        assert!(meta.permissions().mode() & 0o111 != 0);
    }

    /// A release directory with a stand-in irsync, as `fetch` leaves it.
    fn release_dir() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("irsync-x");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("irsync"), b"r").unwrap();
        (tmp, dir)
    }

    fn staged_from(tmp: tempfile::TempDir, dir: &Path) -> Result<Staged> {
        Staged::from_release(release::Fetched::for_test(tmp, dir.to_path_buf()))
    }

    #[test]
    fn a_full_release_stages_all_five() {
        let (tmp, dir) = release_dir();
        std::fs::write(dir.join("fileset"), b"f").unwrap();
        put_pages(&dir.join("man"));
        let s = staged_from(tmp, &dir).unwrap();
        assert_eq!(
            s.contents(),
            Contents {
                fileset: true,
                man: true
            }
        );
        let (names, _out, _p) = unpack(&s);
        let mut want = vec!["files", "files/irsync", "files/fileset", "files/man"];
        want.extend(["files/man/man1", "files/man/man5", "installer.sh"]);
        for w in want {
            assert!(names.contains(&w.to_string()), "{w} missing in {names:?}");
        }
        for p in PAGES {
            assert!(names.contains(&format!("files/man/{p}")), "{names:?}");
        }
    }

    #[test]
    fn a_release_with_two_pages_has_no_man() {
        let (tmp, dir) = release_dir();
        let man = dir.join("man");
        put_pages(&man);
        std::fs::remove_file(man.join(PAGES[1])).unwrap();
        let s = staged_from(tmp, &dir).unwrap();
        assert!(!s.contents().man);
        let (names, _out, _p) = unpack(&s);
        assert!(!names.iter().any(|n| n.contains("man")), "{names:?}");
    }

    #[test]
    fn a_release_symlink_for_fileset_is_an_error() {
        let (tmp, dir) = release_dir();
        let secret = tmp.path().join("secret");
        std::fs::write(&secret, b"s").unwrap();
        symlink(&secret, dir.join("fileset")).unwrap();
        let e = staged_from(tmp, &dir).unwrap_err().to_string();
        assert!(e.contains("fileset"), "{e}");
    }

    #[test]
    fn a_release_symlink_for_a_man_directory_is_an_error() {
        for link_at in ["man", "man/man1"] {
            let (tmp, dir) = release_dir();
            let real = tmp.path().join("elsewhere");
            put_pages(&real);
            if link_at == "man" {
                symlink(&real, dir.join("man")).unwrap();
            } else {
                std::fs::create_dir(dir.join("man")).unwrap();
                symlink(real.join("man1"), dir.join("man/man1")).unwrap();
            }
            let e = staged_from(tmp, &dir).unwrap_err().to_string();
            assert!(e.contains(link_at), "{link_at}: {e}");
        }
    }

    #[test]
    fn a_release_whose_top_directory_is_a_symlink_is_an_error() {
        let (tmp, real) = release_dir();
        let dir = tmp.path().join("irsync-link");
        symlink(&real, &dir).unwrap();
        let e = staged_from(tmp, &dir).unwrap_err().to_string();
        assert!(e.contains("top directory"), "{e}");
    }

    #[test]
    fn a_release_symlink_for_a_page_is_an_error() {
        let (tmp, dir) = release_dir();
        let man = dir.join("man");
        put_pages(&man);
        std::fs::remove_file(man.join(PAGES[0])).unwrap();
        symlink("/etc/hosts", man.join(PAGES[0])).unwrap();
        let e = staged_from(tmp, &dir).unwrap_err().to_string();
        assert!(e.contains("irsync.1"), "{e}");
    }

    #[test]
    fn a_release_hard_link_for_fileset_is_an_error() {
        let (tmp, dir) = release_dir();
        let secret = tmp.path().join("secret");
        std::fs::write(&secret, b"s").unwrap();
        std::fs::hard_link(&secret, dir.join("fileset")).unwrap();
        let e = staged_from(tmp, &dir).unwrap_err().to_string();
        assert!(e.contains("fileset"), "{e}");
        assert!(e.contains("hard link"), "{e}");
    }

    /// On macOS, a staged file with an extended attribute; elsewhere, where
    /// `pack` passes no `--no-xattrs`, a plain one. The tar holds no
    /// attribute records.
    #[test]
    fn packed_tar_has_no_extended_attributes() {
        let (_d, exe) = bin_dir("irsync");
        if cfg!(target_os = "macos") {
            let set = Command::new("xattr")
                .args(["-w", "user.test", "1"])
                .arg(&exe)
                .status();
            assert!(set.is_ok_and(|s| s.success()), "xattr -w failed");
        }
        let p = Staged::from_local(&exe).unwrap().pack().unwrap();
        let raw = std::fs::read(p.path()).unwrap();
        for name in [&b"LIBARCHIVE.xattr"[..], b"SCHILY.xattr"] {
            assert!(
                !raw.windows(name.len()).any(|w| w == name),
                "{} in the tar",
                String::from_utf8_lossy(name)
            );
        }
    }

    #[test]
    fn extra_release_members_are_not_sent() {
        let (tmp, dir) = release_dir();
        std::fs::write(dir.join("evil"), b"x").unwrap();
        std::fs::create_dir(dir.join("extra")).unwrap();
        std::fs::write(dir.join("extra/more"), b"x").unwrap();
        std::fs::write(dir.join("README.md"), b"x").unwrap();
        let s = staged_from(tmp, &dir).unwrap();
        let (names, _out, _p) = unpack(&s);
        assert_eq!(names, ["files", "files/irsync", "installer.sh"]);
    }
}
