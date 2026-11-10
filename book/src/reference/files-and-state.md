# Files and state

What `irsync` reads and writes outside the trees it syncs, and what it leaves
inside them.

## Config directory

All under `~/.config/irsync/`, all optional. There is no lookup under `/etc`; a
daemon run system-wide is given `--config` (see
[Run the daemon](../how-to/run-the-daemon.md#run-it-as-its-own-account)).

| File | Holds |
| --- | --- |
| `irsync.toml` | The whole configuration: `identity`, `state_dir`, `[client]`, `[daemon]`, `[telemetry]`, the `[hosts.*]` aliases, the `[exports.*]` tables and the `[all]` grants. `--config PATH` names another file; the files below are then looked up beside it. See [Configuration](configuration.md). |
| `hosts.toml`, `exports.toml` | Optional split files, read as part of `irsync.toml`. `irsync` never edits them. See [Configuration](configuration.md#split-files). |
| `irsync.toml.lock` | Present only while a management command edits the file. See [Management commands](management.md#editing-rules). |
| `id_ed25519`, `id_ed25519.pub` | The client's key, generated when no other key serves. See [Configuration](configuration.md#identity-and-state). |
| `daemon_ed25519`, `daemon_ed25519.pub` | The daemon's key, `[daemon] identity`, generated when missing by the daemon, `irsync export add` or `irsync show`, or on demand by `irsync id generate --daemon`. |

Generated private keys are written with mode 0600, in OpenSSH format. The
`.pub` file beside each holds the public key line. `irsync id show` prints the
`EndpointId` of either key; see [Management commands](management.md#id).

## State directory

`state_dir` defaults to `~/.local/state/irsync`, or `$XDG_STATE_HOME/irsync` on
Linux when that is set. The client and the daemon use the same layout.

| Path | Holds |
| --- | --- |
| `index/<hash>.sqlite` | One SQLite database per root: a local directory the client synced or indexed, or an export. `<hash>` is 32 hex digits of the BLAKE3 hash of the root's canonical path; the database records the path too. |
| `index/<hash>.sqlite-wal`, `-shm` | SQLite's write-ahead log and shared memory, beside each database. |
| `daemon.lock` | Held by the running daemon, so a second daemon with the same state directory refuses to start. It holds the daemon's `pid`, the path of its program (`exe`) and its `version`, one `key=value` line each. The ssh setup reads it to tell whether a daemon runs and which build it is. |
| `daemon.log` | The log of a daemon the ssh setup started. A daemon you start yourself logs to stderr instead. |

Each database holds:

- the file hashes of the root, which let a session skip unchanged files and
  reuse chunks of renamed and copied ones;
- the progress of partial files, so that an interrupted transfer resumes where
  it stopped;
- dormant sessions, so that the next run of the same sync resumes its plan;
- the time of the last garbage collection.

Exports with the same path share one database.

The whole directory is safe to delete. The next sync rebuilds what it needs,
at the cost of hashing files again and of losing the sessions that could have
resumed. [Clean up state](../how-to/clean-up.md) removes only what nobody is
coming back for.

## On a server set up over ssh

The [ssh setup](../tutorial/first-sync.md#the-quick-way-over-ssh) puts its own
copy of `irsync` on the server at `~/.local/share/irsync/bin/irsync`. It
replaces that copy when the client runs a different build, and leaves an
`irsync` you installed elsewhere alone. Over ssh it then runs `irsync
ssh-setup`, a hidden command that grants the client, makes the daemon's key
when it is missing and starts the daemon. The server's config, keys and state
are the usual ones above; the daemon it starts writes `daemon.log` in the
state directory.

## In the synced trees

A receiver writes into temporary files beside their destinations and renames
them into place:

| Name | What it is |
| --- | --- |
| `.irsync.<name>.part` | A file being received. An interrupted transfer leaves it behind for the next run to resume. With `--partial-dir DIR` these files live under `DIR` instead. |
| `.irsync.<name>.tmp` | A symlink, hard link or special file being made, renamed into place at once. |

When the full name would exceed 255 bytes, `<name>` is cut and `~` plus 16
hex digits of a BLAKE3 hash of the whole name is put before the suffix, so two
long names with the same beginning still get different temporary files.

Names that start with `.irsync.` and end in `.part` or `.tmp` are never
listed, never accepted from a peer and never created as destinations.

Garbage collection removes `.part` files and their progress records once
they have gone untouched for longer than `dormant_session_timeout` (7 days by
default; the client always uses 7 days), unless a stored session still lists
the file. It removes only regular files with that exact form of name, and
never follows a symlink. Progress records do not remember a `--partial-dir`:
a stale record whose `.part` file is not beside its destination is dropped,
and the file itself, if it exists, is collected on a later pass by its age.
When and how often collection runs is in
[Clean up state](../how-to/clean-up.md#what-runs-on-its-own).
