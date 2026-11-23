# Config as a message, and the split of `[hosts]` into servers and clients

Issue: #202. Date: 2026-11-22.

## Intent

Today one `[hosts.ALIAS]` table does two jobs. It names a server to dial
(`endpoint_id` and the dial keys) and a client that grants are for
(`client_id`). A grant that names an alias uses `client_id` and falls back
to `endpoint_id`, so a daemon's id can end up granted as if it were a
client.

#202 splits the table in two: servers this machine connects to, and
clients it lets in. That changes the layout of the config file, and it will
not be the last change (#203 adds an admin right). So the work starts one
step earlier: the config gets one definition that does not depend on the
file layout, and a way to keep reading old layouts.

Mike's decisions, as he made them:

- The config is defined as a protobuf message, for consistency with how the
  rest of irsync defines its data. It is an in-memory form. It is not for
  sending over the wire and not shaped for #203.
- **The message is the only place the config lives.** A reader fills it
  from the file. Everything else works from it. A writer builds the file
  from it.
- When the layout changes, the message definition is changed, the reader
  of the old layout is snapshotted as `legacy_vX`, and it fills the new
  message.
- Comments in the file must survive, across edits and across versions. So
  the message carries the comments too.
- An old `[hosts]` file is converted silently on the first edit. No
  warning period, no refusal to start.
- A host entry that has only an `endpoint_id` and is named in a grant
  keeps its access through the conversion.

## Goals

- No config key and no command mixes a dialled identity with a granted one.
- The "`client_id`, else `endpoint_id`" rule exists only inside the
  reader of the old layout.
- An old file works unchanged after an upgrade, before and after it is
  rewritten, and grants the same access both times.
- No comment is lost by an edit or by a conversion.
- A later layout change needs: a change to the message, a snapshot of the
  reader, and nothing else outside `config/`.

## Non-goals

- The admin right and anything else of #203.
- Changes to the protocol, to stored state, or to how exports and grants
  work.
- Keeping a hand-made layout. Every save writes the file in the writer's
  form: its order of tables and keys, its spacing, its spelling of values.
  Comments and the order of entries are what survive.
- Encoding the message. It is never stored or sent, so its field numbers
  may change freely.

## Anti-goals

- A file that grants something different after the rewrite than before.
- A daemon that does not start, or drops a client, because of the upgrade.
- A second place that knows the file layout (a command that edits TOML
  text, a converter beside a reader).

## Approaches

Three ways to keep old files readable and comments alive were on the
table.

1. **Text-level steps.** Each old version is one function that rewrites the
   parsed TOML document to the next layout; the TOML library carries the
   comments. Exact comment placement and old versions never need touching
   again. But the TOML document stays the real config, and the message
   only a view of it.
2. **Frozen readers, comments copied by origin.** Readers fill the message
   and note where each entry came from; the writer copies comments from
   the old file. Comment placement is a guess at the edges.
3. **Comments live in the message** (picked). Readers fill values and
   comments, one writer builds the file. Old readers are plain snapshots.
   Cost: the management commands are rewritten to edit the message, and
   hand-made layout is not kept.

Mike picked 3: the message is the only place the config lives.

## Two stages, two pull requests

**Stage 1: the message, reader and writer, with `[hosts]` as it is.**
No change in what irsync does, except that a save now writes the file in
the writer's form. It gets an issue of its own, which its PR closes.

**Stage 2: the split.** The message changes, the stage 1 reader becomes
`legacy_v1`, commands and book follow. Its PR closes #202.

Stage 2 is the first use of the snapshot step, so it also proves stage 1.

## Stage 1

### The message

One file, `proto/irsync/config/config.proto`, package `irsync.config`,
built by `crates/irsync-proto` like the others (add the directory to
`build.rs`). No version in the package name: there is one definition, the
current one. A header comment says the message is never encoded.

Shape (names are the builder's to polish):

```proto
message Config {
  repeated string header = 1;        // comment lines at the top of the file
  repeated string footer = 2;        // comment lines after the last entry
  optional bool unsafe_config_allowed = 3;
  optional string identity = 4;      // as written, "~" not expanded
  optional string state_dir = 5;
  map<string, Comment> key_comments = 6;   // for the top-level keys
  Client client = 7;
  Daemon daemon = 8;                 // holds Discovery
  Telemetry telemetry = 9;
  repeated Host hosts = 10;          // file order
  repeated Export exports = 11;      // file order
  GrantTable all = 12;
  GrantTable immediate = 13;         // rw only
}

message Comment {
  repeated string above = 1;         // whole lines above, from '#' on
  string trailing = 2;               // at the end of the line
}

// Every table message (Client, Daemon, Discovery, Telemetry, Host, Export,
// GrantTable) has these three besides its keys:
//   Comment comment;                     the table's own
//   map<string, Comment> key_comments;   by key name
//   repeated string tail;                comment lines after its last key;
//                                        "" is a blank line between them

message Entry {                      // one element of a string array
  string value = 1;
  Comment comment = 2;
}
```

Rules:

- Every key that may be absent from the file is `optional`, so "not set"
  and "set to the default" stay different. The writer writes only what is
  set. Defaults are applied where they are today, not in the message.
- Values, not spellings: a duration is a `google.protobuf.Duration`, an id
  is 32 bytes, an enum is an enum. The writer spells them one way
  (`duration::format`, the id as `EndpointId` prints it). Paths are the
  exception: kept as written, so `~` stays `~`.
- Grant lists (`rw`, `ro`) and the other string arrays (`listen`,
  `address`, `ssh_aliases`) are `repeated Entry`, so a comment beside one
  entry stays with it. `grant add` writes such comments today (the
  comment of a pasted public key).
- Hosts and exports are `repeated` with a `name`, not maps: the order in
  the file is kept, and a new entry goes last.
- A string array also has a flag that says the key is in the file, so
  `rw = []` stays different from no `rw` at all.
- Blank lines are kept only inside `header`, `footer` and a `tail`. The
  writer decides all others.
- **My call, different from what I first told Mike:** comments sit inside
  the entry they belong to, not in one side table keyed by path. A renamed
  or removed entry then takes its comments with it, with no bookkeeping.

### Reader

`config/reader.rs`: text to message. It keeps every check and every error
message of today's `Config::parse`, with the same line numbers (the tests
in `tests/config_files.rs` pin them). It owns its serde structs.

Comments come from a second pass over the same text with `toml_edit`,
which exposes them as the decoration before and after each table, key and
array element. Where a comment belongs:

- lines directly above a table header, key or array element: its `above`;
- the rest of a line after a value or header: its `trailing`;
- lines after the last key of a table: that table's `tail`;
- lines at the top of the file, set off by a blank line: `header`;
- lines after everything: `footer`.

A file with no table for an entry (dotted keys, inline tables) reads into
the same message; the writer writes it out as plain tables.

### Typed view

`Config { settings, hosts, exports }` and its types stay as they are, so
`daemon.rs`, `authz.rs`, `main.rs` and the session code do not change in
stage 1. `Config::from_message` builds it in one place: it expands `~`,
turns bytes into `EndpointId`, strings into `Url` and `HostPort`, and
applies defaults. `Config::parse(text, path)` stays, as reader then
`from_message`.

`from_message` returns a `Result`: a message edited by a command can be
wrong (a relative export path set with `config set`). That is the check a
save makes before it writes, in place of today's "would this document
load".

**A file that does not load cannot be repaired by a command** (Mike's
decision). Today `config unset` and the like need only valid TOML. The
message cannot hold a wrong value, so such a file is fixed by hand; the
error gives the line. Hints that tell the user to repair a file that does
not load with a command are reworded.

### Writer

`config/writer.rs`: message to text. Fixed order: `header`, top-level
keys, `[client]`, `[daemon]`, `[daemon.discovery]`, `[telemetry]`,
`[hosts.*]`, `[exports.*]`, `[all]`, `[immediate]`, `footer`. Keys inside
a table in the order of the message. One blank line between tables. An
array on one line, unless an entry has a comment: then one entry per line.
Empty tables are not written.

### Management commands

`manage/` edits the message, not TOML text. `ConfigLock::load` returns the
message, `ConfigLock::save` takes it, puts grants in canonical form (as
`grants::canonicalize` does today), checks it with `from_message`, and
writes it. The functions keep their names and what they do; the careful
comment handling in `manage/grants.rs` (`push_entry`, `remove_entry`) goes
away, because an entry now owns its comment.

`config get/set/unset/list` address the message by the same dotted paths.
`kind_of` stays the list of known keys. How a path reaches its field (a
hand-written match or `prost-reflect`, which is a dependency already) is
the builder's choice.

The SSH setup (`ssh/setup.rs`, `ssh/onboard.rs`) and the automatic export
(`authz.rs`) edit through the same functions.

After stage 1 nothing outside `config/reader.rs` and `config/writer.rs`
uses `toml` or `toml_edit` for the config file.

### Tests

- Read then write then read gives the same message, for a corpus of files
  that covers every key, every comment position, inline tables and dotted
  keys.
- Write then read then write gives the same text (the writer's form is
  stable).
- Every comment line of each corpus file is in the written file.
- The existing tests of `manage_*.rs`, `cli.rs`, `ssh_setup.rs` keep their
  cases; where they compare file text, the expected text becomes the
  writer's form.

### What a user notices

The first command that edits the file rewrites it in the writer's form.
Comments and entries stay; hand-made order of tables, blank lines, inline
tables and the spelling of durations do not. The PR description says so,
for the release notes.

## Stage 2

### The message and the file

`Host` goes. In its place:

```proto
message Server {   // [servers.NAME]
  string name; bytes endpoint_id;   // required
  // relay, relay_mode, congestion, address, parallel, reconnect,
  // dial_retries, dht, mdns, ssh, ssh_aliases: as on Host today
}
message Client {   // [clients.NAME]
  string name; bytes client_id;     // required
}
```

The keys keep their names. A server and a client may have the same name:
one machine that is both. The rules for a valid name (`valid_alias`) are
the same for both.

The writer writes `config_version = 2` as the first key, and
`[servers.*]` then `[clients.*]` where `[hosts.*]` was.

### Which reader

- `config_version = 2`: the current reader. A `[hosts]` table is an error
  that says hosts are now `[servers.NAME]` and `[clients.NAME]`.
- `config_version = 1`, or no key and a `[hosts]` table: `legacy_v1`.
- No key and no `[hosts]`: the current reader. (A hand-written new file
  needs no version key. A file with neither table means the same in both.)
- No key, `[hosts]` and also `[servers]` or `[clients]`: an error that
  says to move the `[hosts]` entries.
- A higher number: an error that says a newer irsync wrote the file.

### `legacy_v1`

`config/legacy_v1.rs` is the stage 1 reader, copied with its serde
structs, so a later change to the current reader cannot change it. It may
share the parsers of single values (durations, ids, addresses). It fills
the new message. For each `[hosts.N]`:

- with an `endpoint_id`: a server `N` with the dial keys, `ssh` and
  `ssh_aliases`;
- with a `client_id`: a client `N` with it;
- with only an `endpoint_id`, and `N` named in any grant list (`[all]`,
  `[immediate]`, an export's `rw` or `ro`): also a client `N` whose
  `client_id` is that `endpoint_id`. This is "keep the access". **My
  call:** this client entry gets a comment line saying it was the
  `endpoint_id` of `[hosts.N]`, so the file says why a daemon's id is let
  in;
- with only a `client_id` but dial keys: the dial keys are dropped (they
  could never be used). **My call:** the command that rewrites the file
  names them on stderr.

Comments: the table's own go to the server when there is one, else to the
client. The comments of the `client_id` key go to the client entry.

Because reading an old file and rewriting it are the same reader followed
by the same writer, irsync behaves the same before and after the first
edit. Any save writes version 2: a management command, the SSH setup, the
daemon adding an automatic export.

After the rewrite an older irsync cannot read the file. The PR description
says so, for "Upgrading" in the release notes.

**An older daemon that is still running** (Mike's decision): a save that
would change the file's version is refused while a daemon runs that cannot
read the new version. The daemon reads the config on every connection, so
without this it would turn every client away after a harmless `grant
add`. The daemon's lock records its version (`LockInfo.version`). The
check sits in `ConfigLock::save`, so it covers the management commands
and the SSH setup, which leaves a daemon from another executable alone.
The error says to restart the daemon and run the command again. A constant
names the first irsync version that reads each file version; version 2's
is the release stage 2 ships in. The file is left as it was.

### Typed view and its users

`Config` gets `servers` and `clients` in place of `hosts`.
`HostEntry::peer_id` is deleted. Who reads what:

- dialling, `by_ssh`, the per-host dial settings (`main.rs`): servers;
- the grant check (`authz::Snapshot::from_files`), `PeerSpec`, the
  canonical form of grants, `alias_of`, `show`'s grant lines, the name a
  daemon logs for a peer: clients.

### Commands

- `irsync server add NAME ID [the flags of host add]`, `server remove
  NAME`, `server rename OLD NEW`, `server list`. No grant ever names a
  server, so `remove` has no `--force` and `rename` touches no grant.
- `irsync client add NAME ID`, `client remove NAME [--force]`, `client
  rename OLD NEW`, `client list`. `remove` is refused while grants name
  the client, `--force` removes them too, `rename` renames them: what
  `host remove` and `host rename` do today.
- `irsync host ...` stays as a hidden command that fails and names the two
  new ones. Without it the words would be read as a sync.
- `grant add EXPORT WHO`: `WHO` is a client name, an id, a public key
  line or key file. A name that is only a server is an error that says how
  to add the machine as a client.
- `irsync NAME:PATH`: `NAME` is looked up in servers only. A name that is
  no server is treated as today's unknown name: an ssh destination to set
  up, after which it is saved as a server. That covers a name that is only
  a client (today's "saved as a client only; setting it up over ssh" in
  `main.rs`): the flow stays and writes a server beside the client. **My
  call.**
- `config get/set/unset`: `servers.NAME.KEY` and `clients.NAME.client_id`.
  `hosts.*` is an error with the same hint.
- `show`: a `servers:` and a `clients:` section.

### SSH setup

On the client it writes the server entry (`ssh/onboard.rs`, today
`host_add` and `host_add_ssh_alias`). On the server it writes the client
entry (`ssh/setup.rs`, today `host_set_client`). The rules in
`ssh/setup.rs` about a name that is already a host with another id shrink
to two, both kept from `check_client_name`: the name is already a client
with another id; and grants still mention the name while no client entry
says who it is (a hand-edited list, or a name that is only a server), so a
new client cannot inherit leftover grants. **My call** to keep the second.
The notes on stderr that explain which id a host's grants follow go away.

### Book

Configuration, Management commands, Command line, Remote addresses, Files
and state, Identity and trust, the first-sync tutorial, SSH setup, Run the
daemon, Direct and relay, Troubleshooting; the man pages built from them
(`tools/build-man.sh`). Configuration gets a short part on
`config_version` and on what happens to an old file.

### Tests

- A corpus of version 1 files with the expected version 2 text beside
  each. It covers: both ids, only `endpoint_id` (granted and not granted),
  only `client_id` (with and without dial keys), comments on the table and
  on each key, grants by name and by id.
- For each corpus file: the grants (`authz::Snapshot`) read from the old
  file equal those read from the rewritten one.
- The reader choice: each of the five cases above.
- A save of a version 1 file while the daemon lock names an older daemon
  is refused and leaves the file unchanged; with no daemon, or a current
  one, it goes through.
- An end-to-end test: a daemon started on a version 1 file lets its
  clients in, adds an automatic export, and the file is then version 2
  and still lets them in.
- The existing tests move to the new commands and tables.

## Left to the builder

- Names of fields, modules and functions beyond those given.
- Whether `Config::load` skips the comment pass when nothing will be
  written.
- How `config get/set` reach a field.
- The exact wording of errors and hints, in the style of the existing
  ones.
- The order of work inside each stage.
