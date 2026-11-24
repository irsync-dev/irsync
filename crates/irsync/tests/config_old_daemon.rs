//! Converting a file of layout 1 is refused while a daemon runs that
//! cannot read layout 2. (This build is itself 0.1.0-beta.6, below
//! `READS_CONFIG_V2`, so the old daemons here are beta.5.)
#![cfg(unix)]

use irsync::manage::{load_message, lock_config, save_config, ManageError};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

fn id() -> String {
    iroh::SecretKey::from_bytes(&[5; 32]).public().to_string()
}

fn v1(state: &Path) -> String {
    format!(
        "state_dir = {:?}\n\n[hosts.a]\nendpoint_id = \"{}\"\n",
        state.display().to_string(),
        id()
    )
}

/// Holds the daemon lock of `state` with `record` in it.
fn hold(state: &Path, record: &str) -> std::fs::File {
    std::fs::create_dir_all(state).unwrap();
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(state.join("daemon.lock"))
        .unwrap();
    f.try_lock().unwrap();
    f.write_all(record.as_bytes()).unwrap();
    f
}

fn record(version: &str) -> String {
    format!("pid=4242\nexe=/bin/irsync\nversion={version}\n")
}

/// Loads and saves `path`.
fn resave(path: &Path) -> Result<(), ManageError> {
    let mut lock = lock_config(path)?;
    let mut msg = lock.load()?;
    lock.save(&mut msg)
}

fn setup(text: impl FnOnce(&Path) -> String) -> (tempfile::TempDir, std::path::PathBuf, String) {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let path = dir.path().join("irsync.toml");
    let text = text(&state);
    std::fs::write(&path, &text).unwrap();
    irsync::platform::testutil::set_mode(&path, 0o600).unwrap();
    (dir, path, text)
}

#[test]
fn save_of_a_v1_file_is_refused_under_an_old_daemon() {
    let (dir, path, text) = setup(v1);
    let _held = hold(&dir.path().join("state"), &record("0.1.0-beta.5 (abc123)"));
    let err = resave(&path).unwrap_err();
    assert!(
        matches!(err, ManageError::OldDaemon { pid: 4242, .. }),
        "{err}"
    );
    let shown = err.to_string();
    assert!(
        shown.contains("pid 4242, version 0.1.0-beta.5 (abc123)"),
        "{shown}"
    );
    assert!(
        shown.contains("config_version 2, read from irsync 0.1.0-beta.7 on"),
        "{shown}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    assert!(!dir.path().join("irsync.toml.lock").exists());
}

#[test]
fn save_goes_through_with_no_daemon_or_a_current_one() {
    let (dir, path, _) = setup(v1);
    resave(&path).unwrap();
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("config_version = 2"));

    for version in [
        format!("{} (abc)", env!("CARGO_PKG_VERSION")),
        "0.1.0-beta.7 (abc)".to_string(),
        "0.1.0 (abc)".to_string(),
        "1.2.3".to_string(),
    ] {
        let (dir, path, _) = setup(v1);
        let _held = hold(&dir.path().join("state"), &record(&version));
        resave(&path).unwrap_or_else(|e| panic!("{version}: {e}"));
    }
    drop(dir);
}

#[test]
fn unknown_daemon_version_refuses_conversion() {
    for rec in [
        String::new(),
        "pid=0\n".to_string(),
        record("garbage"),
        record(""),
    ] {
        let (dir, path, text) = setup(v1);
        let _held = hold(&dir.path().join("state"), &rec);
        let err = resave(&path).unwrap_err();
        assert!(
            matches!(err, ManageError::OldDaemon { .. }),
            "{rec:?}: {err}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }
    let (dir, path, _) = setup(v1);
    let _held = hold(&dir.path().join("state"), "");
    let shown = resave(&path).unwrap_err().to_string();
    assert!(shown.contains("version unknown"), "{shown}");
}

#[test]
fn a_v2_file_is_saved_under_any_daemon() {
    let (dir, path, _) = setup(|state| {
        format!(
            "config_version = 2\nstate_dir = {:?}\n",
            state.display().to_string()
        )
    });
    let _held = hold(&dir.path().join("state"), &record("0.1.0-beta.5 (abc)"));
    resave(&path).unwrap();
}

/// A file of the current layout that does not say `config_version`:
/// an older irsync reads it, but not the file a save writes.
fn unversioned(state: &Path) -> String {
    format!(
        "state_dir = {:?}\n\n[exports.www]\npath = \"/srv/www\"\n",
        state.display().to_string()
    )
}

#[test]
fn save_of_a_file_without_config_version_is_refused_under_an_old_daemon() {
    let (dir, path, text) = setup(unversioned);
    let _held = hold(&dir.path().join("state"), &record("0.1.0-beta.5 (abc)"));
    let err = resave(&path).unwrap_err();
    assert!(matches!(err, ManageError::OldDaemon { .. }), "{err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    assert!(!dir.path().join("irsync.toml.lock").exists());

    let (_dir, path, _) = setup(unversioned);
    resave(&path).unwrap();
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("config_version = 2"));
}

/// Saves a first file at `path`, naming `state` as its state dir.
fn create(path: &Path, state: &Path) -> Result<(), ManageError> {
    let mut lock = lock_config(path)?;
    let mut msg = lock.load()?;
    msg.state_dir = Some(state.display().to_string());
    lock.save(&mut msg)
}

#[test]
fn first_save_is_refused_under_an_old_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let path = dir.path().join("irsync.toml");
    let held = hold(&state, &record("0.1.0-beta.5 (abc)"));
    let err = create(&path, &state).unwrap_err();
    assert!(matches!(err, ManageError::OldDaemon { .. }), "{err}");
    assert!(!path.exists());
    assert!(!dir.path().join("irsync.toml.lock").exists());

    drop(held);
    create(&path, &state).unwrap();
    assert!(path.exists());
}

#[test]
fn save_config_checks_the_daemon_too() {
    for text in [v1 as fn(&Path) -> String, unversioned] {
        let (dir, path, before) = setup(text);
        let _held = hold(&dir.path().join("state"), &record("0.1.0-beta.5 (abc)"));
        let mut msg = load_message(&path).unwrap();
        let err = save_config(&path, &mut msg).unwrap_err();
        assert!(matches!(err, ManageError::OldDaemon { .. }), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }
    // A file that already says config_version 2: any daemon reads it.
    let (dir, path, _) = setup(|state| {
        format!(
            "config_version = 2\nstate_dir = {:?}\n",
            state.display().to_string()
        )
    });
    let _held = hold(&dir.path().join("state"), &record("0.1.0-beta.5 (abc)"));
    let mut msg = load_message(&path).unwrap();
    save_config(&path, &mut msg).unwrap();
}

#[test]
fn a_record_with_pid_0_and_the_current_version_is_refused() {
    let (dir, path, text) = setup(v1);
    let rec = format!("pid=0\nversion={} (abc)\n", env!("CARGO_PKG_VERSION"));
    let _held = hold(&dir.path().join("state"), &rec);
    let err = resave(&path).unwrap_err();
    assert!(
        matches!(err, ManageError::OldDaemon { pid: 0, .. }),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
}
