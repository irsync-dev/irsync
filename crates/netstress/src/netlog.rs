//! Reads one iteration's irsync client log and counts what the fault layer
//! and irsync did, so the run loop can tell a real resume from a lucky pass.

/// What the client log says happened.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct NetEvents {
    /// "netsim active" was logged: the client really ran a fault build.
    pub active: bool,
    /// "netsim: dead zone open" lines.
    pub dead_zones: u32,
    /// Dead zones followed by a "session started" with `resumed=true` and the
    /// id of the first session.
    pub resumed: u32,
    /// "connection lost; reconnecting" lines outside a dead zone, that is not
    /// between a dead zone's open and the next "session started".
    pub stray_reconnects: u32,
}

/// Count the events in `log_text`. ANSI colour codes are stripped first.
pub fn read(log_text: &str) -> NetEvents {
    let mut events = NetEvents::default();
    let mut first_session: Option<String> = None;
    let mut in_dead_zone = false;
    for raw in log_text.lines() {
        let line = strip_ansi(raw);
        if line.contains("netsim active") {
            events.active = true;
        } else if line.contains("netsim: dead zone open") {
            events.dead_zones += 1;
            in_dead_zone = true;
        } else if line.contains("connection lost; reconnecting") {
            if !in_dead_zone {
                events.stray_reconnects += 1;
            }
        } else if let Some((_, after)) = line.split_once("session started") {
            // The span prefix before the message also carries `session_id=`
            // and `resumed=`; only the event's own fields count.
            let id = field(after, "session=");
            let resumed = field(after, "resumed=") == Some("true");
            if first_session.is_none() {
                first_session = id.map(str::to_owned);
            }
            if in_dead_zone && resumed && id.is_some() && id == first_session.as_deref() {
                events.resumed += 1;
            }
            in_dead_zone = false;
        }
    }
    events
}

impl NetEvents {
    /// None: fine. Some(reason): the iteration fails with this reason.
    pub fn verdict(&self, _has_outages: bool) -> Option<String> {
        if !self.active {
            Some("fault layer not active".to_owned())
        } else if self.stray_reconnects > 0 {
            Some(format!(
                "reconnect without a dead zone ({} time(s))",
                self.stray_reconnects
            ))
        } else if self.dead_zones > self.resumed {
            Some(format!(
                "dead zone did not end in a resumed session ({} dead zone(s), {} resumed)",
                self.dead_zones, self.resumed
            ))
        } else {
            None
        }
    }
}

/// The value of `key` (like `resumed=`) in `text`, up to the next space.
fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let rest = text.split_once(key)?.1;
    Some(rest.split_whitespace().next().unwrap_or(""))
}

/// Remove ESC `[` digits/semicolons `m` sequences.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("\x1b[") {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 2..];
        let end = tail.find(|c: char| !(c.is_ascii_digit() || c == ';'));
        match end {
            Some(e) if tail[e..].starts_with('m') => rest = &tail[e + 1..],
            _ => {
                out.push_str("\x1b[");
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "fc8db1da913aee9568abb1dcc89b8c11";

    fn fixture() -> String {
        let prefix = format!(
            "session{{direction=ClientSends side=\"client\" role=\"sender\" peer=ab source_index=0 session_id=\"{ID}\" resumed=false"
        );
        [
            "2026-11-06T10:00:00Z  WARN irsync::net::netsim: netsim active profile=4g-vehicle side=Client seed=7 stream=1".to_owned(),
            format!("2026-11-06T10:00:01Z  INFO {prefix}}}: irsync::session::engine: session started session={ID} resumed=false direction=ClientSends peer_version=0.1.0"),
            "2026-11-06T10:00:05Z  WARN irsync::net::netsim::transport: netsim: dead zone open secs=31".to_owned(),
            "2026-11-06T10:00:06Z  WARN irsync::session::reconnect: connection lost; reconnecting attempt=1".to_owned(),
            "2026-11-06T10:00:09Z  WARN irsync::session::reconnect: connection lost; reconnecting attempt=2".to_owned(),
            "2026-11-06T10:00:36Z  WARN irsync::net::netsim::transport: netsim: dead zone closed".to_owned(),
            // After the reconnect the span prefix holds both values; the event's own value must win.
            format!("2026-11-06T10:00:37Z  INFO {prefix} reconnects=1 session_id=\"{ID}\" resumed=true}}: irsync::session::engine: session started session={ID} resumed=true direction=ClientSends peer_version=0.1.0"),
        ]
        .join("\n")
    }

    /// Wrap level and target in colour codes, as tracing does.
    fn colour(log: &str) -> String {
        log.replace(" WARN ", " \x1b[33m WARN\x1b[0m ")
            .replace(" INFO ", " \x1b[32m INFO\x1b[0m ")
            .replace("irsync::", "\x1b[2mirsync::")
            .replace(": netsim", "\x1b[0m: netsim")
    }

    #[test]
    fn a_dead_zone_that_resumed() {
        assert_eq!(
            read(&fixture()),
            NetEvents {
                active: true,
                dead_zones: 1,
                resumed: 1,
                stray_reconnects: 0
            }
        );
        assert_eq!(read(&fixture()).verdict(true), None);
    }

    #[test]
    fn parses_a_coloured_log() {
        let coloured = colour(&fixture());
        assert!(coloured.contains('\x1b'));
        assert_eq!(read(&coloured), read(&fixture()));
    }

    #[test]
    fn a_new_session_after_a_dead_zone_is_not_a_resume() {
        // Only the event's field changes; the span prefix keeps resumed=true too.
        let log = fixture().replace(
            &format!("session started session={ID} resumed=true"),
            &format!("session started session={ID} resumed=false"),
        );
        let e = read(&log);
        assert_eq!(e.resumed, 0);
        assert!(e.verdict(true).is_some());
    }

    #[test]
    fn the_events_resumed_value_wins_over_the_span_prefix() {
        // Prefix says resumed=false first, event says resumed=true: that is a resume.
        let last = fixture().lines().last().unwrap().to_owned();
        assert!(last.find("resumed=false").unwrap() < last.find("session started").unwrap());
        assert_eq!(read(&fixture()).resumed, 1);
    }

    #[test]
    fn a_different_session_id_is_not_a_resume() {
        let log = fixture().replace(
            &format!("session started session={ID} resumed=true"),
            "session started session=00000000000000000000000000000000 resumed=true",
        );
        assert_eq!(read(&log).resumed, 0);
    }

    #[test]
    fn a_reconnect_outside_a_dead_zone_is_stray() {
        let log = format!(
            "{}\n2026-11-06T10:00:40Z  WARN irsync::session::reconnect: connection lost; reconnecting attempt=1",
            fixture()
        );
        let e = read(&log);
        assert_eq!(e.stray_reconnects, 1);
        assert!(e.verdict(true).is_some());
    }

    #[test]
    fn inactive_is_reported() {
        assert!(read("session started")
            .verdict(false)
            .unwrap()
            .contains("not active"));
    }
}
