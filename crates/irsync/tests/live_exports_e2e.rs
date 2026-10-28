//! `irsync export add` and `irsync grant add` against a running daemon: the daemon
//! re-reads exports.toml per connection, so no restart is needed.

mod common;

use common::{irsync_bin as irsync, spawn_daemon_binary};

fn stderr(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Start a daemon with no exports, then `irsync export add` and `irsync grant add`
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

    // 1-2. An isolated daemon with no exports; its stderr goes to a file.
    let (mut daemon, daemon_id, listen, log) = spawn_daemon_binary(&server);
    let warning = std::fs::read_to_string(&log).unwrap();
    assert!(warning.contains("no exports defined"), "{warning}");

    // 3. The client's id.
    let o = irsync(&client, &["id", "show"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let out = String::from_utf8(o.stdout).unwrap();
    let client_id = out.lines().nth(1).unwrap().to_string();

    // 4. Nothing is exported yet: the sync fails.
    let address = format!("127.0.0.1:{}", listen.port());
    let from = format!("{}/", src.display());
    // The real path on the server, no export name.
    let to = format!("{daemon_id}:{}/", data.display());
    let sync_args = ["-a", "--reconnect", "0", "--address", &address, &from, &to];
    let o = irsync(&client, &sync_args);
    assert!(!o.status.success(), "sync before add should fail");
    assert!(
        stderr(&o).contains("rejected this client's key"),
        "{}",
        stderr(&o)
    );
    assert!(
        stderr(&o).contains(&format!("irsync grant list {client_id}"))
            && stderr(&o).contains(&format!("irsync grant add EXPORT {client_id}")),
        "{}",
        stderr(&o)
    );
    let log_now = std::fs::read_to_string(&log).unwrap();
    assert!(log_now.contains("unauthorized"), "{log_now}");

    // 5. Export and authorize while the daemon runs.
    let o = irsync(
        &server,
        &["export", "add", &format!("data:{}", data.display())],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    let o = irsync(&server, &["grant", "add", "data", &client_id]);
    assert!(o.status.success(), "{}", stderr(&o));

    // 6. The same sync now works.
    let o = irsync(&client, &sync_args);
    assert!(
        o.status.success(),
        "sync after grant add failed: {}",
        stderr(&o)
    );
    assert_eq!(std::fs::read(data.join("a.txt")).unwrap(), b"hello");

    // 6b. With a grant, a path outside every export is refused at Hello,
    // with the same hint.
    let nowhere = format!("{daemon_id}:/nowhere/");
    let o = irsync(
        &client,
        &[
            "-a",
            "--reconnect",
            "0",
            "--address",
            &address,
            &from,
            &nowhere,
        ],
    );
    assert!(
        !o.status.success(),
        "push to an unexported path should fail"
    );
    assert!(
        stderr(&o).contains("no access to")
            && stderr(&o).contains(&nowhere)
            && stderr(&o).contains(&format!("irsync grant list {client_id}")),
        "{}",
        stderr(&o)
    );

    // 7. Removing the grant takes effect on the next connection too.
    let o = irsync(&server, &["grant", "remove", "all", &client_id]);
    assert!(o.status.success(), "{}", stderr(&o));
    let o = irsync(&client, &sync_args);
    assert!(!o.status.success(), "sync after grant remove should fail");
    assert!(
        stderr(&o).contains("rejected this client's key"),
        "{}",
        stderr(&o)
    );
    assert!(
        stderr(&o).contains(&format!("irsync grant list {client_id}"))
            && stderr(&o).contains(&format!("irsync grant add EXPORT {client_id}")),
        "{}",
        stderr(&o)
    );
    let log_now = std::fs::read_to_string(&log).unwrap();
    assert!(log_now.matches("unauthorized").count() >= 2, "{log_now}");
    assert!(daemon.0.try_wait().unwrap().is_none(), "daemon exited");

    // 8. The guard stops the daemon.
}
