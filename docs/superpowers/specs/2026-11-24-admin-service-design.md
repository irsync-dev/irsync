# Admin service design

Issue: #203. Depends on: #284 (server-wide rights move onto the client entry).
Follow-ups: #286 (sync-only SSH setup).

## Why

Managing an irsync server today needs a shell on it: `irsync export add`,
`irsync grant add`, `irsync client add` all run on the server and edit its
`irsync.toml`. The admin service lets a client with the right manage the
server over irsync's own connection (`irsync/1`). It is also the way in for a
future web interface, which will talk to the daemon as an ordinary client.

## Goals

- A client whose entry carries `admin = "rw"` can change the server's
  exports, clients and grants remotely; `admin = "ro"` can read them.
- Several changes can be applied as one unit: all of them or none. The CLI
  does not use this yet; the web interface and a later interactive shell will.
- Every remote change is logged on the server with the client's name.
- Checks run on the server, where the files are.

## Non-goals

- Remote `config set` / `config unset` (the `[daemon]` settings). Left out
  for now.
- Remote `export update`. Left out for now.
- Remote `id generate` (never), and remote `id list/show`, `server *`,
  `sync`, `list`, `gc`, `index`, `daemon` (these are local by nature).
- Pushed news (live status, change feeds). That would be a separate stream
  kind, not built now.
- An interactive shell, or several changes in one CLI call. The server
  supports transactions now; the CLI sends one change per transaction.
- Moving the SSH setup onto the admin service. The SSH setup stays as it is.
- Backward compatibility: irsync has no users yet, so the protocol and config
  change freely.

## Anti-goals

- A refusal must never tell a client without `admin` anything about the
  server: not which exports exist, not why it was refused.
- A failed transaction must never leave the config half changed.
- `admin = "ro"` must not be a way to read anything beyond what the read
  commands show.

## The right

On the client entry, beside `all_exports` and `all_paths` from #284:

```toml
[clients.mylaptop]
client_id = "3f1a…"
all_exports = "rw"
all_paths = "rw"
admin = "rw"
```

- `admin = "rw"`: read, and change exports, clients and grants.
- `admin = "ro"`: read status and config only.
- Left out: no admin access.
- Only named clients (`[clients.NAME]`) can hold it.

`admin = "rw"` is as strong as `all_paths = "rw"`: either can get the other
(an admin can grant itself `all_paths`; `all_paths` can write
`irsync.toml`). Both are "the whole account" in the threat model. So:

- The SSH setup sets `admin = "rw"` together with `all_paths = "rw"`, with
  no flag. A sync-only setup is #286.
- On the server, `irsync client set NAME admin=rw` gives it by hand (the
  `client set` command comes from #284, shaped to take a third key).
- `book/src/explanation/threat-model.md` says `admin = "rw"` is the whole
  account, like `all_paths = "rw"`.

## How requests travel: the approaches

Three were considered.

- **A. One request per stream.** `Hello` kind `ADMIN` plus one request, one
  reply, the stream ends. Simple and stateless, but cannot apply several
  changes as one unit.
- **B. An admin session (picked).** One stream carries many requests and
  replies, with transactions. Costs some server state per stream; needed
  because the web interface will want several changes applied together.
- **C. Fetch the config, edit it on the client, upload it** with a
  compare-and-swap. Every command works remotely for free, but checks run on
  the client (wrong place), the audit log needs a diff, concurrent admins get
  retries, and a read-only admin receives the whole file.

The user picked B, for transactions.

## The session

1. The client opens a stream on a connection and sends `Hello` with the new
   direction `ADMIN` (other `Hello` fields empty). The daemon looks up the
   client's `admin` right. A client without one gets the plain refusal and the
   stream closes.
2. The stream is now inside a transaction (an implicit begin).
3. The client sends **changes**. The server checks only that each is well
   formed and stores it. It does not answer each change.
4. The client sends **COMMIT**. The server:
   - takes the config lock (`manage::lock_config`), waiting for it up to
     `authz::LOCK_WAIT` the way `FileAuthorizer::add_auto_export` does (the
     local commands fail at once instead); if it is still held, NACK "busy"
     (my call);
   - loads the config (`manage::load_message`);
   - checks that the client still has `admin = "rw"` in that config;
   - applies the changes in order to the in-memory config, with the same
     manage functions the local commands use (`exports::add_export`,
     `grants::grant_add`, `clients::client_rename`, …), which already work on
     a `pb::Config` in memory;
   - checks the end result: the committing client must still exist and still
     hold `admin = "rw"` (a client cannot remove or downgrade itself
     remotely);
   - if everything worked, writes the file once (`manage::save_config`) and
     answers **ACK**; otherwise writes nothing and answers **NACK**.
5. ACK or NACK ends the transaction. The next one begins at once, on the same
   stream.
6. **Reads** (list, show, config get/list) are answered at once, from the
   saved config, outside the transaction. They do not see pending changes.
   They need `admin = "ro"` or `"rw"`.

A client with only `admin = "ro"` that sends a change gets a NACK at commit
(or a refusal at once; the builder's call), and nothing changes.

Edges:

- Closing the stream without COMMIT drops the pending changes.
- A COMMIT with no changes answers ACK and writes nothing.
- A transaction holds at most a fixed number of changes (say 100); one more
  is refused and the transaction is dropped.
- An idle session times out and closes.
- Many sessions may run at once, on one connection or many. The lock is held
  only during a commit, so two admins collecting changes at the same time do
  not block each other; the later commit sees the earlier one's result.

### No version check

A commit carries no "only if unchanged since I read it" condition: the last
commit wins, as long as every change in it applies cleanly to the config as
it is at commit time; otherwise it is a NACK. (Considered and turned down: a
config version in read replies and an optional one on COMMIT.)

### NACK

To a client with `admin = "rw"`, a NACK names the change that failed (its
place in the transaction) and the reason, such as "change 2: /srv/x is not a
directory". To anyone else, a refusal is the plain one that reveals nothing.

### Other calls made while writing this (mine)

- The server knows the committing client by its EndpointId and looks up its
  entry in the config it loaded for the commit.
- Every read checks the admin right again against the saved config.
- ACK carries a result for each change, so the CLI can print what the local
  command prints (for example how many grant lines a client removal took).
- For `grant add` and `client add`, the CLI reads a key file or key line on
  the client and sends the EndpointId, not the file.

### Replies carry data

Read replies carry structured data (exports with their paths and grants,
clients with their rights, config keys and values), not finished text. The
CLI prints them exactly as the matching local command does; a web interface
shows them its own way.

## Remote commands

| Group | Remote, `admin = "ro"` | Remote, `admin = "rw"` |
|---|---|---|
| exports | `export list`, `show` | `export add`, `export remove` |
| grants | `grant list` | `grant add`, `grant remove` |
| clients | `client list` | `client add/remove/rename/set` |
| config | `config get`, `config list` | — |

`export add` takes an absolute path or one starting with `~` (the server's
home), checked on the server. Each change in the protocol is its own message
type, mirroring the local command's arguments.

## The CLI

`irsync remote SERVERNAME COMMAND ...`

- SERVERNAME is a `[servers]` name or an EndpointId.
- It only dials a daemon that is already running. It reuses sync's dialling
  code where that splits out cleanly, but never starts an SSH setup as a side
  effect (my call).
- COMMAND is one of the remote commands above, with the same arguments as the
  local one.
- A change command opens a session, sends one change and COMMIT, and prints
  the result like the local command does. A read sends one read.
- A path that looks like an already expanded local home (the shell turned
  `~/Photos` into `/home/me/Photos`) is refused, with a hint to quote it:
  `irsync remote myserver export add photos '~/Photos'`.
- `irsync remote --help` lists the commands that work remotely.

## Protocol

- New `Hello.Direction` value `ADMIN`.
- `PROTOCOL_VERSION` goes from 5 to 6.
- An older daemon refuses the unknown direction; the CLI turns that into a
  plain message: the server's irsync is too old for remote admin, update it.
- New messages for: the changes (one per command), COMMIT, ACK, NACK, the
  reads and their replies.

## Audit log

- Each applied change: one info line with the client's name and what changed
  (for example `admin: mylaptop added export photos at ~/Photos`).
- A NACK or a refused session: one warning line with the client's name (or
  EndpointId) and no detail.
- Reads are not logged.

## Order of work

1. #284 first: rights on the client entry and `client set`.
2. Then this issue. Write its plan only after #284 is merged, so the
   clients and grants messages follow what #284 actually built (my call).

## After #284 (checked against the merged code)

- `admin` becomes the third entry of `config::clients::RIGHTS` (`Right::Admin`),
  so the reader, writer, `client set`, `client list`, `grant list` and the
  "still holds a right" note of `grant remove` pick it up. The two matches
  that name each right (`Right::of`, `ClientEntry::from_message`) grow by
  one arm each.
- The daemon closes a connection whose key has no grants before the Hello
  (`Grants::is_empty`). A client whose only right is `admin` must pass that
  gate, so `Grants` gets `admin: Option<Access>` and `is_empty` counts it.
- The daemon serves one control stream per connection. An admin session is
  that stream, so there is one admin session per connection; a web interface
  that wants two at once opens two connections (my call: the daemon's accept
  path stays as it is).
- An older daemon checks the protocol version before the direction, so it
  answers `VERSION_MISMATCH` ("daemon speaks 5"). The CLI turns that into
  "myserver runs an older irsync without remote admin: update it there".
- `irsync remote` dials with the same code as `irsync list` (`connect_for`),
  which never sets a server up.

## Left to the builder

- The message names and layout in the proto files.
- Whether an `admin = "ro"` client's change is refused at once or at commit.
- The transaction size limit and the idle timeout.
- How the daemon runs the blocking lock and file work off the async runtime
  (`lock_config` blocks; see `FileAuthorizer::add_auto_export`).
- How the CLI shares printing code between the local and remote commands.
- The tests: at least a transaction where the last change fails and the
  config is unchanged; two admins committing at once; a client trying to
  remove its own admin right; an older daemon; a client without `admin`.
- The book: a page on remote admin, the `admin` key in the configuration
  reference, the threat model.
