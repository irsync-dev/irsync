//! A real `irsync daemon` and a real `irsync` client on localhost, with no
//! relay, no address lookup and no port mapper: the shape a stress harness
//! or a fork-a-receiver mode would use.

use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Kills the daemon when the test ends, pass or fail.
struct DaemonGuard(Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn bytes(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 % 251) as u8).collect()
}

fn run_client(args: &[&str], tmp: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_irsync"))
        .args(args)
        .env("HOME", tmp.join("home"))
        .env("XDG_STATE_HOME", tmp.join("xdg"))
        .env_remove("IRSYNC_PASSPHRASE")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn client_and_daemon_sync_over_localhost_with_no_relay_or_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path();
    for d in ["src/sub", "dst", "home", "xdg"] {
        std::fs::create_dir_all(tmp.join(d)).unwrap();
    }
    std::fs::write(tmp.join("src/a.txt"), b"hello").unwrap();
    std::fs::write(tmp.join("src/sub/b.bin"), bytes(64 * 1024)).unwrap();

    // The client generates its own identity under HOME on first use;
    // `irsync id` creates it early and prints its EndpointId (line 2).
    let key = tmp.join("home/.config/irsync/id_ed25519");
    let config_path = tmp.join("irsync.toml");
    let o = run_client(&["id", "--config", config_path.to_str().unwrap()], tmp);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let client_id = String::from_utf8(o.stdout)
        .unwrap()
        .lines()
        .nth(1)
        .unwrap()
        .to_string();

    std::fs::write(
        tmp.join("irsync.toml"),
        format!(
            "state_dir = {:?}\n[daemon]\nidentity = {:?}\n\
             relay_mode = \"disabled\"\nportmap = false\n\
             [daemon.discovery]\ndht = false\ndns = false\nmdns = false\n",
            tmp.join("state"),
            tmp.join("daemon_key"),
        ),
    )
    .unwrap();
    std::fs::write(
        tmp.join("exports.toml"),
        format!(
            "[m]\npath = {:?}\nrw = [\"{}\"]\n",
            tmp.join("dst"),
            client_id
        ),
    )
    .unwrap();

    let child = Command::new(env!("CARGO_BIN_EXE_irsync"))
        .args(["daemon", "--config"])
        .arg(tmp.join("irsync.toml"))
        .arg("--hosts")
        .arg(tmp.join("hosts.toml"))
        .arg("--exports")
        .arg(tmp.join("exports.toml"))
        .args(["--listen", "127.0.0.1:0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut daemon = DaemonGuard(child);
    let stdout = daemon.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let (mut id, mut listen) = (None, None);
    while listen.is_none() {
        let line = rx
            .recv_timeout(Duration::from_secs(15))
            .expect("daemon printed no 'listening on:' line within 15 s");
        if let Some(rest) = line.strip_prefix("endpoint id: ") {
            id = Some(irsync::identity::parse_endpoint_id(rest.trim()).unwrap());
        } else if let Some(rest) = line.strip_prefix("listening on: ") {
            assert!(id.is_some(), "'listening on:' came before 'endpoint id:'");
            listen = Some(rest.trim().parse::<SocketAddr>().unwrap());
        } else {
            panic!("unexpected stdout line {line:?}");
        }
    }
    let (id, listen) = (id.unwrap(), listen.unwrap());
    assert_eq!(listen.ip().to_string(), "127.0.0.1");
    assert_ne!(listen.port(), 0);

    let address = format!("localhost:{}", listen.port());
    let src = format!("{}/", tmp.join("src").display());
    let dest = format!("{id}:m/");
    let config = tmp.join("irsync.toml");
    let args = [
        "-a",
        "--reconnect",
        "0",
        "--address",
        &address,
        "--config",
        config.to_str().unwrap(),
        &src,
        &dest,
    ];
    let o = run_client(&args, tmp);
    assert!(
        o.status.success(),
        "client failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(key.exists(), "client identity under HOME");
    assert_eq!(std::fs::read(tmp.join("dst/a.txt")).unwrap(), b"hello");
    assert_eq!(
        std::fs::read(tmp.join("dst/sub/b.bin")).unwrap(),
        bytes(64 * 1024)
    );

    // A second run through the same addresses is a no-op and still succeeds.
    let o = run_client(&args, tmp);
    assert!(
        o.status.success(),
        "second client run failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn unresolvable_address_fails_before_the_identity_is_needed() {
    use ssh_key::{rand_core::OsRng, Algorithm, LineEnding, PrivateKey};
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path();
    for d in ["src", "home", "xdg"] {
        std::fs::create_dir_all(tmp.join(d)).unwrap();
    }
    std::fs::write(tmp.join("empty_config"), b"").unwrap();
    // An encrypted key: loading it without a passphrase would fail, so the
    // address error can only come first if resolution runs before loading.
    let key = tmp.join("enc_key");
    PrivateKey::random(&mut OsRng, Algorithm::Ed25519)
        .unwrap()
        .encrypt(&mut OsRng, "hunter2")
        .unwrap()
        .write_openssh_file(&key, LineEnding::LF)
        .unwrap();
    let id = iroh::SecretKey::generate().public().to_string();
    let src = format!("{}/", tmp.join("src").display());
    let dest = format!("{id}:m/");
    let config = tmp.join("empty_config");
    let o = run_client(
        &[
            "--address",
            "no-such-host.invalid:9",
            "--identity",
            key.to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            &src,
            &dest,
        ],
        tmp,
    );
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("resolving address no-such-host.invalid:9"),
        "{err}"
    );
    assert!(!err.contains("IRSYNC_PASSPHRASE"), "{err}");
}

#[test]
fn unknown_alias_error_names_hosts_file() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path();
    std::fs::create_dir_all(tmp.join("home")).unwrap();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    let o = run_client(
        &[
            "-a",
            "--hosts",
            tmp.join("hosts.toml").to_str().unwrap(),
            tmp.join("src/").to_str().unwrap(),
            "home:photos/",
        ],
        tmp,
    );
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("\"home\" is not an alias in")
            && err.contains("hosts.toml")
            && err.contains("no such file"),
        "{err}"
    );
}

#[test]
fn no_listening_line_when_startup_fails_after_bind() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path();
    std::fs::create_dir_all(tmp.join("dst")).unwrap();
    // state_dir is a regular file, so opening the export index fails.
    std::fs::write(tmp.join("state"), b"not a directory").unwrap();
    std::fs::write(
        tmp.join("irsync.toml"),
        format!(
            "state_dir = {:?}\n[daemon]\nidentity = {:?}\n\
             relay_mode = \"disabled\"\nportmap = false\n\
             [daemon.discovery]\ndht = false\ndns = false\nmdns = false\n",
            tmp.join("state"),
            tmp.join("daemon_key"),
        ),
    )
    .unwrap();
    std::fs::write(
        tmp.join("exports.toml"),
        format!("[m]\npath = {:?}\n", tmp.join("dst")),
    )
    .unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_irsync"))
        .args(["daemon", "--config"])
        .arg(tmp.join("irsync.toml"))
        .arg("--hosts")
        .arg(tmp.join("hosts.toml"))
        .arg("--exports")
        .arg(tmp.join("exports.toml"))
        .args(["--listen", "127.0.0.1:0"])
        .env_remove("IRSYNC_PASSPHRASE")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!o.status.success(), "daemon should fail to start");
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        !out.contains("listening on:"),
        "ready lines printed although startup failed:\n{out}"
    );
}
