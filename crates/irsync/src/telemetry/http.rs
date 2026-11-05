//! The debug HTTP port: `/metrics`, `/debug/timing` and `/debug/flame`.

use super::flame::FlameState;
use super::timing::TimingState;
use metrics_exporter_prometheus::PrometheusHandle;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

/// The request head may be this long.
const MAX_HEAD: usize = 8 * 1024;
/// How long the accept loop pauses after a failed accept (out of fds, say).
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

const PROMETHEUS: &str = "text/plain; version=0.0.4";
const PLAIN: &str = "text/plain; charset=utf-8";

/// What the server renders: the metrics recorder and the span sinks' state.
#[derive(Clone)]
pub struct Routes {
    pub prometheus: Option<PrometheusHandle>,
    pub timing: Arc<TimingState>,
    pub flame: Arc<FlameState>,
}

impl Routes {
    /// The status, content type and body for `GET path`.
    fn get(&self, path: &str) -> (u16, &'static str, String) {
        match path {
            "/" => (
                200,
                PLAIN,
                "/metrics       Prometheus metrics\n\
                 /debug/timing  wall and busy time per span\n\
                 /debug/flame   folded stacks, self time in microseconds\n"
                    .into(),
            ),
            "/metrics" => (
                200,
                PROMETHEUS,
                self.prometheus
                    .as_ref()
                    .map_or_else(|| "# no metrics recorder\n".into(), |h| h.render()),
            ),
            "/debug/timing" => (200, PLAIN, self.timing.render()),
            "/debug/flame" => (200, PLAIN, self.flame.render()),
            _ => (404, PLAIN, "not found\n".into()),
        }
    }
}

/// How long one connection may take and how many may be open at once.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Reading the request, answering it and draining the rest.
    pub conn_timeout: Duration,
    /// Connections beyond this many are closed unanswered.
    pub max_conns: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            conn_timeout: Duration::from_secs(10),
            max_conns: 16,
        }
    }
}

/// Binds `addr` and serves `routes` on it, a task per connection within
/// `limits`. Returns the bound address and the accept loop's handle.
pub async fn serve(
    addr: SocketAddr,
    routes: Routes,
    limits: Limits,
) -> io::Result<(SocketAddr, JoinHandle<()>)> {
    let listener = TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    let permits = Arc::new(Semaphore::new(limits.max_conns));
    let task = tokio::spawn(async move {
        let mut failing = false;
        loop {
            let stream = match listener.accept().await {
                Ok((stream, _)) => {
                    failing = false;
                    stream
                }
                Err(e) => {
                    accept_failed(&e, &mut failing).await;
                    continue;
                }
            };
            // All permits taken: dropping the stream closes it.
            let Ok(permit) = permits.clone().try_acquire_owned() else {
                continue;
            };
            let routes = routes.clone();
            tokio::spawn(async move {
                let _ = tokio::time::timeout(limits.conn_timeout, handle(stream, &routes)).await;
                drop(permit);
            });
        }
    });
    Ok((bound, task))
}

/// Pauses the accept loop after `e`, logging the first error of a run of them.
async fn accept_failed(e: &io::Error, failing: &mut bool) {
    if !*failing {
        tracing::debug!("debug port: accept: {e}");
        *failing = true;
    }
    tokio::time::sleep(ACCEPT_BACKOFF).await;
}

/// The request head (up to the blank line), or the status to refuse with.
async fn read_head(stream: &mut TcpStream) -> Result<Vec<u8>, Option<u16>> {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        if head.windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(head);
        }
        if head.len() > MAX_HEAD {
            return Err(Some(431));
        }
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return Err(None),
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
}

async fn handle(mut stream: TcpStream, routes: &Routes) -> io::Result<()> {
    let head = match read_head(&mut stream).await {
        Ok(h) => h,
        Err(Some(code)) => return respond(&mut stream, code, PLAIN, "header too large\n").await,
        Err(None) => return Ok(()),
    };
    let line = head.split(|b| *b == b'\r').next().unwrap_or_default();
    let line = String::from_utf8_lossy(line);
    let mut parts = line.split(' ');
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    if method != "GET" {
        return respond(&mut stream, 405, PLAIN, "method not allowed\n").await;
    }
    let path = target.split('?').next().unwrap_or("");
    let (code, ctype, body) = routes.get(path);
    respond(&mut stream, code, ctype, &body).await
}

/// The reason phrase for a status this server sends (empty, which HTTP
/// allows, for any other).
fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        431 => "Request Header Fields Too Large",
        _ => "",
    }
}

async fn respond(stream: &mut TcpStream, code: u16, ctype: &str, body: &str) -> io::Result<()> {
    let reason = reason(code);
    let allow = if code == 405 { "Allow: GET\r\n" } else { "" };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\n{allow}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await?;
    // Read what the client still sends, so closing does not reset the
    // connection under a response it has not read yet.
    let _ = tokio::time::timeout(
        Duration::from_millis(200),
        tokio::io::copy(stream, &mut tokio::io::sink()),
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn request(addr: SocketAddr, raw: &[u8]) -> String {
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(raw).await.unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).await.unwrap();
        String::from_utf8(out).unwrap()
    }

    fn routes() -> Routes {
        Routes {
            prometheus: Some(
                metrics_exporter_prometheus::PrometheusBuilder::new()
                    .build_recorder()
                    .handle(),
            ),
            timing: Default::default(),
            flame: Default::default(),
        }
    }

    #[test]
    fn every_status_gets_its_own_reason() {
        assert_eq!(reason(431), "Request Header Fields Too Large");
        assert_eq!(reason(404), "Not Found");
        // Never another status's phrase.
        assert_eq!(reason(500), "");
    }

    #[tokio::test]
    async fn routes_and_errors() {
        let (addr, _task) = serve("127.0.0.1:0".parse().unwrap(), routes(), Limits::default())
            .await
            .unwrap();

        // A client that never finishes its head does not hold up the others.
        let mut slow = TcpStream::connect(addr).await.unwrap();
        slow.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
        let r = tokio::time::timeout(
            Duration::from_secs(1),
            request(addr, b"GET /metrics HTTP/1.1\r\n\r\n"),
        )
        .await
        .expect("answered within 1 s");
        assert!(r.starts_with("HTTP/1.1 200"), "{r}");
        assert!(
            r.contains("Content-Type: text/plain; version=0.0.4\r\n"),
            "{r}"
        );
        assert!(r.contains("Connection: close\r\n"));

        let r = request(addr, b"GET / HTTP/1.1\r\n\r\n").await;
        assert!(
            r.contains("charset=utf-8") && r.contains("/debug/flame"),
            "{r}"
        );
        let r = request(addr, b"POST / HTTP/1.1\r\n\r\n").await;
        assert!(r.starts_with("HTTP/1.1 405"), "{r}");
        let r = request(addr, b"GET /nope HTTP/1.1\r\n\r\n").await;
        assert!(r.starts_with("HTTP/1.1 404"), "{r}");
        let big = format!("GET / HTTP/1.1\r\nX-Pad: {}\r\n\r\n", "a".repeat(9 * 1024));
        let r = request(addr, big.as_bytes()).await;
        assert!(r.starts_with("HTTP/1.1 431"), "{r}");
        drop(slow);
    }

    #[tokio::test]
    async fn busy_server_closes_new_connections_until_idle_ones_time_out() {
        let limits = Limits {
            conn_timeout: Duration::from_millis(500),
            max_conns: 16,
        };
        let (addr, _task) = serve("127.0.0.1:0".parse().unwrap(), routes(), limits)
            .await
            .unwrap();
        let mut idle = Vec::new();
        for _ in 0..16 {
            idle.push(TcpStream::connect(addr).await.unwrap());
        }

        // No permit left: closed unanswered (an EOF or a reset).
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _ = s.write_all(b"GET /metrics HTTP/1.1\r\n\r\n").await;
        let mut out = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(1), s.read_to_end(&mut out))
            .await
            .expect("closed within 1 s");
        assert!(out.is_empty(), "{read:?} {}", String::from_utf8_lossy(&out));

        // The idle ones time out and are closed; their permits come back.
        for mut s in idle {
            let mut out = Vec::new();
            let n = tokio::time::timeout(Duration::from_secs(2), s.read_to_end(&mut out))
                .await
                .expect("idle connection closed after the timeout");
            assert!(out.is_empty(), "{n:?}");
        }
        let r = tokio::time::timeout(
            Duration::from_secs(1),
            request(addr, b"GET /metrics HTTP/1.1\r\n\r\n"),
        )
        .await
        .expect("answered within 1 s");
        assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    }

    #[tokio::test(start_paused = true)]
    async fn failed_accept_backs_off() {
        let e = io::Error::other("too many open files");
        let mut failing = false;
        let t = tokio::time::Instant::now();
        accept_failed(&e, &mut failing).await;
        assert!(failing);
        accept_failed(&e, &mut failing).await;
        assert_eq!(t.elapsed(), 2 * ACCEPT_BACKOFF);
    }
}
