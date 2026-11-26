# Remote export set, rename and indexing

Issue: #300 (part 2). Builds on the admin service (#203,
`docs/superpowers/specs/2026-11-24-admin-service-design.md`).

## Why

The user is building a web interface that manages a server's clients and
exports over the admin protocol. Clients are already complete remotely
(add, remove, rename, set). Exports are not: a web interface cannot flip
an export's flags, cannot rename one, and cannot have a new export's files
hashed so that syncs into it reuse what is already there.

The issue's part 2 also named remote `config set`/`unset` (the
`[daemon]` settings) and remote `export update`. `export update` no
longer exists: it became `irsync index --export NAME`. Remote `config set`
is not needed for clients and exports, so it stays out (see Non-goals).

## Goals

- A client with `admin = "rw"` can run `export set` and `export rename`
  remotely, as changes in a transaction, like the other changes.
- A client with `admin = "rw"` can start hashing an export's files into its
  index (what `irsync index --export NAME` does locally), for example right
  after adding the export. The daemon does the work in the background.
- A client with `admin = "ro"` or `"rw"` can ask which indexing runs are
  going on, with their progress, and how the last run of each export ended.
  The web interface polls this to show progress.
- `irsync remote` gets the same commands, so the CLI and the web interface
  cover the same ground.

## Non-goals

- Remote `config set` / `config unset`. Nothing the web interface needs
  for clients and exports depends on it. It comes back when something does,
  with its own decision on which `[daemon]` keys are safe remotely.
- Pushing progress to the client. The client polls (decided by the user).
  Part 3 of #300 (a news stream) stays separate.
- Stopping a run that has started. A run ends when it is done or when the
  daemon stops.
- Seeing runs of `irsync index` started on the server by hand: those run in
  another process. Only the daemon's own runs show.
- Keeping run history across a daemon restart. The status lives in memory.
- A percentage. The indexer walks the folder and hashes as it goes; it does
  not know the total in advance. Progress is counts.

## Anti-goals

- `admin = "ro"` must not become a way to read file names inside an
  export. The status carries counts only, never the paths of files that
  failed; those go to the daemon's log.
- An indexing request must never run inside a transaction or hold the config
  lock: hashing can take hours.
- Two runs must never hash the same folder at once.

## Approaches

Three ways to start indexing were considered.

- **A. A request of its own (picked).** "Index this export", sent outside
  transactions like a read, needs `admin = "rw"`, answered at once
  ("started" or "already running"). To add and index an export, the client
  commits `export add` and then sends this request. The same request
  re-indexes later.
- **B. A flag on `export add`:** indexing starts when the commit succeeds.
  One step for the client, but it starts hashing from inside a transaction,
  and re-indexing later still needs A. So it is A plus more.
- **C. No new request:** adding an export with `index_at_start` would also
  index it at once. Nothing new on the wire, but it changes what the flag
  means and gives no way to re-index.

The user picked A.

## Part 1: `export set` and `export rename` as changes

Two new change messages, beside the existing ones, mirroring the local
commands' arguments:

- `export set NAME KEY=VALUE...`: `read_only`, `index_at_start`, `fsync`,
  each `true` or `false`, as the local command takes them
  (`manage::exports::parse_setting`, `export_set`).
- `export rename OLD NEW` (`manage::exports::rename_export`).

They go through `manage::admin::apply` like the others: applied in order,
all or nothing, and the ACK carries the line the local command prints. Each
gets an audit summary (`set export photos read_only=true`,
`renamed export photos to pictures`).

Note for the reader: an export's index belongs to its path, not its name, so
a rename keeps the index.

## Part 2: starting an indexing run

A new request, sent outside the transaction like a read: **index EXPORT**.

- Needs `admin = "rw"` in the saved config, checked at the request, like a
  read checks its right.
- Names one export, by name, as it stands in the **saved** config. Changes
  not yet committed are not seen: add the export, commit, then ask to index
  it.
- The daemon starts hashing the export's folder in the background, with the
  same code as `index_at_start` (`warm::index_root` on the export's index
  from `index_for`), and answers at once.
- The answer is one of:
  - **started**, with the run's status (see Part 3);
  - **already running**, with the status of the run in progress. A second
    run is never started for the same folder. Two exports with the same
    path share one index and so one run;
  - an **error**: no such export, or the folder cannot be opened. The session
    stays open.
- A client with only `admin = "ro"` gets "not allowed" as an error answer and
  the session stays open (my call: it has the admin right, so it learns
  nothing it may not know). The warning line in the log is as for a NACK.
- The audit log gets one info line: `admin: mylaptop started indexing export
  photos`. When a run ends, one info line with its counts, as
  `index_at_start` writes today, and one warning line per file that failed
  (with its path: the log is the server owner's).
- A run started this way goes on when the session or the connection ends. It
  also goes on if the export is removed or renamed meanwhile: it hashes a
  folder, and the index stays with the folder.

## Part 3: asking what is being indexed

A new read: **indexing**. Needs `admin = "ro"` or `"rw"`, like the other
reads. It is answered from memory, not from the config file.

The answer lists, for each export path the daemon has indexed or is
indexing since it started, the latest run:

- the export's name when the run started, and the path;
- who started it: a client (its name, as the audit log writes it) or
  `index_at_start`;
- when it started and, once it is over, when it ended (Unix seconds);
- the counts so far: files examined, files hashed, bytes hashed, files that
  failed;
- once over: whether the walk was incomplete, and the error text if the run
  failed as a whole (for example the folder is gone).

Only the latest run per path is kept; a new run replaces the finished one.
So the list has one line per indexed path at most, and a short export that
finishes between two polls still shows how it ended.

Runs started by `index_at_start` when the daemon starts are tracked the same
way, so the web interface sees them too. This changes `start_tasks` in
`daemon.rs` to go through the same run table.

The counts come from the report `warm::index_root` already keeps as it goes
(`WarmReport`): it is shared with the run table while the run is going,
instead of returned only at the end. The paths of failed files stay out of
the answer (anti-goal).

## The CLI

- `irsync remote SERVER export set NAME KEY=VALUE...` and
  `irsync remote SERVER export rename OLD NEW`: one change and a COMMIT,
  printed as the local commands print.
- `irsync remote SERVER index NAME`: starts the run, then polls the
  indexing read about once a second until the run ends, and prints what
  local `irsync index --export NAME` prints as its summary line; exit 23
  when files failed or the walk was incomplete, as locally. Unlike the local
  command it does not list the files that failed: it says how many, and that
  their paths are in the server's log (decided by the user: counts only, for
  every client, `rw` included). On "already running" it
  waits for that run instead. Interrupting the command (Ctrl-C) leaves the
  run going on the server and says so. (My call: waiting by default matches
  the local command.)
- `irsync remote SERVER index` with no name: prints the indexing read, one
  line per path.
- `irsync remote --help` lists the new commands.

## Protocol

- Change messages `ExportSet` and `ExportRename` in `Change.op`.
- A new top-level request for starting a run (beside `Change`, `Commit`
  and `Read` in `AdminRequest`), and its answer in `AdminReply`.
- A new read (`Read.what`) for the indexing status, and its answer.
- `PROTOCOL_VERSION` goes from 8 to 9. No backward compatibility: irsync has
  no users yet.

## Tests

At least:

- `export set` and `export rename` remotely: the file changes as the local
  command would change it, ACK lines match; a transaction where a later
  change fails leaves both undone.
- Start a run on an export; poll until it ends; the counts match the files.
- A second start while the first runs answers "already running" and does not
  hash twice.
- `admin = "ro"` cannot start a run, can read the status.
- Start on an export that is not committed yet, and on a name that does not
  exist: error, session still usable.
- An `index_at_start` run shows in the status.
- The status never carries file paths, for `ro` or `rw`.
- The CLI's `remote index NAME` waits and exits 23 on a failed file.

## The book

- `book/src/how-to/remote-admin.md`: the new commands, and that indexing
  runs in the background and can be watched.
- `book/src/how-to/seed-the-index.md`: indexing a server's export remotely,
  and that the files that failed are named in the server's log only.
- `book/src/reference/command-line.md` and `management.md`: the new
  commands.
- `book/src/reference/protocol.md`: the new messages and version 9.

## Left to the builder

- Message names and field layout in `proto/irsync/v1/admin.proto`.
- Where the run table lives (on `Shared` in `daemon.rs` is natural) and how
  the report is shared while a run goes on.
- Whether to cap how many runs hash at once across exports; each run already
  uses `HASH_WORKERS`.
- How `index_for` opening a fresh index with `index_at_start` is kept
  from starting a second run beside a requested one.
- How the CLI shares printing with the local `index` and `export` commands.
