# Single-file sources

Date: 2026-11-13. Status: approved for planning.

Removes the v1 limit "Single-file sources are refused in v1 (directories
only)" (`docs/IRSYNC_DECISIONS.md`, §2.3). It changes what a copy accepts
as its source and where the copied file lands. It doesn't change flags,
the config files, the management commands or how a directory is copied.

## Intent

The user ran

```sh
irsync ubuntu-26.04.1-desktop-amd64.iso eowyn:tmp
```

and it failed with `source … is not a directory`. Copying one big file,
such as an ISO, is an everyday rsync line, and irsync promises to read
like rsync. A single file should work in both directions and land where
rsync would put it. A big file is exactly where irsync's strengths count
most: delta transfer against an older copy, and resuming after a dropped
connection.

## Why it fails today

Every session is "one root directory on each side":

- The sender refuses a root that isn't a directory
  (`session/sender_role.rs`, `bail!("source {} is not a directory")`).
- `resolve_roots` (`session/mod.rs`) applies the trailing-slash rule by
  sending the source's basename as `Hello.name`. The daemon joins that
  name below the remote path (`daemon.rs`, `admit`), and the result
  becomes the receiving root directory (`engine.rs`, `prepare_root`,
  which requires or creates directories).
- On a pull, `resolve_roots` joins the remote path's last component onto
  the local destination and treats the result as the local root directory.

## Goals

The same placement rules as rsync, in both directions. Below, `T` is the
destination path as typed.

| Command | `T` is a directory | `T` doesn't exist | `T` is a file |
| --- | --- | --- | --- |
| `irsync f.iso eowyn:T` | `T/f.iso` | a file named `T` | `T` is updated |
| `irsync f.iso eowyn:T/` | `T/f.iso` | directory `T` is created, then `T/f.iso` | error |
| `irsync eowyn:dir/f.iso L` | `L/f.iso` | a file named `L` | `L` is updated |
| `irsync eowyn:dir/f.iso L/` | `L/f.iso` | directory `L` is created, then `L/f.iso` | error |

- When `T/f.iso` (or `L/f.iso`) already exists and is a directory, the
  copy is an error, as in rsync. No directory is ever replaced by a file.
- **Updating an existing file uses the normal machinery:** delta transfer
  against the old content, `.part` files, `--inplace`, `--append`, `-c`,
  `-u`, `--dry-run`, `-i` itemizing, metadata flags (`-a`, `-t`, `-p`, and
  so on) and progress all behave as they do for a file inside a directory.
- **Resume works.** A dropped connection during a single-file copy
  resumes like a directory session. The session key must identify the
  single-file session so that a rerun of the same command resumes it, and
  so that it never collides with a directory session at the same place.
- **Several sources on a push** (`irsync a.iso photos/ eowyn:T/`) work
  when some sources are files. As with rsync, when there are several
  sources the destination is always a directory. Each file source lands
  as `T/<basename>` and is never renamed.
- **Listing one file:** `irsync eowyn:dir/f.iso` with no destination
  prints that one file in the usual listing format and exits 0.
- **A trailing slash on a file source** (`f.iso/`, or `eowyn:dir/f.iso/`)
  is an error, as in rsync.

## Non-goals

- Local-to-local copies. One side is always remote, as today.
- `--mkpath`. The missing-parent rules for a single-file destination are
  the same as for a directory destination today (see Constraints).
- `--delete`, `--delete-excluded` and `--max-delete` have nothing to act
  on for a single-file session. They are accepted and do nothing, as in
  rsync. Nothing else in the destination directory is ever touched.
- Several sources on a pull. Not supported today and still out of scope.

## Anti-goals

- A single-file copy must never delete, rename or rewrite any other
  entry in the destination directory. That includes "cleaning up" the
  directory as a side effect of treating it as a session root.
- A single-file push must not let the client place a file outside the
  export, or follow a symlink out of it. The existing receiver safety
  rules (`docs/IRSYNC_DECISIONS.md` §1.5, `fs/safe.rs`) apply to the
  final file path and to every directory above it.
- No silent fallback. If the daemon or client is too old to support
  single files, the user gets a clear error, never a directory-shaped
  copy of the wrong thing.
- Directory copies must behave exactly as before.

## Constraints

- **Protocol.** Client and daemon must speak exactly the same
  `PROTOCOL_VERSION`; the daemon rejects any other (`daemon.rs`). Single
  files need new information on the wire, so `PROTOCOL_VERSION` goes from
  3 to 4 (my call, following earlier changes). An old client against a
  new daemon, or the other way round, gets the existing "protocol version
  unsupported" error. The book's protocol reference
  (`book/src/reference/protocol.md`) is updated.
- **Who decides placement.** Only the receiving side can see whether `T`
  exists and what it is: on a push, the daemon; on a pull, the client.
  Only the sending side knows the source is a file: on a push, the
  client; on a pull, the daemon. So:
  - On a push, the client tells the daemon in `Hello` that the source is
    a single file, with its basename. The daemon resolves the path,
    applies the table above, and enforces the export boundary on the
    final path.
  - On a pull, the client can't know before connecting that the remote
    path names a file. The daemon tells it in `HelloAck` (or an
    equivalent early message) that the source is a single file, and the
    client then applies the table above to its local destination. Today
    `resolve_roots` decides the local root before the connection
    exists, so that step has to move or be revisited for this case.
- **Missing parents.** Whatever irsync does today when the directories
  above a directory destination are missing (`prepare_root` with
  `Missing::Create`, `Refuse` or `Allow`) applies unchanged to the
  directories above a single-file destination. A dry run creates nothing.
- **Filters.** `--include` / `--exclude` apply to a file source the way
  rsync applies them: matched against the file's name. An excluded single
  file is simply not copied (exit 0, nothing transferred). (My call.)
- **Automatic exports (the "immediate" grant).** Today `auto_root`
  (`autoexport.rs`, called from `add_immediate` in `daemon.rs`) runs
  before `admit` and before any placement decision. On a push to a
  missing path, it creates that path as a directory and exports it. For a
  single-file push this must change. The daemon has to know the source is
  a single file before `auto_root` runs. When `T` is missing or is a file,
  it exports `T`'s parent (it must already be a directory) and creates
  nothing, which matches what already happens for an existing file and
  for pulls. When `T` is a directory, `T` is exported, as today. When the
  destination ends in `/`, the current rule is unchanged: create `T` as a
  directory and export it. This gives the client lasting read-write access
  to the whole parent directory. The user chose that, knowing what it
  costs.
- **Symlinks and special files as the source.** A single source that is
  not a regular file is handled the way the same entry would be inside a
  directory copy (my call, matching rsync): a symlink is copied as a link
  with `-l`/`-a` and skipped with a notice otherwise; a device or special
  file is copied with `-D`/`-a` and skipped otherwise. A source symlink
  is never followed. Today the push sender follows a symlinked root
  (`std::fs::metadata` in `sender_role.rs`); that stays as it is for
  directories and doesn't apply to single files.
- **Symlinks on the destination side.** No change: the existing rules for
  each side apply to the directories above the final file. The daemon
  refuses symlinks in the path, and the client follows them, as today.
- **Several sources with `--delete`.** Each source runs as its own
  session, one after another (`main.rs`). So in
  `irsync --delete a.iso photos/ eowyn:T/`, the `photos/` session deletes
  the `T/a.iso` that was just copied. rsync doesn't, because it merges the
  sources into one list. The same already happens today with `a/ b/`.
  This is accepted as a known limitation, not fixed here (my call), and is
  noted in `book/src/reference/limitations.md`.
- **Resume key on a pull.** The client looks up a stored session before it
  connects. Today it also creates the local folder, opens the index and
  cleans up there before connecting. For a single-file pull, base the key
  on the paths as typed. Create nothing locally until the daemon has said
  the source is a file, so the anti-goals hold.
- **Temporary files.** `.part` files (or `--partial-dir`) go next to the
  final file, as for a file inside a directory.
- **Tests.** Follow `AGENTS.md`. New end-to-end tests run in-process. The
  `rsync_compare.rs` suite should gain cases comparing every row of the
  placement table against GNU rsync, both directions where the harness
  allows. A dropped connection mid-file should be covered through the
  paused-clock netsim helper (`with_netsim`), not an ignored test.
- **Docs.** Update `book/src/reference/remote-addresses.md` (trailing
  slashes, with single-file rows), the `SyncArgs` help text in
  `cli/mod.rs`, the CLI reference, and remove the line from
  `docs/IRSYNC_DECISIONS.md` §2.3, adding a ruling for the new behavior
  under §1.

## Decided

- Rsync's placement rules, as in the table, both push and pull (user).
- Automatic exports export the parent of a single-file destination (user).
- Symlink and special-file sources follow the in-directory rules; several
  sources with `--delete` stay a known limitation (my calls).
- Protocol version bump to 4, no compatibility shim (my call).
- `--delete` and friends are a no-op for a single-file session (my call,
  matching rsync).
- Listing a single remote file shows just that file (my call).
- Filters apply to the file's name (my call, matching rsync).

## Left to the builder

- How a single-file session is represented internally. One route: the
  session root is the file's parent directory, and the listing holds
  exactly one entry whose name on the receiving side may differ from the
  source's name (the "file named `T`" case). Another: a session whose
  root is the file itself. Pick whichever keeps the receiver's safety
  checks, the index and resume simplest. Whichever is chosen, the
  anti-goals above (no other entry touched, no escape from the export)
  must hold.
- The exact new fields in `Hello` / `HelloAck` and their names.
- The exact error messages, provided they name the path and say what was
  expected (for example, "`T` is a file, but the destination ends in /").
