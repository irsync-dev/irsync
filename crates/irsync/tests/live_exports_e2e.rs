//! `irsync add` and `irsync allow` against a running daemon: the daemon
//! re-reads exports.toml per connection, so no restart is needed.

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

fn irsync(home: &Path, args: &[&str]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_irsync"))
        .args(args)
        .env("HOME", home)
        .env("XDG_STATE_HOME", home.join("xdg"))
        .env_remove("IRSYNC_PASSPHRASE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Bounded: a protocol hang fails the test instead of blocking it.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("irsync {args:?} did not finish within 30 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

fn stderr(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Start a daemon with no exports, then `irsync add` and `irsync allow`
/// with the daemon running; the next sync succeeds without a restart.
#[test]
fn add_and_allow_take_effect_without_restart() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path();
    let server = tmp.join("server/home");
    let client = tmp.join("client/home");
    let data = tmp.join("server/data");
    let src = tmp.join("client/src");
    for d in [&server, &client, &src, &data] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(src.join("a.txt"), b"hello").unwrap();

    // 1. Daemon settings: no relay, no lookup, no port mapper.
    let cfg = server.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("irsync.toml"),
        "[daemon]\nrelay_mode = \"disabled\"\nportmap = false\n\
         [daemon.discovery]\ndht = false\ndns = false\nmdns = false\n",
    )
    .unwrap();

    // 2. Start the daemon with no exports; its stderr goes to a file.
    let log = tmp.join("daemon.stderr");
    let child = Command::new(env!("CARGO_BIN_EXE_irsync"))
        .args(["daemon", "--listen", "127.0.0.1:0"])
        .env("HOME", &server)
        .env("XDG_STATE_HOME", server.join("xdg"))
        .env_remove("IRSYNC_PASSPHRASE")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(&log).unwrap())
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
            listen = Some(rest.trim().parse::<SocketAddr>().unwrap());
        } else {
            panic!("unexpected stdout line {line:?}");
        }
    }
    let (daemon_id, listen) = (id.unwrap(), listen.unwrap());
    let warning = std::fs::read_to_string(&log).unwrap();
    assert!(warning.contains("no exports defined"), "{warning}");

    // 3. The client's id.
    let o = irsync(&client, &["id"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let out = String::from_utf8(o.stdout).unwrap();
    let client_id = out.lines().nth(1).unwrap().to_string();

    // 4. Nothing is exported yet: the sync fails.
    let address = format!("127.0.0.1:{}", listen.port());
    let from = format!("{}/", src.display());
    let to = format!("{daemon_id}:data/");
    let sync_args = ["-a", "--reconnect", "0", "--address", &address, &from, &to];
    let o = irsync(&client, &sync_args);
    assert!(!o.status.success(), "sync before add should fail");
    assert!(stderr(&o).contains("connection lost"), "{}", stderr(&o));
    let log_now = std::fs::read_to_string(&log).unwrap();
    assert!(log_now.contains("unauthorized"), "{log_now}");

    // 5. Export and authorize while the daemon runs.
    let o = irsync(&server, &["add", &format!("data:{}", data.display())]);
    assert!(o.status.success(), "{}", stderr(&o));
    let o = irsync(&server, &["allow", "data", &client_id]);
    assert!(o.status.success(), "{}", stderr(&o));

    // 6. The same sync now works.
    let o = irsync(&client, &sync_args);
    assert!(o.status.success(), "sync after allow failed: {}", stderr(&o));
    assert_eq!(std::fs::read(data.join("a.txt")).unwrap(), b"hello");

    // 7. Revoking takes effect on the next connection too.
    let o = irsync(&server, &["revoke", &client_id]);
    assert!(o.status.success(), "{}", stderr(&o));
    let o = irsync(&client, &sync_args);
    assert!(!o.status.success(), "sync after revoke should fail");
    assert!(stderr(&o).contains("connection lost"), "{}", stderr(&o));
    let log_now = std::fs::read_to_string(&log).unwrap();
    assert!(log_now.matches("unauthorized").count() >= 2, "{log_now}");
    assert!(daemon.0.try_wait().unwrap().is_none(), "daemon exited");

    // 8. The guard stops the daemon.
}
