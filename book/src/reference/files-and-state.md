# Files and state

What `irsync` reads and writes outside the trees it syncs, and what it leaves
inside them.

## Config directory

All under `~/.config/irsync/`, all optional. There is no lookup under `/etc`; a
daemon run system-wide is given `--config` (see
[Run the daemon](../how-to/run-the-daemon.md#run-it-as-its-own-account)).

| File | Holds |
| --- | --- |
| `irsync.toml` | The whole configuration: `identity`, `state_dir`, `[client]`, `[daemon]`, `[telemetry]`, the `[servers.*]` and `[clients.*]` tables, and the `[exports.*]` tables. `--config PATH` names another file; the files below are then looked up beside it. See [Configuration](configuration.md). |
| `irsync.toml.lock` | Present only while a management command edits the file. See [Management commands](management.md#editing-rules). |
| `id_ed25519`, `id_ed25519.pub` | The client's key, generated when no other key serves. See [Configuration](configuration.md#identity-and-state). |
| `daemon_ed25519`, `daemon_ed25519.pub` | The daemon's key, `[daemon] identity`, generated when missing by the daemon, `irsync export add` or `irsync show`, or on demand by `irsync id generate --daemon`. |

Generated private keys are written with mode 0600, in OpenSSH format. irsync
refuses a key that other users can read or write; see
[File permissions](configuration.md#file-permissions).
The `.pub` file beside each holds the public key line. `irsync id show` prints the
`EndpointId` of either key, under a `client:` or `daemon:` label; see
[the `id` commands](management.md#id).

A new `irsync.toml` is written with mode 0600, since it names exports, peers
and grants. An existing one keeps the mode it has when a management command
saves it. irsync refuses a config file that other users can read or write,
unless the file sets `unsafe_config_allowed = true`.

## State directory

`state_dir` defaults to `~/.local/state/irsync`, or `$XDG_STATE_HOME/irsync` when
that is set. The client and the daemon use the same layout.

| Path | Holds |
| --- | --- |
| `index/<hash>.sqlite` | One SQLite database per root: a local directory the client synced or indexed, or an export. `<hash>` is 32 hex digits of the BLAKE3 hash of the root's canonical path; the database records the path too. |
| `index/<hash>.sqlite-wal`, `-shm` | SQLite's write-ahead log and shared memory, beside each database. |
| `daemon.lock` | Held by the running daemon, so a second daemon with the same state directory refuses to start. It holds the daemon's `pid`, the path of its program (`exe`) and its `version`, one `key=value` line each. `irsync pair` reads it to tell whether a daemon runs and which build it is. |
| `daemon.addr` | Where the running daemon can be reached right now: its `pid`, its home `relay` and each direct `addr`, one `key=value` line each, rewritten whenever they change. `irsync pair` passes them to the client, so the first sync to a daemon it just started does not wait for the daemon to appear in the DHT or DNS. |
| `daemon.log` | The log of a daemon that `irsync pair` started in the background or made a launchd agent on macOS, at info level (`-vv`). A daemon that runs as a systemd service logs to the service manager (`journalctl --user -u irsync`). A daemon you start yourself logs to stderr instead. |

Each database holds:

- the file hashes of the root, which let a session skip unchanged files and
  reuse chunks of renamed and copied ones;
- the progress of partial files, so that an interrupted transfer resumes where
  it stopped;
- dormant sessions, so that the next run of the same sync resumes its plan;
- the time of the last garbage collection.

Exports with the same path share one database.

The databases hold the name and hashes of every file in the root. The `index`
directory is therefore kept at mode 0700, and irsync creates a missing state
directory with mode 0700 too. `daemon.log` is created with mode 0600.

The whole directory is safe to delete. The next sync rebuilds what it needs,
at the cost of hashing files again and of losing the sessions that could have
resumed. To remove only what nobody is coming back for, see
[Clean up state](../how-to/clean-up.md).

## On a server paired over ssh

`irsync pair` (see [Your first sync](../tutorial/first-sync.md#the-quick-way-over-ssh))
installs on the server the same files as `install.sh`: `irsync` and
`fileset` in `~/.local/bin`, and `irsync.1`, `fileset.1` and `irsync.toml.5`
under `~/.local/share/man`. All five come along when `pair` sends release
files; when it sends the client's own files, `fileset` and the manual pages
come only if they sit beside the client's `irsync`. It replaces an `irsync` there when the client runs a
different build. It does not touch an `irsync` installed elsewhere. It does not
use `~/.local/share/irsync/bin` any more (see
[Run the daemon](../how-to/run-the-daemon.md#service-files-from-an-older-irsync)).
Over ssh it then runs `irsync
ssh-setup`, a hidden command that grants the client, makes the daemon's key
when it is missing and starts the daemon, as a user service when it can: `~/.config/systemd/user/irsync.service` on Linux or `~/Library/LaunchAgents/org.irsync.daemon.plist` on macOS, each marked "Written by irsync pair". The server's config, keys and state
are the usual ones above; a daemon it starts in the background writes `daemon.log` in the
state directory.

## In the synced trees

A receiver writes into temporary files beside their destinations and renames
them into place:

| Name | What it is |
| --- | --- |
| `.irsync.<name>.part` | A file being received. An interrupted transfer leaves it behind for the next run to resume. With `--partial-dir DIR` these files live under `DIR` instead. It has mode 0600 until it is placed, so other users cannot read a file while it arrives. |
| `.irsync.<name>.tmp` | A symlink, hard link or special file being made, renamed into place at once. |

When the full name would exceed 255 bytes, `<name>` is cut and `~` plus 16
hex digits of a BLAKE3 hash of the whole name is put before the suffix, so two
long names with the same beginning still get different temporary files.

Names that start with `.irsync.` and end in `.part` or `.tmp` are never
listed, never accepted from a peer and never created as destinations.

With `--paranoid`, the sending side keeps reflink clones in a folder named
`.irsync-clones-<uid>` (the uid in decimal) in a writable directory on each
file's filesystem. A directory named exactly `.irsync-clones-` followed by
digits is never listed and so never synced or deleted; a name such as
`.irsync-clones-backup` is an ordinary directory.

Garbage collection removes `.part` files and their progress records once
they have gone untouched for longer than `dormant_session_timeout` (7 days by
default; the client always uses 7 days), unless a stored session still lists
the file. It removes only regular files with that exact form of name, and
never follows a symlink. Progress records do not remember a `--partial-dir`:
a stale record whose `.part` file is not beside its destination is dropped,
and the file itself, if it exists, is collected on a later pass by its age.
When and how often collection runs is in
[Clean up state](../how-to/clean-up.md#what-runs-on-its-own).
