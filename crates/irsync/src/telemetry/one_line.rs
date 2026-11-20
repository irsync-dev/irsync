//! Keeps every log event on one line.
//!
//! A log field can hold text from the other side: an error message, a
//! version string, a path. `tracing-subscriber` escapes the characters that
//! drive a terminal in it, but not line breaks. A peer could so end a log
//! line and start one of its own making, for example a line that looks
//! like the daemon accepting or refusing someone. The writer here turns
//! every line break inside an event into `\012` or `\015`.

use std::io;

use tracing_subscriber::fmt::MakeWriter;

/// Wraps a log writer: see the module docs.
#[derive(Debug, Clone)]
pub struct OneLine<W>(pub W);

impl<'a, W: MakeWriter<'a>> MakeWriter<'a> for OneLine<W> {
    type Writer = OneLineWriter<W::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        OneLineWriter(self.0.make_writer())
    }

    fn make_writer_for(&'a self, meta: &tracing::Metadata<'_>) -> Self::Writer {
        OneLineWriter(self.0.make_writer_for(meta))
    }
}

/// The writer of one event. The fmt layer hands it the whole event in one
/// `write`, ending in the event's own line break.
#[derive(Debug)]
pub struct OneLineWriter<W>(W);

impl<W: io::Write> io::Write for OneLineWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let body = buf.strip_suffix(b"\n").unwrap_or(buf);
        if !body.iter().any(|b| matches!(b, b'\n' | b'\r')) {
            self.0.write_all(buf)?;
            return Ok(buf.len());
        }
        let mut line = Vec::with_capacity(buf.len() + 16);
        for &b in body {
            match b {
                b'\n' => line.extend_from_slice(br"\012"),
                b'\r' => line.extend_from_slice(br"\015"),
                b => line.push(b),
            }
        }
        line.extend_from_slice(&buf[body.len()..]);
        self.0.write_all(&line)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Sink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Sink {
        type Writer = Sink;
        fn make_writer(&'a self) -> Sink {
            self.clone()
        }
    }

    /// What the fmt layer writes for the events `f` logs.
    fn logged(f: impl FnOnce()) -> String {
        let sink = Sink::default();
        let sub = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(OneLine(sink.clone()))
            .finish();
        tracing::subscriber::with_default(sub, f);
        let bytes = sink.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn text_from_a_peer_cannot_start_a_log_line() {
        let forged = "no such file\n2026-11-20T10:00:00Z  WARN irsync: accepted key abc\r\nmore";
        let out = logged(|| {
            tracing::warn!(error = %forged, "cannot send file");
            tracing::info!("{forged}");
            tracing::info!(path = ?forged, "plain");
        });
        assert_eq!(out.lines().count(), 3, "{out}");
        assert_eq!(out.matches('\n').count(), 3, "{out}");
        assert!(!out.contains('\r'), "{out}");
        for line in out.lines() {
            assert!(line.contains("irsync"), "{line}");
        }
        assert!(
            out.contains(
                r"no such file\0122026-11-20T10:00:00Z  WARN irsync: accepted key abc\015\012more"
            ),
            "{out}"
        );
    }

    #[test]
    fn a_line_without_breaks_passes_as_it_is() {
        let out = logged(|| tracing::info!(n = 3, "caf\u{e9} done"));
        assert!(out.ends_with("caf\u{e9} done n=3\n"), "{out}");
    }
}
