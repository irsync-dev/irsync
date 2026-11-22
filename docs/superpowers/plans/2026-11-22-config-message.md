# Config as a Message (Stage 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The config lives in one protobuf message that carries its comments; a reader fills it from `irsync.toml`, a writer builds the file from it, and every management command edits the message.

**Architecture:** `proto/irsync/config/config.proto` defines the message. `config/reader.rs` and `config/comments.rs` turn file text into it, `config/writer.rs` turns it back into text, and `Config::from_message` builds the typed `Config` the rest of irsync already uses. `manage/` is rebuilt on the message beside the old code in `manage/msg/`, then swapped in. The file layout (`[hosts]`) does not change in this stage.

**Tech Stack:** Rust, prost 0.14 (built with protox in `crates/irsync-proto`), `toml` for values and line numbers, `toml_edit` for comments.

**Spec:** `docs/superpowers/specs/2026-11-22-config-message-and-server-client-split-design.md` (this plan is its "Stage 1"; stage 2 gets a plan of its own once this has merged).

## Global Constraints

- No change in what irsync does, except that a save writes the file in the writer's form.
- Every error message and line number of today's `Config::parse` stays. `crates/irsync/tests/config_files.rs` pins them: it must pass unchanged, in every task from Task 3 on.
- The message is never encoded. Field numbers may change freely; say so in the `.proto` header.
- Every key that may be absent from the file is `optional` in the message. Defaults are applied in `Config::from_message`, never in the reader, and the writer writes only what is set.
- Values, not spellings: durations as `google.protobuf.Duration`, ids as 32 bytes, enums as enums. Paths are kept as written (`~` stays `~`).
- Hosts and exports keep their order from the file. A new entry goes last.
- A comment is stored as its whole line from `#` on, without indentation: `"# the nas"`.
- After Task 10, `toml` and `toml_edit` are used for the config only in `config/reader.rs`, `config/comments.rs` and `config/writer.rs`.
- No `unsafe`. If `Cargo.lock` changes, run `tools/sync-fuzz-lock.sh` and commit both lock files.
- Everyday test run: `cargo test -q --workspace --all-features --no-fail-fast`. Before the push: `tools/ci.sh check`.
- Commit messages: a short summary line, a blank line, then why, if it is not obvious. No attribution lines or trailers of any kind.

## Review Focus

Inputs the spec implies but does not spell out. Each has its test in the task named.

1. **A comment where the placement rules name no home** (after the last element of a multi-line array, between a table header and its first key, in a blank-line-separated block between two tables): it must still be in the written file. Task 5, `odd_comment_places_survive`.
2. **A `#` inside a quoted string** (`ssh = "me@host#1"`) is part of the value, not a comment. Task 4, `hash_in_a_string_is_not_a_comment`.
3. **Names that need quotes** (`[exports."www.example.com"]`, a host alias with a space or a non-ASCII letter): read, written with quotes, read back equal. Task 5, `quoted_names_round_trip`.
4. **Edges of the file itself:** empty, comments only, no newline at the end, CRLF line ends. Each reads, and writes text that reads back to the same message. Task 5, `file_edges_round_trip`.
5. **Layouts with no table header** (`hosts = { nas = { endpoint_id = "…" } }`, `daemon.portmap = false`): read into the same message as the plain form, comments kept, written as plain tables. Task 5, `inline_and_dotted_forms_become_tables`.

---

## File Structure

| File | Responsibility |
|---|---|
| `proto/irsync/config/config.proto` (new) | The message. |
| `crates/irsync-proto/build.rs`, `src/lib.rs`, `Cargo.toml` | Build it; expose it as `irsync_proto::config`. |
| `crates/irsync/src/config/reader.rs` (new) | Text to message: shape pass, typed pass, checks. Owns its serde structs. |
| `crates/irsync/src/config/comments.rs` (new) | Fills the comment fields of a message from the same text. |
| `crates/irsync/src/config/writer.rs` (new) | Message to text. |
| `crates/irsync/src/config/file.rs` | `Config`, `Config::from_message`, `Config::parse`, `Config::load`. Loses the parsing code. |
| `crates/irsync/src/config/hosts.rs`, `exports.rs`, `settings.rs` | Typed types only. Lose `Deserialize`, the `Raw*` types and `*_from_raw`. |
| `crates/irsync/src/manage/msg/{mod,hosts,grants,exports,keys}.rs` (new, temporary) | The management edits on the message. Task 10 moves them over `manage/{hosts,grants,exports,config}.rs`. |
| `crates/irsync/tests/config_message.rs` (new) | Reader, comments, writer, round trips. |
| `crates/irsync/tests/fixtures/config_corpus/*.toml` (new) | Files for the round-trip tests. |
| `crates/irsync/tests/msg_{hosts,grants,exports,keys}.rs` (new, temporary) | Ports of `manage_{hosts,grants,exports,config}.rs`. Task 10 moves them over the old files. |

In this plan `pb` means `irsync_proto::config`.

---

### Task 1: The message

**Files:**
- Create: `proto/irsync/config/config.proto`
- Modify: `crates/irsync-proto/build.rs` (the list `["irsync/v1", "irsync/capture/v1"]`), `crates/irsync-proto/src/lib.rs`, `crates/irsync-proto/Cargo.toml` (move `prost-types` from dev-dependencies to dependencies), `crates/irsync/Cargo.toml` (add `prost-types = { workspace = true }`)
- Test: `crates/irsync-proto/src/lib.rs` (its `tests` module)

**Interfaces:**
- Produces: module `irsync_proto::config` with the prost types of the file below.

- [ ] **Step 1: Write the failing test** in `crates/irsync-proto/src/lib.rs`

```rust
#[test]
fn config_message_has_optional_keys_and_comments() {
    let c = config::Config::default();
    assert_eq!(c.unsafe_config_allowed, None);
    assert!(c.hosts.is_empty() && c.header.is_empty());
    let h = config::Host { name: "nas".into(), ..Default::default() };
    assert_eq!(h.endpoint_id, None);
    assert_eq!(h.comment, None);
    let e = config::Entry { value: "laptop".into(), comment: None };
    assert_eq!(e.value, "laptop");
}
```

- [ ] **Step 2: Run it.** `cargo test -p irsync-proto config_message` — fails: no module `config`.

- [ ] **Step 3: Write `proto/irsync/config/config.proto`**

```proto
// The configuration of irsync, in memory. A reader fills this from
// irsync.toml and a writer builds the file from it. It is never encoded:
// field numbers may change whenever the config changes.
syntax = "proto3";
package irsync.config;

import "google/protobuf/duration.proto";

message Comment {
  repeated string above = 1;   // whole lines, from '#' on
  string trailing = 2;         // at the end of the line, from '#' on; "" for none
}

message Entry {                // one element of a string array
  string value = 1;
  Comment comment = 2;
}

enum RelayMode { RELAY_MODE_UNSPECIFIED = 0; RELAY_MODE_DEFAULT = 1; RELAY_MODE_DISABLED = 2; }
enum Congestion { CONGESTION_UNSPECIFIED = 0; CONGESTION_BBR = 1; CONGESTION_CUBIC = 2; }
enum OtlpProtocol { OTLP_PROTOCOL_UNSPECIFIED = 0; OTLP_PROTOCOL_GRPC = 1; OTLP_PROTOCOL_HTTP = 2; }

// Every table has: comment (its own), key_comments (by key name) and
// tail (comment and blank lines after its last key; "" is a blank line).

message Config {
  repeated string header = 1;  // lines at the top of the file; "" is a blank line
  repeated string footer = 2;  // lines after everything
  optional bool unsafe_config_allowed = 3;
  optional string identity = 4;
  optional string state_dir = 5;
  map<string, Comment> key_comments = 6;
  repeated string tail = 7;    // after the last top-level key
  Client client = 8;
  Daemon daemon = 9;
  Telemetry telemetry = 10;
  repeated Host hosts = 11;
  repeated Export exports = 12;
  GrantTable all = 13;
  GrantTable immediate = 14;   // only rw is used
}

message Client {
  Comment comment = 1; map<string, Comment> key_comments = 2; repeated string tail = 3;
  optional uint32 parallel = 4;
  google.protobuf.Duration reconnect = 5;
  optional uint32 dial_retries = 6;
  optional bool dht = 7;
  optional bool ssh_install = 8;
  optional bool mdns = 9;
  optional string relay = 10;
  optional RelayMode relay_mode = 11;
  optional Congestion congestion = 12;
}

message Daemon {
  Comment comment = 1; map<string, Comment> key_comments = 2; repeated string tail = 3;
  optional string identity = 4;
  google.protobuf.Duration idle_timeout = 5;
  google.protobuf.Duration dormant_session_timeout = 6;
  optional uint32 max_dormant_sessions_per_key = 7;
  repeated Entry listen = 8;
  bool listen_set = 9;         // the key is in the file, even when empty
  optional bool portmap = 10;
  optional string relay = 11;
  optional RelayMode relay_mode = 12;
  optional Congestion congestion = 13;
  Discovery discovery = 14;
  optional string debug_addr = 15;
  optional bool immediate = 16;
  optional bool allow_root = 17;
}

message Discovery {
  Comment comment = 1; map<string, Comment> key_comments = 2; repeated string tail = 3;
  optional bool dht = 4;
  optional bool dns = 5;
  optional bool mdns = 6;
}

message Telemetry {
  Comment comment = 1; map<string, Comment> key_comments = 2; repeated string tail = 3;
  optional string otlp_endpoint = 4;
  optional OtlpProtocol otlp_protocol = 5;
}

message Host {
  Comment comment = 1; map<string, Comment> key_comments = 2; repeated string tail = 3;
  string name = 4;
  optional bytes endpoint_id = 5;
  optional bytes client_id = 6;
  optional string relay = 7;
  optional RelayMode relay_mode = 8;
  optional Congestion congestion = 9;
  repeated Entry address = 10;
  bool address_set = 11;
  optional uint32 parallel = 12;
  google.protobuf.Duration reconnect = 13;
  optional uint32 dial_retries = 14;
  optional bool dht = 15;
  optional bool mdns = 16;
  optional string ssh = 17;
  repeated Entry ssh_aliases = 18;
  bool ssh_aliases_set = 19;
}

message Export {
  Comment comment = 1; map<string, Comment> key_comments = 2; repeated string tail = 3;
  string name = 4;
  string path = 5;             // as written
  optional bool read_only = 6;
  optional bool warm_index = 7;
  optional bool fsync = 8;
  optional bool auto = 9;
  repeated Entry rw = 10;
  bool rw_set = 11;
  repeated Entry ro = 12;
  bool ro_set = 13;
}

message GrantTable {
  Comment comment = 1; map<string, Comment> key_comments = 2; repeated string tail = 3;
  repeated Entry rw = 4;
  bool rw_set = 5;
  repeated Entry ro = 6;
  bool ro_set = 7;
}
```

The `*_set` flags exist because a repeated field cannot say "the key is there with an empty array" (`rw = []`), which today's files hold after a `grant remove`. A message field (`Client client`, a `Duration`) is absent when the table or key is.

- [ ] **Step 4: Build it.** Add `"irsync/config"` to the directory list in `build.rs`; in `lib.rs` add `pub mod config { include!(concat!(env!("OUT_DIR"), "/irsync.config.rs")); }`. In `build.rs`, make prost generate `BTreeMap` for the maps: `.btree_map(["."])`, so tests can compare messages and iteration is stable.

- [ ] **Step 5: Run.** `cargo test -p irsync-proto` — all pass, including `descriptor_set_has_both_packages`.

- [ ] **Step 6: Commit.** `git commit -m "Define the config as a protobuf message"` with a body line: the message every reader fills and the writer writes from; never encoded.

---

### Task 2: The reader, values only

**Files:**
- Create: `crates/irsync/src/config/reader.rs`
- Modify: `crates/irsync/src/config/mod.rs` (`pub mod reader;`)
- Create: `crates/irsync/tests/config_message.rs`

**Interfaces:**
- Produces: `pub fn read_values(text: &str, path: &Path) -> Result<pb::Config, ConfigError>` in `config::reader`. All comment fields empty. (Task 4 adds `read`, which also fills comments.)

The reader is today's `Config::parse` (`config/file.rs:82-175`), `hosts_from_raw` (`config/hosts.rs`) and `exports_from_raw` (`config/exports.rs`), copied into this file with serde structs of its own, and changed in three ways: every key is an `Option` (no `#[serde(default)]` that fills a value in), a path stays the string that was written, and the result is the message. The shape pass, every check, and every error text and line stay exactly as they are. `Config::parse` is not touched in this task.

- [ ] **Step 1: Write the failing tests** in `tests/config_message.rs`

```rust
use irsync::config::reader::read_values;
use irsync_proto::config as pb;
use std::path::Path;

fn id(n: u8) -> iroh::EndpointId { iroh::SecretKey::from_bytes(&[n; 32]).public() }
fn values(text: &str) -> pb::Config { read_values(text, Path::new("irsync.toml")).unwrap() }

#[test]
fn unset_and_set_to_the_default_differ() {
    assert_eq!(values("").daemon, None);
    assert_eq!(values("[daemon]\n").daemon.unwrap().portmap, None);
    assert_eq!(values("[daemon]\nportmap = true\n").daemon.unwrap().portmap, Some(true));
}

#[test]
fn values_not_spellings() {
    let c = values(&format!(
        "identity = \"~/k\"\n[client]\nreconnect = \"90\"\n[hosts.nas]\nendpoint_id = \"{}\"\nrelay_mode = \"disabled\"\nreconnect = 3600\n",
        id(1)
    ));
    assert_eq!(c.identity.as_deref(), Some("~/k")); // as written
    assert_eq!(c.client.as_ref().unwrap().reconnect.as_ref().unwrap().seconds, 90);
    let h = &c.hosts[0];
    assert_eq!(h.endpoint_id.as_deref(), Some(id(1).as_bytes().as_slice()));
    assert_eq!(h.relay_mode, Some(pb::RelayMode::Disabled as i32));
    assert_eq!(h.reconnect.as_ref().unwrap().seconds, 3600);
}

#[test]
fn entries_keep_file_order() {
    let c = values(&format!(
        "[hosts.zed]\nendpoint_id = \"{0}\"\n[exports.b]\npath = \"/b\"\n[hosts.abe]\nendpoint_id = \"{0}\"\n[exports.a]\npath = \"/a\"\n",
        id(1)
    ));
    assert_eq!(c.hosts.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["zed", "abe"]);
    assert_eq!(c.exports.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["b", "a"]);
}

#[test]
fn an_empty_array_is_set() {
    let c = values("[all]\nrw = []\n");
    let all = c.all.unwrap();
    assert!(all.rw.is_empty() && all.rw_set && !all.ro_set);
}

#[test]
fn errors_are_those_of_config_parse() {
    for text in [
        "[hosts.nas]\nendpoint_id = \"nope\"\n",
        "[exports.p]\npath = \"rel\"\n",
        "[nas]\nendpoint_id = \"x\"\n",
        "rw = [\"x\"]\n",
        "[daemon]\nlisten = [\"1.2.3.4:1\", \"1.2.3.5:1\"]\n",
        "[immediate]\nro = [\"x\"]\n",
        "[hosts.all]\nendpoint_id = \"x\"\n",
        "[client]\nbogus = 1\n",
    ] {
        let old = irsync::config::file::Config::parse(text, Path::new("f")).unwrap_err();
        let new = read_values(text, Path::new("f")).unwrap_err();
        assert_eq!(new.to_string(), old.to_string(), "{text}");
    }
}
```

- [ ] **Step 2: Run.** `cargo test -p irsync --test config_message` — fails: no `reader`.

- [ ] **Step 3: Implement `read_values`.** Entry order: sort hosts and exports by the start of their span. `listen`, `address`, `ssh_aliases`, `rw`, `ro`: an `Entry` per element with the text as written for grants and ssh aliases, and `HostPort`'s `to_string()` for addresses; set the `*_set` flag when the key is in the file. `debug_addr` is kept as written, after `parse_debug_addr` accepted it. The tilde check for an export path (`must be absolute`) is made on the expanded path, and the unexpanded text is stored.

- [ ] **Step 4: Run.** `cargo test -p irsync --test config_message` — pass.

- [ ] **Step 5: Commit.** `git commit -m "Read irsync.toml into the config message"`

---

### Task 3: The typed view

**Files:**
- Modify: `crates/irsync/src/config/file.rs`, `hosts.rs`, `exports.rs`, `settings.rs`
- Test: `crates/irsync/tests/config_message.rs`; `crates/irsync/tests/config_files.rs` unchanged

**Interfaces:**
- Consumes: `reader::read_values`.
- Produces: `impl Config { pub fn from_message(msg: &pb::Config, path: &Path) -> Result<Config, ConfigError> }`. `Config::parse(text, path)` becomes `from_message(&read_values(text, path)?, path)`. `Settings::parse` stays.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn from_message_applies_defaults_and_expands_tilde() {
    let c = Config::from_message(&values("identity = \"~/k\"\n[daemon]\n"), Path::new("f")).unwrap();
    assert_eq!(c.settings.identity, dirs::home_dir().unwrap().join("k"));
    assert_eq!(c.settings.ssh_identity, None); // the file names an identity
    assert_eq!(c.settings.daemon, DaemonSettings::default());
    assert_eq!(c.hosts.path, Path::new("f"));
}

#[test]
fn from_message_refuses_what_a_command_can_break() {
    let mut m = values("[exports.p]\npath = \"/p\"\n");
    m.exports[0].path = "rel".into();
    let e = Config::from_message(&m, Path::new("f")).unwrap_err();
    assert_eq!(e.to_string(), "f: [exports.p]: path \"rel\" must be absolute");
    let mut m = values("");
    m.hosts.push(pb::Host { name: "nas".into(), endpoint_id: Some(vec![1, 2, 3]), ..Default::default() });
    assert!(Config::from_message(&m, Path::new("f")).is_err());
}
```

- [ ] **Step 2: Run** — fails: no `from_message`.

- [ ] **Step 3: Implement `from_message`** and switch `Config::parse` to it. First, while the old parse still exists, run Task 2's `errors_are_those_of_config_parse` once with the messages printed and replace its comparison by those literal messages. It re-checks what an edit can break, with `line: None`: host alias rules (`valid_alias`), an id of 32 bytes that is a valid key, a host with neither id, a reserved or invalid export name, a relative export path, two listen addresses of one family, URLs, `debug_addr`, duplicate host or export names. It uses the same message texts as the reader. Then delete from `file.rs`, `hosts.rs`, `exports.rs` and `settings.rs` what only the old parse used: `RawMerged`, `RawHost`, `RawHosts`, `hosts_from_raw`, `RawExports`, `AllGrants`, `ImmediateGrants`, `exports_from_raw`, the `de_*` functions, and `Deserialize` on the typed types. `immediate_problem` moves into the reader.

- [ ] **Step 4: Run.** `cargo test -q -p irsync --all-features --no-fail-fast` — all pass. `tests/config_files.rs`, `authz_snapshot.rs` and every e2e test pass with no edit: that is the proof that nothing changed.
- [ ] **Step 5: Commit.** `git commit -m "Build the typed config from the message"`

---

### Task 4: Comments

**Files:**
- Create: `crates/irsync/src/config/comments.rs`
- Modify: `crates/irsync/src/config/reader.rs`, `config/mod.rs`
- Test: `crates/irsync/tests/config_message.rs`

**Interfaces:**
- Consumes: `read_values`.
- Produces: `pub fn read(text: &str, path: &Path) -> Result<pb::Config, ConfigError>` in `config::reader`: `read_values`, then `comments::attach(text, &mut msg)`. `pub(crate) fn attach(text: &str, msg: &mut pb::Config)`; it cannot fail, since `read_values` accepted the text.

Where a comment goes (`toml_edit` exposes each as the decoration of a table, key, value or array element):

| In the file | In the message |
|---|---|
| lines directly above a key | that key's `key_comments[key].above`; blank lines among them are dropped |
| the rest of a line after a key's value | `key_comments[key].trailing` |
| the text before a table header, after its last blank line | the table's `comment.above` |
| the text before a table header, up to its last blank line | the `tail` of the table before it (of `Config` before the first table); blank lines inside are kept as `""`, at its ends dropped |
| the same, when nothing but comments came before in the file | `Config.header` |
| the rest of the header line | the table's `comment.trailing` |
| lines above an array element | that `Entry.comment.above` |
| the rest of the line after an element (after its comma, if any) | that `Entry.comment.trailing` |
| comment lines after the last element of an array | the last element's `comment.above`, appended; for an empty array the key's `above` |
| lines after everything | `Config.footer` |
| comments between a table header and its first key | that key's `above` |

For `[hosts]` and `[exports]` headers written on their own (`[exports]` followed by `[exports.p]`): their comments go to the `above` of the first entry.

- [ ] **Step 1: Write the failing tests.** One file text exercising every row, with exact assertions:

```rust
const COMMENTED: &str = "\
# irsync on the nas
# second header line

identity = \"~/k\" # my key
# after the top keys

# about the daemon
[daemon] # trailing on header
# above portmap
portmap = false # why off
# daemon tail

# the photos
[exports.photos]
path = \"/photos\"
rw = [
    # the laptop
    \"laptop\", # mine
    \"phone\",
    # after the last
]
# the end
";

#[test]
fn every_comment_has_a_home() {
    let c = read(COMMENTED, Path::new("f")).unwrap();
    assert_eq!(c.header, ["# irsync on the nas", "# second header line"]);
    assert_eq!(c.key_comments["identity"].trailing, "# my key");
    assert_eq!(c.tail, ["# after the top keys"]);
    let d = c.daemon.unwrap();
    assert_eq!(d.comment.as_ref().unwrap().above, ["# about the daemon"]);
    assert_eq!(d.comment.unwrap().trailing, "# trailing on header");
    assert_eq!(d.key_comments["portmap"].above, ["# above portmap"]);
    assert_eq!(d.key_comments["portmap"].trailing, "# why off");
    assert_eq!(d.tail, ["# daemon tail"]);
    let e = &c.exports[0];
    assert_eq!(e.comment.as_ref().unwrap().above, ["# the photos"]);
    assert_eq!(e.rw[0].comment.as_ref().unwrap().above, ["# the laptop"]);
    assert_eq!(e.rw[0].comment.as_ref().unwrap().trailing, "# mine");
    assert_eq!(e.rw[1].comment.as_ref().unwrap().above, ["# after the last"]);
    assert_eq!(c.footer, ["# the end"]);
}

#[test]
fn hash_in_a_string_is_not_a_comment() {
    let c = read(&format!("[hosts.nas]\nendpoint_id = \"{}\"\nssh = \"me@host#1\"\n", id(1)), Path::new("f")).unwrap();
    assert_eq!(c.hosts[0].ssh.as_deref(), Some("me@host#1"));
    assert!(c.hosts[0].key_comments.is_empty());
}

#[test]
fn a_file_without_comments_has_none() {
    let text = "[daemon]\nportmap = false\n";
    assert_eq!(read(text, Path::new("f")).unwrap(), read_values(text, Path::new("f")).unwrap());
}
```

- [ ] **Step 2: Run** — fails: no `read`.
- [ ] **Step 3: Implement `comments::attach` and `reader::read`.** A comment whose table or key the walk does not find in the message is appended to `Config.footer`, never dropped.
- [ ] **Step 4: Run** — pass.
- [ ] **Step 5: Commit.** `git commit -m "Carry the file's comments in the config message"`

---

### Task 5: The writer

**Files:**
- Create: `crates/irsync/src/config/writer.rs`, `crates/irsync/tests/fixtures/config_corpus/{full,commented,inline,dotted,odd_comments,quoted_names,grants}.toml`
- Modify: `crates/irsync/src/config/mod.rs`
- Test: `crates/irsync/tests/config_message.rs`

**Interfaces:**
- Consumes: `reader::read`.
- Produces: `pub fn write(msg: &pb::Config) -> String` in `config::writer`.

The writer's form:

- Order: `header`, a blank line, the top-level keys (`unsafe_config_allowed`, `identity`, `state_dir`), `Config.tail`, then `[client]`, `[daemon]`, `[daemon.discovery]`, `[telemetry]`, each `[hosts.NAME]`, each `[exports.NAME]`, `[all]`, `[immediate]`, then `footer`.
- Keys inside a table in the order of the message's fields.
- One blank line before each table header's `above` lines (none at the start of the file). No blank lines inside a table, except those in a `tail`.
- `key = value # trailing`: one space before a trailing comment.
- An array on one line (`rw = ["a", "b"]`), unless an entry has a comment: then one entry per line, four spaces in, a comma after each.
- A table that is present but has no key and no comment is not written. `[all]` and `[immediate]` with only empty arrays are written, as today.
- Durations with `duration::format`, as a string. Ids as `EndpointId` prints them. Enums in their file words (`"disabled"`, `"cubic"`, `"grpc"`).
- Strings and names are quoted by `toml_edit` (`toml_edit::value(s)`, `toml_edit::Key::new(name).display_repr()`), never by hand.
- The file ends with one newline. An empty message is the empty string.

- [ ] **Step 1: Write the failing tests**

```rust
fn corpus() -> Vec<(String, String)> { /* every *.toml under tests/fixtures/config_corpus, (name, text) */ }

#[test]
fn the_writers_form() {
    // COMMENTED from Task 4, as the writer lays it out.
    let out = write(&read(COMMENTED, Path::new("f")).unwrap());
    assert_eq!(out, "\
# irsync on the nas
# second header line

identity = \"~/k\" # my key
# after the top keys

# about the daemon
[daemon] # trailing on header
# above portmap
portmap = false # why off
# daemon tail

# the photos
[exports.photos]
path = \"/photos\"
rw = [
    # the laptop
    \"laptop\", # mine
    # after the last
    \"phone\",
]

# the end
");
}

#[test]
fn read_write_read_is_the_same_message() {
    for (name, text) in corpus() {
        let m = read(&text, Path::new("f")).unwrap();
        assert_eq!(read(&write(&m), Path::new("f")).unwrap(), m, "{name}");
    }
}

#[test]
fn the_form_is_stable() {
    for (name, text) in corpus() {
        let once = write(&read(&text, Path::new("f")).unwrap());
        assert_eq!(write(&read(&once, Path::new("f")).unwrap()), once, "{name}");
    }
}

#[test]
fn no_comment_is_lost() {
    for (name, text) in corpus() {
        let out = write(&read(&text, Path::new("f")).unwrap());
        for line in text.lines().filter_map(|l| l.find('#').map(|i| l[i..].trim_end())) {
            assert!(out.contains(line), "{name}: lost {line:?}");
        }
    }
}

#[test]
fn the_typed_config_is_the_same_after_a_rewrite() {
    for (name, text) in corpus() {
        let out = write(&read(&text, Path::new("f")).unwrap());
        assert_eq!(Config::parse(&out, Path::new("f")).unwrap(), Config::parse(&text, Path::new("f")).unwrap(), "{name}");
    }
}

#[test] fn odd_comment_places_survive() { /* odd_comments.toml through no_comment_is_lost's check, alone, so a failure names it */ }
#[test] fn quoted_names_round_trip() { /* [exports."www.example.com"], [hosts."my nas"], [hosts."käse"]: write() holds each header quoted exactly so, and reads back equal */ }
#[test] fn file_edges_round_trip() { /* "", "# only\n", "[daemon]\nportmap = false" (no final newline), "[daemon]\r\nportmap = false\r\n": each reads; write() reads back to the same message; the CRLF text and its LF twin give equal messages */ }
#[test] fn inline_and_dotted_forms_become_tables() { /* inline.toml and dotted.toml give the same message as their plain twins written in the test, comments included, and write() holds "[hosts.nas]" and "[daemon]" */ }
```

The corpus: `full.toml` sets every key of every table at least once (the list is `kind_of` in `manage/config.rs`) with a non-default value; `commented.toml` is `COMMENTED`; `inline.toml` writes hosts and exports as inline tables; `dotted.toml` uses `daemon.portmap = false` and `hosts.nas.endpoint_id = "…"`; `odd_comments.toml` has a comment after the last element of a multi-line array, between a header and its first key, in a blank-line-separated block between two tables, and after a bare `[exports]` header; `quoted_names.toml` as above; `grants.toml` has `[all]`, `[immediate]`, empty arrays, ids and aliases mixed, a comment on a grant entry. The `#` check in `no_comment_is_lost` means no corpus file may hold a `#` inside a string.

- [ ] **Step 2: Run** — fails: no `writer`.
- [ ] **Step 3: Implement `write`.**
- [ ] **Step 4: Run.** `cargo test -p irsync --test config_message` — pass. If `the_form_is_stable` fails for a comment position, fix the placement rule in `comments.rs` so that what the writer writes is read back into the same field; do not special-case the writer.
- [ ] **Step 5: Commit.** `git commit -m "Write irsync.toml from the config message"`

---

### Task 6: Host edits on the message

**Files:**
- Create: `crates/irsync/src/manage/msg/mod.rs`, `manage/msg/hosts.rs`, `crates/irsync/tests/msg_hosts.rs`
- Modify: `crates/irsync/src/manage/mod.rs` (`pub mod msg;`)

**Interfaces:**
- Produces, in `manage::msg::hosts`, with what each does unchanged from `manage/hosts.rs`:
  - `pub struct HostFlags` — as today, except `reconnect: Option<Duration>`.
  - `pub fn host_add(msg: &mut pb::Config, alias: &str, id: EndpointId, flags: &HostFlags) -> Result<Option<EndpointId>, ManageError>`
  - `pub fn host_add_ssh_alias(msg: &mut pb::Config, alias: &str, dest: &str)`
  - `pub fn host_set_client(msg: &mut pb::Config, alias: &str, id: EndpointId) -> Result<(), ManageError>`
  - `pub fn host_remove(msg: &mut pb::Config, alias: &str, force: bool) -> Result<usize, ManageError>`
  - `pub fn host_rename(msg: &mut pb::Config, old: &str, new: &str) -> Result<usize, ManageError>`
  - `pub fn host_aliases(msg: &pb::Config) -> Vec<String>`
  - `pub fn host_list(msg: &pb::Config) -> Vec<String>` — the same lines as today; the `key = value` lines in the writer's key order and spelling.
- Produces, in `manage::msg` (used by Tasks 7 to 10): `pub fn id_bytes(id: EndpointId) -> Vec<u8>`, `pub fn id_of(bytes: &[u8]) -> Option<EndpointId>`.
- Consumes from Task 7: `lists_naming`, `remove_named`, `rename_named`. Until Task 7 exists, write them in `msg/hosts.rs` as private functions over `all`, `immediate` and every export's `rw` and `ro`; Task 7 moves them to `msg/grants.rs` and makes them `pub`.

- [ ] **Step 1: Port the tests.** Copy `tests/manage_hosts.rs` to `tests/msg_hosts.rs`. Every case stays. A document built with `.parse::<DocumentMut>()` becomes `reader::read(text, Path::new("f")).unwrap()`; an assertion on `doc[..]` becomes one on the message; an assertion on `doc.to_string()` becomes one on `writer::write(&msg)`, with the expected text in the writer's form. Add:

```rust
#[test]
fn rename_and_remove_take_the_comments_along() {
    let mut m = read(&format!("# the nas\n[hosts.nas]\nendpoint_id = \"{}\" # its daemon\n", id(1)), Path::new("f")).unwrap();
    host_rename(&mut m, "nas", "box").unwrap();
    assert_eq!(write(&m), format!("# the nas\n[hosts.box]\nendpoint_id = \"{}\" # its daemon\n", id(1)));
    host_remove(&mut m, "box", false).unwrap();
    assert_eq!(write(&m), "");
}
```

- [ ] **Step 2: Run.** `cargo test -p irsync --test msg_hosts` — fails to compile.
- [ ] **Step 3: Implement** the functions above.
- [ ] **Step 4: Run** — pass.
- [ ] **Step 5: Commit.** `git commit -m "Edit hosts in the config message"`

---

### Task 7: Grant edits on the message

**Files:**
- Create: `crates/irsync/src/manage/msg/grants.rs`, `crates/irsync/tests/msg_grants.rs`
- Modify: `manage/msg/hosts.rs` (use the moved functions), `manage/msg/mod.rs`

**Interfaces:**
- Produces, in `manage::msg::grants`, behaviour unchanged from `manage/grants.rs`:
  - `pub fn canonicalize(msg: &mut pb::Config, hosts: &Hosts) -> bool`
  - `pub fn lists_naming(msg: &pb::Config, name: &str) -> Vec<String>`
  - `pub fn remove_named(msg: &mut pb::Config, name: &str) -> usize`
  - `pub fn rename_named(msg: &mut pb::Config, old: &str, new: &str) -> usize`
  - `pub fn grant_add(msg: &mut pb::Config, export: Option<&str>, who: &PeerSpec, ro: bool, resolve: &dyn Fn(&str) -> Option<EndpointId>) -> Result<(), ManageError>`
  - `pub fn grant_immediate(msg: &mut pb::Config, who: &PeerSpec, resolve: &dyn Fn(&str) -> Option<EndpointId>) -> Result<bool, ManageError>`
  - `pub fn grant_remove_immediate(msg: &mut pb::Config, who: Option<&PeerSpec>, resolve: …) -> Result<usize, ManageError>`
  - `pub fn grant_remove(msg: &mut pb::Config, export: Option<&str>, who: Option<&PeerSpec>, resolve: …) -> Result<usize, ManageError>`
  - `list_all`, `list_export`, `list_peer`: not ported; they read the typed `Exports` and `Hosts` and stay where they are.

- [ ] **Step 1: Port the tests.** `tests/manage_grants.rs` to `tests/msg_grants.rs`, as in Task 6. The cases that pin where a comment lands after an add or a remove (`push_entry`, `remove_entry`) become: the comment of the entry that stays is still on that entry, and the removed entry's comment is gone. A public key's comment `c` is stored as `Entry.comment.trailing = "# c"`. The `INJECTED` cases stay: a comment or a name with a newline or a `]` must not produce a second table in `write()`. Add:

```rust
#[test]
fn canonicalize_keeps_the_comment_of_the_entry_that_stays() {
    // rw holds the id (with a comment) and its alias: the first stays, renamed to the alias, with its comment.
    let mut m = read(&format!("[hosts.laptop]\nclient_id = \"{0}\"\n[all]\nrw = [\n    \"{0}\", # mine\n    \"laptop\",\n]\n", id(1)), Path::new("f")).unwrap();
    let hosts = Config::from_message(&m, Path::new("f")).unwrap().hosts;
    assert!(canonicalize(&mut m, &hosts));
    let rw = &m.all.as_ref().unwrap().rw;
    assert_eq!(rw.len(), 1);
    assert_eq!((rw[0].value.as_str(), rw[0].comment.as_ref().unwrap().trailing.as_str()), ("laptop", "# mine"));
}
```

- [ ] **Step 2: Run** — fails to compile.
- [ ] **Step 3: Implement.** Adding to a list sets its `*_set` flag; removing the last entry leaves the flag set (the file keeps `rw = []`, as today).
- [ ] **Step 4: Run.** `cargo test -p irsync --test msg_grants --test msg_hosts` — pass.
- [ ] **Step 5: Commit.** `git commit -m "Edit grants in the config message"`

---

### Task 8: Export edits on the message

**Files:**
- Create: `crates/irsync/src/manage/msg/exports.rs`, `crates/irsync/tests/msg_exports.rs`

**Interfaces:**
- Produces, in `manage::msg::exports`, behaviour unchanged from `manage/exports.rs`:
  - `pub fn add_export(msg: &mut pb::Config, name: &str, path: &Path, read_only: bool, warm_index: bool) -> Result<(), ManageError>`
  - `pub fn add_auto_export(msg: &mut pb::Config, path: &Path, peer: EndpointId) -> Result<String, ManageError>`
  - `pub fn remove_export(msg: &mut pb::Config, name: &str) -> Result<(), ManageError>`
  - `unique_auto_name`, `parse_add_target`: not ported; they touch no document.

- [ ] **Step 1: Port the tests** (`tests/manage_exports.rs` to `tests/msg_exports.rs`), as in Task 6.
- [ ] **Step 2: Run** — fails to compile.
- [ ] **Step 3: Implement.** `add_export` writes only the flags that are true, as today (`read_only: Some(true)` or `None`).
- [ ] **Step 4: Run** — pass.
- [ ] **Step 5: Commit.** `git commit -m "Edit exports in the config message"`

---

### Task 9: `config get/set/unset/list` on the message

**Files:**
- Create: `crates/irsync/src/manage/msg/keys.rs`, `crates/irsync/tests/msg_keys.rs`

**Interfaces:**
- Consumes: `Kind`, `kind_of`, `parse_key`, `default_of`, `Got` from `manage::config` (they touch no document and stay).
- Produces, in `manage::msg::keys`:
  - `pub enum Value { Bool(bool), Int(i64), Text(String), List(Vec<String>) }`
  - `pub fn parse_value(kind: Kind, values: &[String]) -> Result<Value, ManageError>` — the checks and messages of today's `parse_value`.
  - `pub fn get(msg: &pb::Config, path: &[String]) -> Result<Got, ManageError>`
  - `pub fn list(msg: &pb::Config) -> Vec<String>`
  - `pub fn set(msg: &mut pb::Config, path: &[String], value: Value) -> Result<(), ManageError>`
  - `pub fn unset(msg: &mut pb::Config, path: &[String]) -> Result<(), ManageError>`

- [ ] **Step 1: Port the tests** (`tests/manage_config.rs` to `tests/msg_keys.rs`). Expected output of `get` and `list` follows the writer's spelling (a duration set as `"90"` prints `90s`) and, for `list`, the writer's order. Add:

```rust
#[test]
fn every_known_key_can_be_set_read_and_unset() {
    // One (path, values) pair per arm of kind_of that is not Kind::Table; hosts.nas.* and exports.p.* on a message that has both.
    for (key, values) in ALL_KEYS {
        let mut m = base();
        let path = parse_key(key).unwrap();
        set(&mut m, &path, parse_value(kind_of(&path).unwrap(), &values).unwrap()).unwrap();
        assert!(matches!(get(&m, &path).unwrap(), Got::Lines(_)), "{key}");
        Config::from_message(&m, Path::new("f")).unwrap();
        unset(&mut m, &path).unwrap();
        assert_eq!(get(&m, &path).unwrap(), Got::Unset, "{key}");
    }
}

#[test]
fn unset_takes_the_keys_comment_and_set_keeps_it() {
    let mut m = read("[daemon]\n# why\nportmap = false\n", Path::new("f")).unwrap();
    let p = parse_key("daemon.portmap").unwrap();
    set(&mut m, &p, Value::Bool(true)).unwrap();
    assert_eq!(write(&m), "[daemon]\n# why\nportmap = true\n");
    unset(&mut m, &p).unwrap();
    assert_eq!(write(&m), "");
}
```

- [ ] **Step 2: Run** — fails to compile.
- [ ] **Step 3: Implement.** How a path reaches its field (one hand-written match shared by get, set and unset, or `prost-reflect`) is yours to choose; `every_known_key_can_be_set_read_and_unset` is what keeps it complete. Unsetting a table path (`hosts.nas`, `client`) removes the table with its comments. Unsetting `hosts.nas.endpoint_id` on a host with no `client_id` is caught by the save's `from_message`, as the old "would not load" check caught it.
- [ ] **Step 4: Run** — pass.
- [ ] **Step 5: Commit.** `git commit -m "Read and set config keys in the message"`

---

### Task 10: Switch over

**Files:**
- Modify: `crates/irsync/src/manage/mod.rs`, `crates/irsync/src/main.rs`, `crates/irsync/src/ssh/setup.rs`, `crates/irsync/src/ssh/onboard.rs`, `crates/irsync/src/authz.rs`, `crates/irsync/src/manage/show.rs`
- Move: `manage/msg/hosts.rs` over `manage/hosts.rs`, `msg/grants.rs` into `manage/grants.rs` (keep `list_all`, `list_export`, `list_peer`), `msg/exports.rs` into `manage/exports.rs` (keep `unique_auto_name`, `parse_add_target`), `msg/keys.rs` into `manage/config.rs` (keep `Kind`, `kind_of`, `parse_key`, `default_of`, `Got`); delete `manage/msg/`
- Move: `tests/msg_hosts.rs` over `tests/manage_hosts.rs`, likewise `grants`, `exports`, `keys` over `manage_config.rs`
- Modify: `tests/manage_doc.rs`, `tests/ssh_setup.rs`, and any test under `crates/irsync/tests` that compares the text of a saved config

**Interfaces:**
- Produces, in `manage`:
  - `pub fn load_message(path: &Path) -> Result<pb::Config, ManageError>` — replaces `load_doc`. A missing file is the empty message. A file that does not load is `ManageError::Config(e)`, with the reader's error and its line.
  - `ConfigLock::load(&self) -> Result<pb::Config, ManageError>`, `ConfigLock::save(self, msg: &mut pb::Config) -> Result<(), ManageError>`, `save_config(path, msg)`. `save`: `canonicalize_doc`, then `Config::from_message` (an error is `ManageError::Invalid` and the file is left as it was), then `writer::write`, written and renamed as today, mode kept.
  - `canonicalize_doc(msg: &mut pb::Config, path: &Path) -> Result<bool, ManageError>`, `canonicalize_without(msg, path, alias)`.
  - `load_doc`, `save_doc` and `ManageError::Parse` are deleted.

- [ ] **Step 1: Write the failing tests** in `tests/manage_doc.rs` (port its cases the same way), adding:

```rust
#[test]
fn a_save_keeps_comments_and_writes_the_writers_form() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("irsync.toml");
    std::fs::write(&path, "[all]\nrw=[]   # nobody yet\n\n\n# the daemon\n[daemon]\nportmap=false\n").unwrap();
    let lock = lock_config(&path).unwrap();
    let mut m = lock.load().unwrap();
    lock.save(&mut m).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "# the daemon\n[daemon]\nportmap = false\n\n[all]\nrw = [] # nobody yet\n");
}

#[test]
fn a_save_that_would_not_load_leaves_the_file_and_frees_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("irsync.toml");
    std::fs::write(&path, "[exports.p]\npath = \"/p\"\n").unwrap();
    let lock = lock_config(&path).unwrap();
    let mut m = lock.load().unwrap();
    m.exports[0].path = "rel".into();
    assert!(matches!(lock.save(&mut m), Err(ManageError::Invalid { .. })));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[exports.p]\npath = \"/p\"\n");
    lock_config(&path).unwrap();
}

#[test]
fn a_file_that_does_not_load_cannot_be_edited() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("irsync.toml");
    std::fs::write(&path, "[hosts.nas]\nendpoint_id = \"nope\"\n").unwrap();
    let e = lock_config(&path).unwrap().load().unwrap_err();
    assert!(e.to_string().contains("irsync.toml:2:"), "{e}");
}
```

- [ ] **Step 2: Run** — fails to compile.
- [ ] **Step 3: Switch `manage/mod.rs`** to the interfaces above and move the `msg` files and tests into place.
- [ ] **Step 4: Switch the callers.** `main.rs` (every `lock.load()`, `load_doc`, `cfg::`, `manage::hosts::`, `manage::grants::`, `manage::exports::` site; `host add --reconnect` passes the parsed `Duration`), `ssh/setup.rs` (line 488: `Value::Bool(false)` for `toml_edit::Value::from(false)`), `ssh/onboard.rs` (its helpers that read `[hosts.alias]` out of the document, lines 282-310, read the message), `authz.rs` (`add_auto_export`). `main.rs:2101` and `:2365` call `host_list` on `load_message`.
- [ ] **Step 5: Run** `cargo test -q --workspace --all-features --no-fail-fast`. Tests that compare the text of a saved config fail on layout only: change the expected text to the writer's form, and nothing else. A test that fails for another reason is a bug in Tasks 6 to 10.
- [ ] **Step 6: Check the boundary.** `rg -n "toml_edit|toml::" crates/irsync/src` names only `config/reader.rs`, `config/comments.rs`, `config/writer.rs`, and `config/mod.rs` (`unsafe_allowed`, `toml_err`, `ConfigError::Toml`). Move `toml_err` into the reader if nothing else uses it.
- [ ] **Step 7: Run** `cargo clippy --workspace --all-targets --all-features -- -D warnings` and `cargo fmt --check`.
- [ ] **Step 8: Commit.** `git commit -m "Manage the config through the message"` with a body: the commands, the SSH setup and the automatic export now edit the message; a save writes the file in one form and keeps comments.

---

### Task 11: The book, the full check, the pull request

**Files:**
- Modify: `book/src/reference/management.md` (line 25: "The commands keep the file's comments and layout"; line 44: "The alias keeps its place, keys and comments"; lines 87-88 on a key's comment), `book/src/reference/configuration.md` (line 6)

- [ ] **Step 1: Rewrite those sentences.** What is true now: a command keeps the file's comments and the order of its hosts and exports; it writes the tables in a fixed order and the values in one spelling (a duration `"90"` becomes `"90s"`); blank lines and inline tables are not kept; a file that does not load must be fixed by hand, the error names the line. Check the hints in the code that tell a user to repair a file that does not load with a command (`rg -n "config (set|unset)" crates/irsync/src`) and reword any that apply to a file that does not load.
- [ ] **Step 2: Man pages.** `python3 -m unittest discover -s tools/tests` — pass (management.md and configuration.md feed them).
- [ ] **Step 3: Full check.** `tools/ci.sh check` — pass. If `Cargo.lock` changed: `tools/sync-fuzz-lock.sh`, commit both.
- [ ] **Step 4: Commit and push.** `git commit -m "Say what a config edit keeps and what it rewrites"`, `git push`.
- [ ] **Step 5: Open the pull request** against `main`, with `Closes #N` for the stage 1 issue. Its description says, for the release notes: the first command that edits `irsync.toml` rewrites it in a standard form; comments and the order of hosts and exports are kept; blank lines, inline tables and the spelling of durations are not; a file that does not load can no longer be repaired with `irsync config unset`.
