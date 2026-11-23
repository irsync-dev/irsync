//! The comments of `irsync.toml`, attached to the config message.
//!
//! `toml_edit` keeps every comment in the decoration of a table header,
//! key, value or array element. [`attach`] walks the file in that form and
//! puts each comment line where the writer puts it back:
//!
//! - lines above a key, or between a table header and its first key: the
//!   key's `above` (blank lines dropped); the rest of its line: its
//!   `trailing`;
//! - the text before a table header: after its last blank line, the
//!   table's `above`; up to it, the `tail` of the table before (of the
//!   top level before the first table), or the file's `header` when only
//!   comments came before; the rest of the header line: its `trailing`;
//! - lines above an array element: its `above`; the rest of its line,
//!   after the comma: its `trailing`; lines after the last element: the
//!   last element's `above` (the key's, for an empty array);
//! - the text after the last key of the file: up to its first blank
//!   line, the `tail` of the last table (of the top level when the file
//!   has top-level keys and no table header); the rest: the file's
//!   `footer`. In a file of comments only, all of it is the footer.
//!
//! A comment is the whole line from `#` on. The comments of a bare
//! `[hosts]` or `[exports]` header, or of a top-level `hosts = { … }`
//! key, go to its first entry, without blank lines. A comment with
//! no place in the message goes to the footer: none is dropped.

use super::{line_of, ConfigError};
use irsync_proto::config as pb;
use std::collections::BTreeMap;
use std::path::Path;
use toml_edit::{DocumentMut, Item, RawString, TableLike, Value};

/// Fills the comment fields of `msg`, which [`super::reader::read_values`]
/// read from `text`. `path` is the file named in errors.
///
/// `toml_edit` should take every text `toml` took; if it does not, this
/// is an error rather than a message without comments, which the next
/// save would write without them.
pub(crate) fn attach(text: &str, path: &Path, msg: &mut pb::Config) -> Result<(), ConfigError> {
    let doc = text.parse::<DocumentMut>().map_err(|e| ConfigError::File {
        path: path.to_path_buf(),
        line: e.span().map(|s| line_of(text, s.start)),
        message: e.message().to_string(),
    })?;
    let mut walk = Walk {
        lines: Vec::new(),
        at_start: true,
    };
    walk.document(&doc);
    for (place, line) in walk.lines {
        put(msg, place, line);
    }
    Ok(())
}

/// What a comment belongs to: a table by its path, a key in a table, or
/// an element of the array under a key.
#[derive(Clone)]
enum Spot {
    Table(Vec<String>),
    Key(Vec<String>, String),
    Entry(Vec<String>, String, usize),
}

/// Where a line goes in the message.
#[derive(Clone)]
enum Place {
    Header,
    Footer,
    Tail(Vec<String>),
    Above(Spot),
    Trailing(Spot),
}

/// The comment lines of a file, each with its place, in file order.
struct Walk {
    lines: Vec<(Place, String)>,
    /// Nothing but comments came before.
    at_start: bool,
}

impl Walk {
    fn document(&mut self, doc: &DocumentMut) {
        self.keys(doc.as_table(), &[]);
        let mut headers = Vec::new();
        tables_with_headers(doc.as_table(), &[], &mut headers);
        headers.sort_by_key(|(_, t)| t.position());
        let mut before: Option<Vec<String>> = None;
        for (path, table) in headers {
            let lines = lines_of(raw(table.decor().prefix()));
            let (up_to_blank, after_blank) = split_at_last_blank(&lines);
            let place = match before {
                Some(p) => Place::Tail(p),
                None if self.at_start => Place::Header,
                None => Place::Tail(Vec::new()),
            };
            self.with_blanks(up_to_blank, &place);
            self.comments(after_blank, &Place::Above(Spot::Table(path.clone())));
            self.at_start = false;
            self.same_line(
                raw(table.decor().suffix()),
                &Place::Trailing(Spot::Table(path.clone())),
            );
            self.keys(table, &path);
            before = Some(path);
        }
        // Up to the end of the file, which may lack a final newline.
        let rest: Vec<&str> = raw(Some(doc.trailing()))
            .split('\n')
            .map(str::trim)
            .collect();
        // The lines right after the last key belong to its table, as the
        // writer writes a tail; those after a blank line, to the file.
        let tail = match before {
            Some(p) => Some(Place::Tail(p)),
            None if has_top_keys(doc) => Some(Place::Tail(Vec::new())),
            None => None,
        };
        let (own, footer) = match tail {
            Some(_) if !self.at_start => split_at_first_blank(&rest),
            _ => (&[][..], &rest[..]),
        };
        if let Some(place) = tail {
            self.with_blanks(own, &place);
        }
        self.with_blanks(trim_blanks(footer), &Place::Footer);
    }

    /// The keys of a table with a header (or of the top level), in order.
    /// Tables with headers of their own are walked by [`Self::document`].
    fn keys(&mut self, table: &dyn TableLike, path: &[String]) {
        for (name, item) in table.iter() {
            match item {
                Item::Table(t) if t.is_dotted() => self.keys(t, &child(path, name)),
                Item::Value(v) => {
                    let spot = Spot::Key(path.to_vec(), name.to_owned());
                    if let Some(key) = table.key(name) {
                        self.above_key(raw(key.leaf_decor().prefix()), spot.clone());
                    }
                    self.value(v, path, name);
                    self.same_line(raw(v.decor().suffix()), &Place::Trailing(spot));
                }
                _ => {}
            }
        }
    }

    /// The lines above a key, which start at the start of a line.
    fn above_key(&mut self, prefix: &str, spot: Spot) {
        let lines = lines_of(prefix);
        let own = if std::mem::take(&mut self.at_start) {
            let (up_to_blank, after_blank) = split_at_last_blank(&lines);
            self.with_blanks(up_to_blank, &Place::Header);
            after_blank
        } else {
            &lines[..]
        };
        self.comments(own, &Place::Above(spot));
    }

    /// The comments inside an array or an inline table.
    fn value(&mut self, value: &Value, path: &[String], name: &str) {
        match value {
            Value::Array(a) => {
                let items = a
                    .iter()
                    .enumerate()
                    .map(|(i, e)| {
                        (
                            Spot::Entry(path.to_vec(), name.to_owned(), i),
                            raw(e.decor().prefix()),
                            raw(e.decor().suffix()),
                        )
                    })
                    .collect();
                let empty = Spot::Key(path.to_vec(), name.to_owned());
                self.sequence(items, raw(Some(a.trailing())), empty, |_, _| {});
            }
            Value::InlineTable(t) => {
                let inner = child(path, name);
                let values: Vec<(&str, &Value)> = t.iter().collect();
                let items = values
                    .iter()
                    .map(|(k, v)| {
                        let prefix = t.key(k).map_or("", |key| raw(key.leaf_decor().prefix()));
                        (
                            Spot::Key(inner.clone(), (*k).to_owned()),
                            prefix,
                            raw(v.decor().suffix()),
                        )
                    })
                    .collect();
                let empty = Spot::Table(inner.clone());
                self.sequence(items, raw(Some(t.trailing())), empty, |w, i| {
                    let (k, v) = values[i];
                    w.value(v, &inner, k);
                });
            }
            _ => {}
        }
    }

    /// The items of an array or inline table, each with the text before
    /// and after it. Text before an item starts on the line of the one
    /// before (or of the opening bracket); `trailing` is the text before
    /// the closing bracket, and `empty` takes it when there are no items.
    fn sequence(
        &mut self,
        items: Vec<(Spot, &str, &str)>,
        trailing: &str,
        empty: Spot,
        mut inner: impl FnMut(&mut Self, usize),
    ) {
        let spots: Vec<Spot> = items.iter().map(|(s, _, _)| s.clone()).collect();
        for (i, (spot, prefix, suffix)) in items.into_iter().enumerate() {
            let (first, lines) = split_first_line(prefix);
            let place = match i {
                0 => Place::Above(spot.clone()),
                _ => Place::Trailing(spots[i - 1].clone()),
            };
            self.comments(&[first], &place);
            self.comments(&lines, &Place::Above(spot.clone()));
            inner(self, i);
            // After the item, before its comma.
            let (first, lines) = split_first_line(suffix);
            self.comments(&[first], &Place::Trailing(spot.clone()));
            let next = spots.get(i + 1).unwrap_or(&spot).clone();
            self.comments(&lines, &Place::Above(next));
        }
        let (first, lines) = split_first_line(trailing);
        match spots.last() {
            None => {
                self.comments(&[first], &Place::Above(empty.clone()));
                self.comments(&lines, &Place::Above(empty));
            }
            Some(last) => {
                self.comments(&[first], &Place::Trailing(last.clone()));
                self.comments(&lines, &Place::Above(last.clone()));
            }
        }
    }

    /// The comment at the end of a line, if any.
    fn same_line(&mut self, text: &str, place: &Place) {
        self.comments(&[text.trim()], place);
    }

    /// The comments among `lines`, without the blank ones.
    fn comments(&mut self, lines: &[&str], place: &Place) {
        for line in lines.iter().filter(|l| !l.is_empty()) {
            self.lines.push((place.clone(), (*line).to_owned()));
        }
    }

    /// `lines` with the blank ones, as `""`.
    fn with_blanks(&mut self, lines: &[&str], place: &Place) {
        for line in lines {
            self.lines.push((place.clone(), (*line).to_owned()));
        }
    }
}

/// Every table written as a header, with its path, below `table`.
fn tables_with_headers<'a>(
    table: &'a toml_edit::Table,
    path: &[String],
    out: &mut Vec<(Vec<String>, &'a toml_edit::Table)>,
) {
    for (name, item) in table.iter() {
        if let Item::Table(t) = item {
            let path = child(path, name);
            if !t.is_implicit() && !t.is_dotted() {
                out.push((path.clone(), t));
            }
            tables_with_headers(t, &path, out);
        }
    }
}

fn child(path: &[String], name: &str) -> Vec<String> {
    let mut p = path.to_vec();
    p.push(name.to_owned());
    p
}

fn raw(text: Option<&RawString>) -> &str {
    text.and_then(RawString::as_str).unwrap_or("")
}

/// The whole lines of text that ends where an item starts, trimmed; a
/// blank line is `""`. The last piece, the indentation of the item, is
/// not a line.
fn lines_of(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').map(str::trim).collect();
    lines.pop();
    lines
}

/// Text that starts after an item on its line: the rest of that line, and
/// the whole lines after it.
fn split_first_line(text: &str) -> (&str, Vec<&str>) {
    match text.split_once('\n') {
        Some((first, rest)) => (first.trim(), lines_of(rest)),
        None => (text.trim(), Vec::new()),
    }
}

/// The lines before the first blank one, and the lines from it on.
fn split_at_first_blank<'a, 'b>(lines: &'b [&'a str]) -> (&'b [&'a str], &'b [&'a str]) {
    lines.split_at(
        lines
            .iter()
            .position(|l| l.is_empty())
            .unwrap_or(lines.len()),
    )
}

/// True when the top level has a key the writer writes there, above the
/// first table: a value that is not an inline table.
fn has_top_keys(doc: &DocumentMut) -> bool {
    doc.iter()
        .any(|(_, item)| matches!(item, Item::Value(v) if !v.is_inline_table()))
}

/// The lines up to the last blank one, without blank lines at their
/// ends, and the lines after it.
fn split_at_last_blank<'a, 'b>(lines: &'b [&'a str]) -> (&'b [&'a str], &'b [&'a str]) {
    match lines.iter().rposition(|l| l.is_empty()) {
        Some(i) => (trim_blanks(&lines[..i]), &lines[i + 1..]),
        None => (&[], lines),
    }
}

fn trim_blanks<'a, 'b>(lines: &'b [&'a str]) -> &'b [&'a str] {
    let start = lines
        .iter()
        .position(|l| !l.is_empty())
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|l| !l.is_empty())
        .map_or(start, |i| i + 1);
    &lines[start..end]
}

/// The comment fields of one table of the message, and its string arrays
/// by key.
struct Parts<'a> {
    comment: Option<&'a mut Option<pb::Comment>>,
    keys: &'a mut BTreeMap<String, pb::Comment>,
    tail: &'a mut Vec<String>,
    arrays: Vec<(&'static str, &'a mut Vec<pb::Entry>)>,
}

macro_rules! parts {
    ($table:expr $(, $key:literal => $field:ident)*) => {{
        let t = $table;
        Parts {
            comment: Some(&mut t.comment),
            keys: &mut t.key_comments,
            tail: &mut t.tail,
            arrays: vec![$(($key, &mut t.$field)),*],
        }
    }};
}

/// The table of the message at `path`, as the file names it.
fn table<'a>(c: &'a mut pb::Config, path: &[String]) -> Option<Parts<'a>> {
    let path: Vec<&str> = path.iter().map(String::as_str).collect();
    Some(match path[..] {
        [] => Parts {
            comment: None,
            keys: &mut c.key_comments,
            tail: &mut c.tail,
            arrays: Vec::new(),
        },
        ["client"] => parts!(c.client.as_mut()?),
        ["daemon"] => parts!(c.daemon.as_mut()?, "listen" => listen),
        ["daemon", "discovery"] => parts!(c.daemon.as_mut()?.discovery.as_mut()?),
        ["telemetry"] => parts!(c.telemetry.as_mut()?),
        ["all"] => parts!(c.all.as_mut()?, "rw" => rw, "ro" => ro),
        ["immediate"] => parts!(c.immediate.as_mut()?, "rw" => rw, "ro" => ro),
        ["hosts", name] => parts!(
            c.hosts.iter_mut().find(|h| h.name == name)?,
            "address" => address,
            "ssh_aliases" => ssh_aliases
        ),
        ["exports", name] => parts!(
            c.exports.iter_mut().find(|e| e.name == name)?,
            "rw" => rw,
            "ro" => ro
        ),
        _ => return None,
    })
}

/// A key whose value is a table, inline or dotted, has the comments of
/// that table. A bare `[hosts]` or `[exports]` gives its comments to its
/// first entry.
fn redirect(c: &mut pb::Config, place: Place) -> Place {
    let place = match place {
        // `hosts = { … }` at the top level is the group's header.
        Place::Above(Spot::Key(p, k)) if p.is_empty() && is_group(&k) => {
            Place::Above(Spot::Table(vec![k]))
        }
        Place::Trailing(Spot::Key(p, k)) if p.is_empty() && is_group(&k) => {
            Place::Trailing(Spot::Table(vec![k]))
        }
        Place::Above(Spot::Key(p, k)) if table(c, &child(&p, &k)).is_some() => {
            Place::Above(Spot::Table(child(&p, &k)))
        }
        Place::Trailing(Spot::Key(p, k)) if table(c, &child(&p, &k)).is_some() => {
            Place::Trailing(Spot::Table(child(&p, &k)))
        }
        other => other,
    };
    let group = match &place {
        Place::Tail(p) | Place::Above(Spot::Table(p)) | Place::Trailing(Spot::Table(p)) => p,
        _ => return place,
    };
    let first = match group.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["hosts"] => c.hosts.first().map(|h| h.name.clone()),
        ["exports"] => c.exports.first().map(|e| e.name.clone()),
        _ => return place,
    };
    match first {
        Some(name) => Place::Above(Spot::Table(child(group, &name))),
        None => place,
    }
}

fn is_group(name: &str) -> bool {
    name == "hosts" || name == "exports"
}

enum Slot<'a> {
    Lines(&'a mut Vec<String>),
    Above(&'a mut pb::Comment),
    Trailing(&'a mut pb::Comment),
}

fn slot<'a>(c: &'a mut pb::Config, place: &Place) -> Option<Slot<'a>> {
    Some(match place {
        Place::Header => Slot::Lines(&mut c.header),
        Place::Footer => Slot::Lines(&mut c.footer),
        Place::Tail(p) => Slot::Lines(table(c, p)?.tail),
        Place::Above(spot) => Slot::Above(comment(c, spot)?),
        Place::Trailing(spot) => Slot::Trailing(comment(c, spot)?),
    })
}

fn comment<'a>(c: &'a mut pb::Config, spot: &Spot) -> Option<&'a mut pb::Comment> {
    match spot {
        Spot::Table(p) => Some(table(c, p)?.comment?.get_or_insert_with(Default::default)),
        Spot::Key(p, k) => Some(table(c, p)?.keys.entry(k.clone()).or_default()),
        Spot::Entry(p, k, i) => {
            let (_, entries) = table(c, p)?
                .arrays
                .into_iter()
                .find(|(name, _)| name == k)?;
            Some(
                entries
                    .get_mut(*i)?
                    .comment
                    .get_or_insert_with(Default::default),
            )
        }
    }
}

fn put(c: &mut pb::Config, place: Place, line: String) {
    let place = redirect(c, place);
    match slot(c, &place) {
        Some(Slot::Lines(lines)) => lines.push(line),
        // A tail redirected to an entry's `above` has blank lines; an
        // `above` holds none.
        Some(Slot::Above(_)) if line.is_empty() => {}
        Some(Slot::Above(comment)) => comment.above.push(line),
        Some(Slot::Trailing(comment)) if comment.trailing.is_empty() => comment.trailing = line,
        // A second comment for the same line, as after an element and
        // again after its comma.
        Some(Slot::Trailing(comment)) => comment.above.push(line),
        None => c.footer.push(line),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_toml_edit_refuses_is_an_error() {
        let mut msg = pb::Config::default();
        let e = attach("[daemon]\nportmap = \n", Path::new("irsync.toml"), &mut msg).unwrap_err();
        let ConfigError::File {
            path,
            line,
            message,
        } = &e
        else {
            panic!("{e:?}");
        };
        assert_eq!(path, Path::new("irsync.toml"));
        assert_eq!(*line, Some(2));
        assert!(!message.is_empty());
        assert!(e.to_string().starts_with("irsync.toml:2: "), "{e}");
    }
}
