//! The debug HTTP port on the real binaries: the client waits for a signal
//! after its sync, the daemon announces its port.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Kills the child when the test ends, pass or fail.
struct Guard(Child);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn forward_lines<R: Read + Send + 'static>(r: R, tx: mpsc::Sender<String>) {
    std::thread::spawn(move || {
        for line in BufReader::new(r).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
}

/// The next line from `rx` that has `prefix` (after ANSI colours are
/// dropped) and what follows it; panics after 15 s.
fn line_with(rx: &mpsc::Receiver<String>, prefix: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = rx
            .recv_timeout(left)
            .unwrap_or_else(|_| panic!("no line with {prefix:?} within 15 s"));
        if let Some(i) = line.find(prefix) {
            return line[i + prefix.len()..].trim().to_string();
        }
    }
}

fn get(addr: SocketAddr, path: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\n\r\n").unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

fn irsync(tmp: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_irsync"));
    c.env("HOME", tmp.join("home"))
        .env("XDG_STATE_HOME", tmp.join("xdg"))
        .env_remove("IRSYNC_PASSPHRASE")
        .stdin(Stdio::null());
    c
}

struct Daemon {
    _guard: Guard,
    out: mpsc::Receiver<String>,
    id: String,
    listen: SocketAddr,
}

/// A daemon on localhost exporting `m` at `tmp/dst` to the client's
/// identity; `debug_addr` goes into `[daemon]`.
fn start_daemon(tmp: &Path, debug_addr: Option<&str>) -> Daemon {
    for d in ["src", "dst", "home", "xdg"] {
        std::fs::create_dir_all(tmp.join(d)).unwrap();
    }
    std::fs::write(tmp.join("src/a.txt"), b"hello").unwrap();
    let config = tmp.join("irsync.toml");
    let o = irsync(tmp)
        .args(["id", "show", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let client_id = String::from_utf8(o.stdout)
        .unwrap()
        .lines()
        .nth(1)
        .unwrap()
        .to_string();
    let debug = debug_addr.map_or(String::new(), |a| format!("debug_addr = {a:?}\n"));
    std::fs::write(
        &config,
        format!(
            "state_dir = {:?}\n[daemon]\nidentity = {:?}\n{debug}\
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
            "[exports.m]\npath = {:?}\nrw = [\"{client_id}\"]\n",
            tmp.join("dst")
        ),
    )
    .unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_irsync"))
        .args(["daemon", "--config"])
        .arg(&config)
        .args(["--listen", "127.0.0.1:0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut guard = Guard(child);
    let (tx, out) = mpsc::channel();
    forward_lines(guard.0.stdout.take().unwrap(), tx);
    let id = line_with(&out, "endpoint id: ");
    let listen = line_with(&out, "listening on: ").parse().unwrap();
    Daemon {
        _guard: guard,
        out,
        id,
        listen,
    }
}

/// The client command line for a sync into the daemon's export.
fn client(tmp: &Path, d: &Daemon, extra: &[&str]) -> Command {
    let mut c = irsync(tmp);
    c.args(extra)
        .args(["-a", "--reconnect", "0", "--address"])
        .arg(format!("localhost:{}", d.listen.port()))
        .arg("--config")
        .arg(tmp.join("irsync.toml"))
        .arg(format!("{}/", tmp.join("src").display()))
        .arg(format!("{}:m/", d.id));
    c
}

#[test]
fn client_waits_until_sigint() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path();
    let d = start_daemon(tmp, None);
    let child = client(tmp, &d, &["--debug-addr", "127.0.0.1:0"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut guard = Guard(child);
    let (tx, rx) = mpsc::channel();
    forward_lines(guard.0.stderr.take().unwrap(), tx);
    let addr: SocketAddr = line_with(&rx, "irsync: debug port open at ")
        .trim_end_matches("; Ctrl-C to exit")
        .parse()
        .unwrap();

    let metrics = get(addr, "/metrics");
    assert!(
        metrics.contains("irsync_busy_microseconds_total{"),
        "{metrics}"
    );
    assert!(metrics.contains("irsync_files_total{"), "{metrics}");
    assert!(get(addr, "/debug/timing").contains("session"));
    assert!(
        guard.0.try_wait().unwrap().is_none(),
        "exited without a signal"
    );

    let pid = nix::unistd::Pid::from_raw(guard.0.id() as i32);
    nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGINT).unwrap();
    let status = guard.0.wait().unwrap();
    assert_eq!(status.code(), Some(0));
    assert_eq!(std::fs::read(tmp.join("dst/a.txt")).unwrap(), b"hello");
}

#[test]
fn no_wait_exits() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path();
    let d = start_daemon(tmp, None);
    let o = client(
        tmp,
        &d,
        &["--debug-addr", "127.0.0.1:0", "--debug-port-no-wait"],
    )
    .output()
    .unwrap();
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn port_in_use_warns_and_continues() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path();
    let d = start_daemon(tmp, None);
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = taken.local_addr().unwrap().to_string();
    let child = client(tmp, &d, &["--debug-addr", &addr])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut guard = Guard(child);
    let (tx, rx) = mpsc::channel();
    forward_lines(guard.0.stderr.take().unwrap(), tx);
    // Without the fix the client would wait for a signal: no port is open.
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = guard.0.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = guard.0.kill();
            panic!("client still running after 30 s");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let err: Vec<String> = rx.iter().collect();
    let err = err.join("\n");
    assert_eq!(status.code(), Some(0), "{err}");
    let warnings: Vec<_> = err.lines().filter(|l| l.contains("debug port")).collect();
    assert_eq!(warnings.len(), 1, "{err}");
    assert!(
        warnings[0].contains(&format!("debug port {addr}: ")),
        "{err}"
    );
}

#[test]
fn daemon_debug_port() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path();
    let d = start_daemon(tmp, Some("127.0.0.1:0"));
    let addr: SocketAddr = line_with(&d.out, "debug port: ").parse().unwrap();
    let o = client(tmp, &d, &[]).output().unwrap();
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let m = get(addr, "/metrics");
        if m.contains("side=\"daemon\"") {
            break;
        }
        assert!(Instant::now() < deadline, "no daemon-side metrics:\n{m}");
        std::thread::sleep(Duration::from_millis(200));
    }
}
