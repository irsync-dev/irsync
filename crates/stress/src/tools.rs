//! Locating the `irsync`, `filegen` and `fileset` binaries and recording
//! their versions.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};

use crate::process;

use crate::run_file::{ToolInfo, ToolVersions};

/// A located tool and the first line of its `--version` output.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub path: PathBuf,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tools {
    pub irsync: Tool,
    pub filegen: Tool,
    pub fileset: Tool,
}

fn is_executable(p: &Path) -> bool {
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// The search without `--version`: an override wins, else the executable's
/// own directory, else the first `PATH` directory holding an executable file.
pub fn find(
    name: &str,
    override_: Option<&Path>,
    exe_dir: &Path,
    path_env: &OsStr,
) -> Option<PathBuf> {
    if let Some(p) = override_ {
        return Some(p.to_path_buf());
    }
    let beside = exe_dir.join(name);
    if is_executable(&beside) {
        return Some(beside);
    }
    std::env::split_paths(path_env)
        .map(|d| d.join(name))
        .find(|c| is_executable(c))
}

fn version_of(name: &str, path: &Path) -> Result<String> {
    let out = process::output(
        Command::new(path)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .with_context(|| format!("{name}: cannot run {} --version", path.display()))?;
    if !out.status.success() {
        bail!(
            "{name}: {} --version exited with {}",
            path.display(),
            out.status
        );
    }
    let text = String::from_utf8_lossy(&out.stdout);
    match text.lines().next().map(str::trim) {
        Some(l) if !l.is_empty() => Ok(l.to_string()),
        _ => bail!("{name}: {} --version printed nothing", path.display()),
    }
}

fn locate_one(
    name: &str,
    override_: Option<&Path>,
    exe_dir: &Path,
    path_env: &OsStr,
) -> Result<Tool> {
    let path = find(name, override_, exe_dir, path_env).ok_or_else(|| {
        anyhow!(
            "{name}: not found (searched {} and PATH)",
            exe_dir.display()
        )
    })?;
    let version = version_of(name, &path)?;
    Ok(Tool { path, version })
}

impl Tools {
    /// Each override wins; else beside the current executable; else `PATH`.
    pub fn locate(
        irsync: Option<&Path>,
        filegen: Option<&Path>,
        fileset: Option<&Path>,
    ) -> Result<Tools> {
        let exe = std::env::current_exe().context("cannot find the current executable")?;
        let exe_dir = exe.parent().unwrap_or(Path::new(".")).to_path_buf();
        let path_env = std::env::var_os("PATH").unwrap_or_default();
        Ok(Tools {
            irsync: locate_one("irsync", irsync, &exe_dir, &path_env)?,
            filegen: locate_one("filegen", filegen, &exe_dir, &path_env)?,
            fileset: locate_one("fileset", fileset, &exe_dir, &path_env)?,
        })
    }

    pub fn versions(&self) -> ToolVersions {
        let info = |t: &Tool| ToolInfo {
            path: t.path.display().to_string(),
            version: t.version.clone(),
        };
        ToolVersions {
            irsync: info(&self.irsync),
            filegen: info(&self.filegen),
            fileset: info(&self.fileset),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec_script(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn override_wins_then_exe_dir_then_path() {
        let t = tempfile::tempdir().unwrap();
        let (exe_dir, path_dir) = (t.path().join("exe"), t.path().join("path"));
        for d in [&exe_dir, &path_dir] {
            std::fs::create_dir(d).unwrap();
        }
        exec_script(&path_dir.join("filegen"), "");
        assert_eq!(
            find("filegen", None, &exe_dir, path_dir.as_os_str()),
            Some(path_dir.join("filegen"))
        );
        exec_script(&exe_dir.join("filegen"), "");
        assert_eq!(
            find("filegen", None, &exe_dir, path_dir.as_os_str()),
            Some(exe_dir.join("filegen"))
        );
        assert_eq!(
            find(
                "filegen",
                Some(Path::new("/o/filegen")),
                &exe_dir,
                path_dir.as_os_str()
            ),
            Some(PathBuf::from("/o/filegen"))
        );
        assert_eq!(find("nothing", None, &exe_dir, path_dir.as_os_str()), None);
    }

    #[test]
    fn non_executable_files_are_skipped() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("filegen"), b"").unwrap();
        assert_eq!(find("filegen", None, t.path(), t.path().as_os_str()), None);
    }

    #[test]
    fn missing_tool_names_itself() {
        let err = Tools::locate(Some(Path::new("/nonexistent/irsync")), None, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("irsync"), "{err}");
    }

    #[test]
    fn version_is_first_line_and_failures_name_the_tool() {
        let t = tempfile::tempdir().unwrap();
        let ok = t.path().join("ok");
        exec_script(&ok, "#!/bin/sh\necho 'tool 1.2.3'\necho second\n");
        assert_eq!(version_of("ok", &ok).unwrap(), "tool 1.2.3");
        let bad = t.path().join("bad");
        exec_script(&bad, "#!/bin/sh\nexit 3\n");
        assert!(version_of("bad", &bad)
            .unwrap_err()
            .to_string()
            .contains("bad"));
        let empty = t.path().join("empty");
        exec_script(&empty, "#!/bin/sh\n");
        assert!(version_of("empty", &empty)
            .unwrap_err()
            .to_string()
            .contains("empty"));
    }
}
