# filegen — design

Date: 2026-10-20
Status: draft, awaiting approval

## Intent

`filegen` is a standalone command-line tool that simulates the use of a
POSIX filesystem. Given a root directory and a step count, it walks a Markov
chain whose states are filesystem operations, applies each chosen operation
somewhere in the tree, and stops after the requested number of steps. Every
decision it makes, including the bytes it writes into files, is derived from
a seeded random number generator, so a run is reproducible from its seed.
At the end of a run it writes the exact generator state to a file, and a
later invocation can resume from that file as if it had never stopped.

It exists to validate and harden irsync. The intended loop, driven by a
separate tool that is not part of this design, is:

1. `filegen run` mutates a source tree, from a fresh seed or a saved state.
2. `fileset update` freezes the new state of the source tree in its manifest.
3. `irsync` carries the tree, manifest included, to another host.
4. `fileset check` on the other host confirms every change made it.
5. Repeat.

filegen does none of steps 2 to 4. It only mutates, logs and records.

A second use is performance testing: because every operation is timed and
logged, a run is also a trace of how the filesystem under test behaves, and
later versions can replay specific workloads.

The name goes with `fileset`. Like fileset, the tool is not limited to
regular files; it creates and modifies every kind of filesystem entity a
POSIX tree can hold.

## Goals

- Simulate a generic, plausible workload: a tree that grows, churns, gets
  reorganised and shrinks, the way a tree under real use does. Not a
  uniform spray of operations.
- Cover every kind of object irsync transfers and fileset verifies:
  regular files (sparse ones included), directories, symlinks, FIFOs, hard
  links, permission bits, modification times. Devices, ownership and
  extended attributes are covered but off by default (see below).
- Operate at any depth. Subdirectories receive the same operations as the
  root, recursively.
- Be fully reproducible: the same seed, step count and starting tree
  produce byte-identical results on the same platform.
- Be resumable: a saved state continues the exact sequence the run would
  have produced had it kept going.
- Leave evidence: a human-readable state dump and a human-readable
  operation log with timings, outside the tree, so a failed check on the far
  side can be traced to the operations that preceded it.
- Make every tunable (weights, sizes, caps, name styles, enabled operation
  kinds) configurable, with defaults that need no configuration.
- Work on Linux and macOS, and pass the workspace CI (fmt, clippy with
  warnings denied, tests) on both.
- Share no code with irsync or fileset. Same repository, separate crate.

## Non-goals

- Checking anything. filegen never verifies the tree, never compares it to
  anything, and never reads the fileset manifest.
- Driving the stress loop. A later tool runs filegen, fileset and irsync in
  sequence.
- Specific or recorded workloads (a compiler build, a mail spool, a photo
  import). The configuration must make them expressible later; v1 ships one
  generic workload.
- Concurrency inside a run. One operation at a time, on one thread, in
  step order. Parallel writers would make the log ambiguous and the run
  non-reproducible.
- Windows.
- Mount points: filegen never crosses into another filesystem inside the
  tree. It records entries it finds there as off limits and leaves them
  alone.

## Anti-goals

Things that would make this a failure even if it technically worked:

- **Touching anything outside the root.** Every path filegen writes to is
  under the root, checked after symlink resolution of the parent. Absolute
  symlink targets may point outside the root, but filegen only ever creates
  the link, never writes through it.
- **Touching the fileset manifest.** Any entry whose name starts with
  `.fileset` is invisible to filegen: never created, renamed, linked to,
  deleted, descended into, or chosen as a target or destination. This holds
  at every depth, not just the root, because `fileset init` refuses nested
  manifests.
- **Non-determinism creeping in.** Directory listings are sorted bytewise
  before any choice is made from them. Wall-clock time is logged but never
  feeds a decision. Nothing is chosen from a HashMap iteration order.
- **Silent failure.** An operation that fails on disk aborts the run. The
  state and log are still written, with the failure recorded, and the exit
  code is non-zero.
- **Unbounded growth.** The configured total size cap is a hard limit on
  bytes filegen will allocate, not a hint.

## Constraints and decisions already made

### Crate and build

- `crates/filegen`, a workspace member with one binary, `filegen`, and a
  library the binary and tests use. Same shape as `crates/fileset`.
- Rust, stable toolchain per `rust-toolchain.toml`.
- Dependencies come from `[workspace.dependencies]` where one exists: clap
  (derive), serde, anyhow, thiserror, nix, jiff, tempfile for tests. The
  builder adds a YAML serializer and a seedable, serde-serializable PRNG
  (designer's suggestion: `rand` with `rand_chacha`'s ChaCha8, whose state
  serializes and which is fast enough to fill files at disk speed). New
  workspace dependencies go in the root `Cargo.toml` like the others.
- No dependency on the `irsync`, `irsync-proto` or `fileset` crates.

### Command line

```
filegen run [OPTIONS] <PATH>
```

- `<PATH>`: root of the tree. Must exist and be a directory. It may be
  empty or already populated; filegen takes the tree as it finds it.
- `--steps <N>`: operations to perform in this invocation. Required.
- `--seed <S>`: start a fresh run from this 64-bit seed. Exactly one of
  `--seed` and `--resume` is required.
- `--resume <STATEFILE>`: continue from a state file written by a previous
  run. The seed, step counter and generator state come from the file.
- `--config <FILE>`: YAML configuration file. Every key is optional.
- `--out-dir <DIR>`: where the state and log files go. Lookup order, first
  that is set wins: this flag, the `out_dir` config key, `$FILEGENTMP`,
  `$TMPDIR`, `/tmp`.
- `--prefix <PREFIX>`: base name of the state and log files instead of the
  default `filegen-<seed>`. Also available as the `prefix` config key.
- `--dry-run`: walk the chain and write the log and state, but perform no
  filesystem operation. Output goes to `<prefix>.dry.log` and
  `<prefix>.dry.state.yaml`, never to the real run's files, and the dry
  state file records `dry_run: true`; `--resume` accepts it only together
  with `--dry-run`. The log marks every line as dry. Useful for previewing
  what a seed will do and for testing the chain itself.
- `--force`: with `--seed`, truncate an existing log and state file for
  this prefix instead of refusing (see "Output files").
- `-q, --quiet`: no progress output on stderr.
- `-v, --verbose`: echo each log line to stderr as it happens.
- Every top-level scalar configuration key is also a flag of the same name
  (`--max-total-size 25G`, `--max-file-size 256M`, `--fsync`). Nested keys
  are reached with a repeatable `--set <dotted.key>=<value>`
  (`--set content.pattern_fraction=0.2`), parsed as YAML. Flags override
  the config file, which overrides built-in defaults.
- Sizes accept plain bytes or a `K`, `M`, `G`, `T` suffix (powers of 1024).

Exit codes: 0 on success, 1 on a failed operation or I/O error, 2 on a
usage or configuration error, 130 when interrupted by SIGINT and 143 by
SIGTERM (see "Interrupts and checkpoints").

A future `filegen replay` verb that re-applies a log is out of scope but
nothing here should preclude it; the log format below is designed so that
it could be parsed.

### Output files

Both files are written in the resolved output directory. With the default
prefix and seed 42:

- `filegen-42.state.yaml`: the state dump.
- `filegen-42.log`: the operation log, appended to across resumed runs so
  one seed's history reads as one file.

Neither file is ever placed inside the tree. If the resolved output
directory lies inside `<PATH>`, filegen refuses to start (exit 2).

A fresh run (`--seed`) that finds either file already present for its
prefix refuses to start (exit 2) and names the three ways out: `--resume`
to continue that history, `--prefix` to start a separate one, or `--force`
to truncate both files and start over. This keeps two unrelated runs with
the same seed from interleaving in one log or silently replacing each
other's state. `--resume` requires the state file and appends to the log.

The paths of both files are printed to stderr at the start of a run unless
`--quiet` is given.

### State dump

A YAML document, written at the end of every run and on abort. Written to a
temporary file in the same directory and renamed into place, so a crash
mid-write never leaves a truncated state file. Contents:

- `format_version`: integer, starts at 1.
- `seed`: the seed this run started from.
- `steps_done`: total steps completed over all runs with this seed, so far.
- `rng`: the serialized generator state, exactly as the PRNG crate
  serializes it.
- `chain_state`: the current Markov state (the last operation performed)
  and the locality anchor (the directory it touched), since the next
  transition and the next target draw depend on both.
- `dry_run`: whether this state was produced by a dry run.
- `config`: the effective configuration this run used, fully expanded, so
  the file can serve as `--config` for a reproduction.
- `tree`: `entries` (count) and `total_bytes` (sum of regular file sizes,
  counted by apparent size) as filegen last knew them. Advisory only.
- `last_run`: start and end timestamps, steps performed, outcome (`ok` or
  the failure message), and the per-operation performance summary (see
  log).

On `--resume`, `format_version` must match, and the `config` in the file is
applied before any `--config` file or flags given to the resuming
invocation. Resuming with a different configuration is allowed; it is the
user's business and the new effective config is written back.

### Interrupts and checkpoints

SIGINT and SIGTERM are handled: the step in flight finishes, the state
file and the log footer are written, and the process exits with 130 or
143. A second SIGINT during that shutdown exits immediately. A run of
hours is therefore resumable from a Ctrl-C, not just reproducible.

Independently of signals, the state file is rewritten (atomically, as
above) every `checkpoint_steps` steps, default 1000, so a kill that no
handler can catch loses at most that many steps of generator position.
The tree itself is always ahead of or equal to the checkpoint, never
behind it; a resume from a checkpoint older than the tree is the "altered
tree" case described next, and remains deterministic from that point.

### Resume semantics

On resume filegen does not trust the state file's picture of the tree. It
walks the tree afresh, sorted, and rebuilds its model from what is on disk.
Continuing from the saved generator state then produces the same sequence
of operations the uninterrupted run would have produced, provided the tree
is in the state the previous run left it. If the tree was altered in
between, the run continues deterministically from the altered tree; it is
still reproducible from the log, just not identical to the uninterrupted
run. The `tree` block in the state file lets a reader notice this happened.

### Model of the tree

filegen keeps an in-memory model of the tree: every directory with its
sorted children, each child's kind and, for regular files, its apparent
size and inode (for hard-link bookkeeping). The model is built by a sorted
walk at start-up and updated by each operation. It is the only source for
"pick an existing X" decisions, which is what makes those decisions
deterministic.

The walk and the model skip, and never offer as targets:

- entries named `.fileset*` at any depth;
- entries on a different device than the root (mount points);
- entries whose kind filegen does not handle (sockets, "other").

Memory for the model grows with entry count, not bytes. A tree of a few
million entries is in scope; the model must stay compact (no per-entry
`PathBuf` of the full path; store names and parent links).

### The Markov chain

States are the operation kinds listed below. The chain carries one state:
the kind of the previous operation. A transition table gives, for each
previous kind, a weight for each next kind. One step is: draw the next
kind from the previous kind's row; draw a target (and destination, size,
content and so on as the kind needs) from the model; apply; log; update the
model.

The built-in table expresses a generic workload. Guidance for the builder
when filling it in:

- After `mkdir`, the next operation is very likely to land inside the new
  directory (create file, mkdir again). Fresh directories get populated.
- After `create_file`, writes to that file (append, overwrite) and more
  creates in the same directory are likely. Files are worked on after they
  appear.
- Deletes, renames and symlink work run at a lower background rate from
  every state.
- Rare kinds (fifo, hard link, chmod with special bits, far-future mtime)
  get small weights that still make them appear every few hundred steps.
- The table is the `weights` block of the configuration; any cell can be
  overridden. A kind with weight 0 in every row never occurs; this is how
  xattr, chown and device operations are disabled by default and how a
  user turns a kind off.

Directories that contain an off-limits entry (a `.fileset*` name, a mount
point, a socket) anywhere beneath them are never targets for `remove_tree`
or `rename`, so filegen never has to move or delete past something it
must not touch.

Target selection is also biased, not uniform: the directory the previous
operation touched is the most likely place for the next one, its parent
and siblings next, and a random directory anywhere in the tree after that.
This is what produces locality and recursion. The exact scheme is the
builder's, within the rule that it must be driven only by the PRNG and the
sorted model.

If the drawn kind has no valid target (delete a file in an empty tree,
re-point a symlink when none exists), the step draws again from the same
row, at most a bounded number of times, then falls back to `create_file`
in the root. A step always performs exactly one operation.

### Size budget

Two caps, both configurable:

- `max_file_size`, default 256 MiB: no regular file is ever created or
  grown beyond this apparent size.
- `max_total_size`, default 25 GiB: the sum of apparent sizes of all
  regular files in the model never exceeds this.

As the total approaches the cap, the chain's draws are reweighted toward
operations that shrink or remove (truncate, delete, remove subtree) and
away from those that grow, so a long run keeps churning instead of
stalling. The builder chooses the curve; it must be driven only by the PRNG
and the model, and the log must show when it is in effect.

Hard links and sparse holes count by apparent size, matching the `tree`
block and keeping the arithmetic simple. Sparse files may therefore occupy
less on disk than the cap suggests; hard links more than once. Both are
accepted.

File sizes are drawn from a long-tailed distribution with configurable
shape: most files small (bytes to tens of KiB), a steady minority in the
MiB range, and a few up to `max_file_size`. Zero-length files occur.

### Operation kinds

Each kind names what it does, its target rule and what the log records.
"A file" means a regular file in the model; "a dir" a directory; "an entry"
anything in the model. Destinations for new entries get a generated name
that does not collide with an existing child (or, where stated, that
deliberately does).

Enabled by default:

| Kind | What it does |
| --- | --- |
| `create_file` | New regular file in a dir, content from the PRNG, size from the distribution. Written with large buffered writes. |
| `append` | Grow a file by a drawn amount, within `max_file_size`. |
| `truncate` | Shorten a file to a drawn length, including to zero. |
| `overwrite_range` | Rewrite a drawn byte range in place, length and offset within the current size. |
| `rewrite` | Replace a file's content entirely with a new drawn size. |
| `punch_hole` | Make a drawn region sparse. On Linux `fallocate(PUNCH_HOLE)`; on macOS `F_PUNCHHOLE` where supported, else write zeros and log that the hole was emulated. May also extend a file by seeking past its end and writing, creating a trailing hole. |
| `delete_file` | Unlink an entry that is neither a directory nor a symlink: a regular file, a FIFO, or a device node. |
| `mkdir` | New dir in a dir. |
| `rmdir` | Remove an empty dir. |
| `remove_tree` | Remove a dir and everything under it, bounded by a configurable maximum entry count so one step cannot wipe the tree. |
| `rename` | Rename an entry within its dir, or move it to another dir. Both files and dirs; dirs never into their own subtree. |
| `rename_over` | Rename a file over an existing file (atomic replace). |
| `symlink` | New symlink in a dir. Target is drawn from: an existing file, an existing dir, a dangling path, as a relative path or an absolute path, each with configurable probability. |
| `repoint_symlink` | Replace an existing symlink's target with a newly drawn one. |
| `delete_symlink` | Unlink a symlink. |
| `hardlink` | New hard link to a file, in a drawn dir. The model records the shared inode. |
| `mkfifo` | New FIFO in a dir. Never opened. |
| `chmod` | Set a drawn mode on an entry that is not a symlink. Draws from a configured set of common modes plus, at low weight, suid, sgid and sticky bits. Never removes the owner's read and write bits on a file, or read, write and search bits on a dir, because filegen must be able to keep working. |
| `set_mtime` | Set an entry's mtime to a drawn time: recent past, distant past (epoch to decades ago), or future, with sub-second nanoseconds. Applied with `utimensat` without following symlinks, so symlinks get it too. |

Disabled by default (weight 0; enable through the config):

| Kind | What it does | Why off |
| --- | --- | --- |
| `set_xattr` | Set, replace or remove a user.* xattr with drawn name and value. | irsync does not transfer xattrs; only `fileset check --paranoid` would see them. |
| `chown` | Change uid or gid to a drawn value from a configured list. | Needs root. |
| `mknod` | New character or block device with drawn major/minor. | Needs root. |

Content written by `create_file`, `append`, `overwrite_range` and `rewrite`
is drawn from a per-operation sub-generator seeded by one draw of the main
PRNG, so it is incompressible and unique, a replay from the same seed
produces the same bytes, and the main generator advances by the same
amount whether or not the bytes are actually written (which keeps
`--dry-run` cheap and its state file equal to a real run's). A configurable fraction
of content may instead be drawn from a small set of repeating patterns
(zeros, a repeated byte, a repeated short block), so rename detection,
chunking and sparse handling in irsync see realistic duplicates. Default
fraction is small but non-zero.

### Names

Generated names draw from configurable classes with configurable weights.
Default classes and the designer's intended defaults:

- plain ASCII letters, digits, `-`, `_`, `.`, with an extension from a
  short list (most names);
- names with spaces;
- dotfiles;
- Unicode: Latin with diacritics, CJK, emoji; in NFC form (macOS APFS
  stores names as given but is normalisation-insensitive on lookup, so the
  builder must make sure the model's name is what `readdir` returns on both
  platforms, and the tests cover it);
- long names, up to 255 bytes (NAME_MAX on both platforms);
- deliberately awkward: leading `-`, trailing space or dot, names that are
  valid but ugly for shells.

Names never contain `/` or NUL, are never `.` or `..`, and never start
with `.fileset`. Sibling names are unique under case folding and Unicode
normalisation, not just bytewise, because the default macOS APFS is
case-insensitive and normalisation-insensitive; a draw that collides under
either rule is redrawn on every platform, so the sequence stays the same
everywhere.

Path depth is bounded by a configurable `max_depth` (default 32), and
the byte length of any path relative to the root by `max_path_len`
(default 900 bytes). Both are checked for `mkdir`, for every new name, and
for `rename` of a directory (the longest path in the moved subtree must
still fit); a draw that would exceed either is redrawn. The relative
bound keeps the draw sequence independent of where the root lives. At
start-up filegen checks that the canonical root length plus
`max_path_len` fits the platform's PATH_MAX (1024 on macOS, 4096 on
Linux) and exits 2 with a hint to lower `max_path_len` if not.

### Operation log

Plain text, UTF-8, one line per step, appended to across resumes. The
builder picks the exact layout within these rules:

- Every line carries: step number (global, continuing across resumes),
  wall-clock timestamp (RFC 3339 with microseconds, from jiff), operation
  kind, the paths involved relative to the root, the bytes read or written
  if any, and the elapsed time of the filesystem work for that step in
  microseconds.
- Paths are quoted and escaped so a name with a space, newline, quote or
  invalid UTF-8 round-trips unambiguously. The builder may reuse the
  escaping scheme fileset uses in its output, by re-implementing it, not by
  depending on the crate.
- Kind-specific details are appended as `key=value` pairs: new size, mode,
  mtime, symlink target, hole offset and length, whether a hole was
  emulated, whether the size-cap reweighting was in effect.
- A run starts with a header line (seed, start step, effective output paths,
  config file used, dry-run flag) and ends with a footer block: steps
  performed, outcome, elapsed wall time, and a per-kind summary of count,
  bytes, and p50, p99 and max elapsed microseconds. The same summary goes
  into the state file's `last_run`.
- A failed step logs the kind, the paths, and the OS error before the
  footer.

Timing measures the filesystem work only: from just before the first
syscall of the step to just after the last (including `fsync` if the config
asks for one; default is no fsync). Choosing the operation and updating the
model are not included.

### Progress

Unless `--quiet`, stderr shows a single updating line: steps done of
total, bytes written this run, current step rate. No per-step chatter
unless `--verbose`.

### Configuration file

YAML, all keys optional. The effective configuration with every default
filled in is written into the state file, so running once with no config
and reading the state file documents the whole schema. Top-level keys the
builder must provide, names final:

- `out_dir`, `prefix`
- `max_total_size`, `max_file_size`, `max_depth`, `max_path_len`,
  `remove_tree_max_entries`
- `fsync` (bool, default false), `checkpoint_steps` (default 1000)
- `size_distribution` (shape parameters; the builder picks the
  parameterisation and documents it)
- `content` (fraction and set of patterned content)
- `names` (class weights, extension list)
- `symlink` (target class probabilities)
- `chmod` (mode set, special-bit weight)
- `mtime` (class probabilities, time ranges)
- `locality` (target-selection bias parameters)
- `weights` (the transition table, previous kind → next kind → weight)
- `xattr`, `chown`, `mknod` (their parameters, used only when their
  weights are non-zero)

Unknown keys are an error (exit 2), so a typo cannot silently fall back
to a default.

## Crate layout

Designer's suggestion; the builder may reshape it.

- `cli.rs`: clap definitions, size parsing, output-directory resolution.
- `config.rs`: schema, defaults, merge order, validation.
- `model.rs`: the in-memory tree model and the initial walk.
- `chain.rs`: transition table, state, draw-next-kind, size-cap
  reweighting.
- `pick.rs`: target and destination selection with locality bias, name
  generation.
- `ops/`: one module per operation kind, each exposing apply (real) and a
  dry-run path, returning what the log needs.
- `content.rs`: PRNG-driven and patterned content streams.
- `log.rs`: line formatting, escaping, summary statistics.
- `state.rs`: state file read and write, atomic replace.
- `run.rs`: the step loop, abort handling, progress.

## Testing expectations

- Unit tests per module, with the chain and pickers tested against a fixed
  seed for exact expected sequences, so a change in draw order is caught.
- Determinism: two runs with the same seed on fresh temp dirs produce
  identical logs (minus timestamps and durations) and identical trees
  (compared by walking both, with content).
- Resume: a run of N steps and a run of K steps resumed for N−K steps
  produce identical trees and logs.
- Every enabled-by-default kind is exercised in a seeded run of a few
  thousand steps in a temp dir, and the resulting tree is what the log
  says it is.
- Safety: a seeded run never touches a `.fileset.sqlite` planted at the
  root and in a subdirectory, and never creates a path outside the root
  (checked with a canary directory beside the root).
- Caps: with a tiny `max_total_size` and `max_file_size`, a long run never
  exceeds either, verified by walking the tree after every step in the
  test.
- Dry run: produces its own log and state, leaves the tree untouched, and
  its state file matches the real run's generator position step for step.
- Refusals: a fresh seed over existing output files exits 2; `--force`
  truncates them; a dry state file is refused by a real `--resume`.
- Interrupt: a run that receives SIGTERM mid-way exits 143 with a state
  file and footer, and a resume from it produces the same tree as the
  uninterrupted run.
- The CI matrix runs on Linux and macOS; tests that need a hole-punching
  filesystem or root skip themselves with a message rather than fail.
- An integration test that invokes the built binary through the command
  line, as the future driver will.

## Left to the builder

- The exact transition table and locality scheme, within the guidance
  above.
- The parameterisation of the size distribution and the name classes.
- The log line layout and escaping, within the rules above.
- How CLI flags mirror config keys without a hand-maintained duplicate.
- Choice of PRNG and YAML crates (suggestions given).
- Buffer sizes and write strategy for large files, as long as a 256 MiB
  file is produced at something close to disk speed.
- Whether `remove_tree` deletes in the model first or walks the disk.
- Internal error types.

## Open points, not blocking v1

- A `replay` verb that applies a log to a tree, for reproducing a failure
  without the generator.
- Named workload presets (`--workload build-tree`), which are just
  configuration files shipped with the crate.
- A machine-readable trace (JSON lines) alongside the text log, if the
  performance use grows beyond what the summary gives.
- Concurrency as an opt-in, with a deterministic schedule, if irsync's
  behaviour under concurrent modification becomes a target.
