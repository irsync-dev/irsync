# filegen

`filegen` simulates the use of a POSIX filesystem: it walks a Markov chain
of filesystem operations (create, append, truncate, rename, symlink, hard
link, chmod, set mtime, punch a hole, remove a subtree, ...), applying each
somewhere in a directory tree. Every decision, the bytes written included,
comes from a seeded generator, so a run is reproducible from its seed and
can be resumed from its saved state as if it had never stopped. It shares no
code with irsync or fileset. Full design:
[docs/superpowers/specs/2026-10-20-filegen-design.md](../../docs/superpowers/specs/2026-10-20-filegen-design.md).

It exists to harden irsync, in a stress loop driven by a separate tool:
`filegen run` mutates a source tree; `fileset update` records it in the
tree's manifest; `irsync` carries tree and manifest to another host;
`fileset check` there confirms every change arrived; repeat. filegen only
mutates, logs and records. Every step is timed, so a run is also a
performance trace of the filesystem under test.

## Command

```
filegen run [OPTIONS] --steps <N> (--seed <S> | --resume <STATEFILE>) <PATH>
```

`<PATH>` must be an existing directory, empty or populated.

- `--steps <N>`: operations to perform in this invocation (required)
- `--seed <S>`: start a fresh run from this 64-bit seed
- `--resume <STATEFILE>`: continue from a previous run's state file; exactly
  one of `--seed` and `--resume` is required
- `--config <FILE>`: YAML configuration file; every key is optional
- `--out-dir <DIR>`: where the log and state file go (see below)
- `--prefix <PREFIX>`: base name of the output files instead of
  `filegen-<seed>`; must not be empty, `.`, `..`, or contain `/`
- `--set <KEY=VALUE>`: set any configuration key by its dotted path, value
  parsed as YAML (repeatable), e.g. `--set content.pattern_fraction=0.2`
- `--dry-run`: draw and log every step, update the model, but touch nothing
- `--force`: with `--seed`, truncate an existing log and state file for this
  prefix instead of refusing
- `-q, --quiet`: no progress line and no file paths on stderr
- `-v, --verbose`: echo every log line to stderr (hides the progress line)
- `--max-total-size <SIZE>`, `--max-file-size <SIZE>`, `--max-depth <N>`,
  `--max-path-len <N>`, `--remove-tree-max-entries <N>`,
  `--checkpoint-steps <N>`, `--fsync <true|false>`: the top-level
  configuration keys of the same name. `--fsync` takes a value.

Sizes are plain bytes or carry a `K`, `M`, `G` or `T` suffix (powers of
1024). Precedence, lowest first: built-in defaults, the resumed state's
`config` (without `out_dir` and `prefix`, see below), the `--config` file,
`--set`, the dedicated flags. Unknown keys
are an error, `max_file_size` may not exceed `max_total_size`, the
`names.classes` weights may not all be 0, and no `names.extensions` entry
may contain `/` or NUL.

Examples:

```
filegen run --seed 42 --steps 1000 --out-dir /tmp/fg ~/src-tree
filegen run --resume /tmp/fg/filegen-42.state.yaml --steps 1000 ~/src-tree
filegen run --seed 42 --steps 1000 --dry-run --out-dir /tmp/fg ~/src-tree
filegen run --seed 7 --steps 1000 --max-total-size 1G --max-file-size 16M \
    --set content.pattern_fraction=0.2 --out-dir /tmp/fg ~/src-tree
```

## Output files

The output directory is, first that is set: `--out-dir`, the `out_dir`
config key, `$FILEGENTMP`, `$TMPDIR`, `/tmp`. A relative directory is
resolved against the current directory, and the state file records the
absolute directory used. A `--resume` ignores the `out_dir` and `prefix`
saved in its state file: unless `--out-dir`, `--prefix`, `--set` or a
`--config` file given on that invocation says otherwise, it writes to the
directory of the state file under the prefix taken from the state file's
name, whatever the current directory or the environment. So a resumed run
keeps appending to the same history: the log is appended to across
resumes, each invocation adding its own `# run` header. Both paths are
printed to stderr at the start unless `-q`. With the default prefix and
seed 42:

| | real run | `--dry-run` |
| --- | --- | --- |
| log | `filegen-42.log` | `filegen-42.dry.log` |
| state | `filegen-42.state.yaml` | `filegen-42.dry.state.yaml` |

filegen refuses to start (exit 2) when:

- the output directory is, or lies inside, `<PATH>` (after resolving symlinks);
- a `--seed` run finds either output file already present; the message names
  the ways out: `--resume` to continue that history, `--prefix` to start a
  separate one, `--force` to truncate both. With `--force` the files are
  truncated only after the initial walk of the tree succeeded;
- a dry-run state file is resumed without `--dry-run`, or a real one with it;
- the state file's `format_version` is not 2 (version 1 files were written
  before the content generator changed; the same seed no longer produces
  the same bytes, so they are not resumed);
- the canonical root length plus `max_path_len` reaches `PATH_MAX`.

## Log

One line per step, `key=value` pairs in a fixed order:

```
step=<n> ts=<RFC 3339, µs, UTC> op=<kind> path="<rel>" [to="<rel>"] [dry=1] [bytes=<n>] [<detail>=<v> ...] [pressure=<0.xx>] dur_us=<n> [gen_us=<n>]
```

`step` counts across resumes. Paths are relative to the root, quoted and
escaped (`\"`, `\\`, and `\xhh` for control bytes and invalid UTF-8) so any
name round-trips. `bytes` is the data written, when non-zero. Details
include `size`, `content` (`random`, `zeros`, `repeat_byte`,
`repeat_block`), `offset`/`len`, `mode`, `mtime`/`class`, `target`,
`target_kind`, `absolute`, `entries` and `emulated=1` (a hole written as
zeros). `pressure` appears while the total-size reweighting is in effect.
`dur_us` times the filesystem syscalls only (including `fsync` if enabled),
not the choice of operation. An operation that writes drawn content
(`create_file`, `append`, `overwrite_range`, `rewrite`) generates the bytes
in 1 MiB chunks between its writes; that time is taken out of `dur_us` and
reported as `gen_us`, so `dur_us` measures the filesystem whatever the
generator costs. A failed step is logged as
`step=<n> ts=... op=<kind> path="<rel>" [to="<rel>"] error="<os error>"`,
naming the path the failing call acted on (for a create, the new entry;
`to` for a rename or hard link).

From a real run (paths shortened):

```
# run seed=42 start_step=0 steps=1000 log="/tmp/fg/filegen-42.log" state="/tmp/fg/filegen-42.state.yaml" config="-" dry_run=0 started=2026-10-20T14:24:51.569059Z
step=6 ts=2026-10-20T14:24:51.571654Z op=append path="2yg5uow38.jpg/f4rx i3iipl66u7pmc1e.bin" bytes=30165 size=44002 content=random dur_us=1208 gen_us=12
...
# end steps_done=1000 outcome=ok elapsed_ms=7638
# stat op=create_file count=219 bytes=117340131 p50_us=109 p99_us=156217 max_us=156975 gen_total_us=48112
# stat op=append count=183 bytes=74836731 p50_us=27 p99_us=156299 max_us=156307 gen_total_us=30577
```

The `# end` line is followed by one `# stat` line per kind seen in this
invocation; the percentiles are over `dur_us`, and kinds that write drawn
content add `gen_total_us`, the sum of their `gen_us`. `outcome` is `ok`, `"failed: <message>"` or
`"interrupted: SIGINT"` / `"interrupted: SIGTERM"`. A dry run marks each
step line `dry=1` and logs `dur_us=0` (and `gen_us=0` where the kind
reports it).

A step whose drawn kinds all lack a target, and where even the fallback,
`create_file` in the root, finds no name that fits (for example with a tiny
`max_path_len`), does nothing: it logs no step line and does not advance
`steps_done`, but counts toward `--steps`. The `# end` line then ends in
`skipped=<n>`, the outcome stays `ok`, and unless `-q` one warning on stderr
says how many steps were skipped.

## State file

A YAML document, written to a temporary file and renamed into place:

- `format_version`: 2
- `seed`: the seed the history started from
- `steps_done`: steps completed over all runs of this history
- `rng`: the ChaCha8 chain generator state (`seed`, `stream`, `word_pos`);
  file bytes come from Xoshiro256++ seeded by a draw from it, so they need
  no state of their own
- `chain_state`: `last_kind` (the previous operation) and `anchor` (the
  escaped relative path of the directory it touched; `''` is the root)
- `dry_run`: whether a dry run wrote it
- `config`: the effective configuration, fully expanded, with `out_dir` the
  absolute output directory used; usable as `--config`
- `tree`: `entries` and `total_bytes` as filegen last knew them (advisory)
- `last_run`: `started`, `finished`, `steps` (this invocation), `outcome`
  (as in the log, or `running` for a checkpoint), and `summary`, the
  per-kind statistics of the `# stat` lines

## Resume, interrupts and checkpoints

On `--resume` filegen does not trust the state file's picture of the tree:
it walks the tree afresh and rebuilds its model, then continues from the
saved generator state. If the tree is as the previous run left it, the
result is identical to an uninterrupted run (same tree, same step lines).
If the tree was altered in between, the run continues deterministically from
the altered tree, reproducible but no longer identical; compare `tree` in
the state file with the tree to notice.

SIGINT and SIGTERM let the step in flight finish, then write the log footer
and the state file and exit 130 or 143; the state is resumable. A second
SIGINT during that shutdown exits 130 at once without writing. Signals are
handled from the end of the initial walk on.

Every `checkpoint_steps` steps (default 1000) the state file is rewritten
with outcome `running` and the log is flushed, so a kill no handler can
catch loses at most that many steps of generator position. The tree, and
possibly the log, are then ahead of the checkpoint: a resume starts again at
the checkpoint's step (its `# run` line shows `start_step`), so the log can
hold a few step numbers twice, and the tree is in the "altered" case above.

## Exit codes

- `0`: all steps done
- `1`: an operation failed (the failure line, footer and state are written,
  with the generator rewound to before that step), or another I/O error
- `2`: usage, configuration or state-file error, or a refusal above; a
  `--resume` state file that is missing, unreadable or invalid is reported
  as `state file <path>: <error>`
- `130` / `143`: interrupted by SIGINT / SIGTERM (128 + signal number)

## Safety

filegen never writes outside the root: every target comes from its model of
the tree, and the output directory may not lie inside it. Some entries are
off limits: never targets, never descended into, and a directory holding
one is never removed or renamed. They are entries named `.fileset*` at any
depth, entries on another filesystem (mount points), sockets and other
unknown file types, and entries the start-up walk cannot stat. Unless
filegen runs as root, they are also:

- any directory it cannot list;
- any entry not owned by its effective uid;
- any regular file without owner read and write (`mode & 0o600 != 0o600`,
  such as the `0444` files of a git checkout);
- any directory without owner read, write and search
  (`mode & 0o700 != 0o700`).

As root these four do not apply, and a directory that cannot be listed
(because it vanished during the walk) is kept as an empty directory.
filegen's own `chmod` always keeps owner read and write (and search on
directories), so, with a umask that keeps the owner's bits (any usual one),
nothing filegen creates ever becomes off limits. Absolute
symlink targets may point outside the root, but filegen only creates the
link.

## Configuration

The state file always carries the effective configuration with every
default filled in, so the way to see the schema is a one-step dry run (for
example `filegen run --seed 1 --steps 1 --dry-run --out-dir /tmp/x <DIR>`)
and the `config:` block of `/tmp/x/filegen-1.dry.state.yaml`. The defaults
from such a run, with comments added, nested values in YAML flow style, and
`out_dir` put back to its default `null` (a state file records the directory
used); it loads as `--config` and yields the default configuration:

```yaml
out_dir: null
prefix: null
max_total_size: 25G
max_file_size: 256M
max_depth: 32
max_path_len: 900
remove_tree_max_entries: 1000
fsync: false
checkpoint_steps: 1000
# A new file is empty with weight `empty`, else log-uniform within a band:
# small from 1 byte, medium from small.max, large from medium.max to max_file_size.
size_distribution:
  empty: 0.05
  small: {weight: 0.65, max: 64K}
  medium: {weight: 0.25, max: 8M}
  large_weight: 0.05
# This fraction of writes uses one of `patterns` instead of random bytes.
content:
  pattern_fraction: 0.05
  patterns: [zeros, repeat_byte, repeat_block]
  block_size: 4K
# Relative weights of the name styles. Names are unique per directory under
# case folding and NFC normalisation, on every platform.
names:
  classes: {plain: 70.0, spaces: 8.0, dotfile: 5.0, unicode: 8.0, long: 3.0, awkward: 6.0}
  extensions: [txt, bin, jpg, log, dat, md, json, '']
  max_len: 255
# file/dir/dangling weight the target kind; absolute is a probability.
symlink: {file: 0.5, dir: 0.2, dangling: 0.3, absolute: 0.2}
# Modes are quoted octal strings. special_weight is the chance of adding one
# of suid, sgid, sticky. Owner rw (rwx on directories) is always kept.
chmod:
  modes: ['0644', '0600', '0755', '0700', '0664', '0775', '0444', '0555']
  special_weight: 0.05
# Class weights. Times are drawn relative to 2026-10-20T00:00Z, not the clock.
mtime: {recent: 0.6, distant: 0.3, future: 0.1, distant_years: 40, future_years: 5}
# Where a target is drawn from: below the directory the previous step
# touched, below its parent, or anywhere.
locality: {same_dir: 0.6, parent: 0.2, anywhere: 0.2}
# Previous kind -> next kind. A kind that is 0 in every row never occurs.
weights:
  create_file: {create_file: 15, append: 30, truncate: 3, overwrite_range: 20, rewrite: 3, punch_hole: 0.7, delete_file: 2, mkdir: 3, rmdir: 1, remove_tree: 0.5, rename: 3, rename_over: 1, symlink: 2, repoint_symlink: 1, delete_symlink: 1, hardlink: 1, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  append: {create_file: 25, append: 20, truncate: 4, overwrite_range: 15, rewrite: 3, punch_hole: 1, delete_file: 4, mkdir: 5, rmdir: 3, remove_tree: 1, rename: 3, rename_over: 2, symlink: 2, repoint_symlink: 1, delete_symlink: 1, hardlink: 1, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  truncate: {create_file: 20, append: 20, truncate: 2, overwrite_range: 15, rewrite: 5, punch_hole: 1, delete_file: 4, mkdir: 4, rmdir: 2, remove_tree: 1, rename: 3, rename_over: 2, symlink: 2, repoint_symlink: 1, delete_symlink: 1, hardlink: 1, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  overwrite_range: {create_file: 20, append: 20, truncate: 4, overwrite_range: 15, rewrite: 3, punch_hole: 1, delete_file: 4, mkdir: 4, rmdir: 2, remove_tree: 1, rename: 3, rename_over: 2, symlink: 2, repoint_symlink: 1, delete_symlink: 1, hardlink: 1, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  rewrite: {create_file: 20, append: 18, truncate: 4, overwrite_range: 15, rewrite: 3, punch_hole: 1, delete_file: 4, mkdir: 4, rmdir: 2, remove_tree: 1, rename: 3, rename_over: 2, symlink: 2, repoint_symlink: 1, delete_symlink: 1, hardlink: 1, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  punch_hole: {create_file: 20, append: 18, truncate: 4, overwrite_range: 15, rewrite: 3, punch_hole: 1, delete_file: 4, mkdir: 4, rmdir: 2, remove_tree: 1, rename: 3, rename_over: 2, symlink: 2, repoint_symlink: 1, delete_symlink: 1, hardlink: 1, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  delete_file: {create_file: 25, append: 12, truncate: 2, overwrite_range: 10, rewrite: 3, punch_hole: 0.7, delete_file: 6, mkdir: 6, rmdir: 3, remove_tree: 2, rename: 4, rename_over: 2, symlink: 2, repoint_symlink: 1, delete_symlink: 1, hardlink: 0.7, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  mkdir: {create_file: 45, append: 2, truncate: 1, overwrite_range: 2, rewrite: 1, punch_hole: 0.5, delete_file: 1, mkdir: 20, rmdir: 2, remove_tree: 1, rename: 3, rename_over: 1, symlink: 3, repoint_symlink: 1, delete_symlink: 1, hardlink: 0.5, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  rmdir: {create_file: 20, append: 8, truncate: 2, overwrite_range: 6, rewrite: 2, punch_hole: 0.5, delete_file: 4, mkdir: 12, rmdir: 4, remove_tree: 3, rename: 4, rename_over: 2, symlink: 3, repoint_symlink: 1, delete_symlink: 1, hardlink: 0.5, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  remove_tree: {create_file: 20, append: 8, truncate: 2, overwrite_range: 6, rewrite: 2, punch_hole: 0.5, delete_file: 4, mkdir: 12, rmdir: 3, remove_tree: 3, rename: 4, rename_over: 2, symlink: 3, repoint_symlink: 1, delete_symlink: 1, hardlink: 0.5, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  rename: {create_file: 20, append: 15, truncate: 4, overwrite_range: 12, rewrite: 3, punch_hole: 0.7, delete_file: 6, mkdir: 5, rmdir: 2, remove_tree: 1, rename: 3, rename_over: 2, symlink: 2, repoint_symlink: 1, delete_symlink: 1, hardlink: 0.7, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  rename_over: {create_file: 20, append: 15, truncate: 4, overwrite_range: 12, rewrite: 3, punch_hole: 0.7, delete_file: 6, mkdir: 5, rmdir: 2, remove_tree: 1, rename: 3, rename_over: 2, symlink: 2, repoint_symlink: 1, delete_symlink: 1, hardlink: 0.7, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  symlink: {create_file: 15, append: 10, truncate: 2, overwrite_range: 8, rewrite: 2, punch_hole: 0.5, delete_file: 3, mkdir: 5, rmdir: 2, remove_tree: 1, rename: 3, rename_over: 2, symlink: 6, repoint_symlink: 5, delete_symlink: 4, hardlink: 0.5, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  repoint_symlink: {create_file: 15, append: 10, truncate: 2, overwrite_range: 8, rewrite: 2, punch_hole: 0.5, delete_file: 3, mkdir: 5, rmdir: 2, remove_tree: 1, rename: 3, rename_over: 2, symlink: 6, repoint_symlink: 5, delete_symlink: 4, hardlink: 0.5, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  delete_symlink: {create_file: 20, append: 15, truncate: 4, overwrite_range: 12, rewrite: 3, punch_hole: 0.7, delete_file: 6, mkdir: 5, rmdir: 2, remove_tree: 1, rename: 3, rename_over: 2, symlink: 2, repoint_symlink: 1, delete_symlink: 1, hardlink: 0.7, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  hardlink: {create_file: 20, append: 15, truncate: 4, overwrite_range: 12, rewrite: 3, punch_hole: 0.7, delete_file: 6, mkdir: 5, rmdir: 2, remove_tree: 1, rename: 3, rename_over: 2, symlink: 2, repoint_symlink: 1, delete_symlink: 1, hardlink: 0.7, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  mkfifo: {create_file: 20, append: 15, truncate: 4, overwrite_range: 12, rewrite: 3, punch_hole: 0.7, delete_file: 6, mkdir: 5, rmdir: 2, remove_tree: 1, rename: 3, rename_over: 2, symlink: 2, repoint_symlink: 1, delete_symlink: 1, hardlink: 0.7, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  chmod: {create_file: 20, append: 15, truncate: 4, overwrite_range: 12, rewrite: 3, punch_hole: 0.7, delete_file: 6, mkdir: 5, rmdir: 2, remove_tree: 1, rename: 3, rename_over: 2, symlink: 2, repoint_symlink: 1, delete_symlink: 1, hardlink: 0.7, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  set_mtime: {create_file: 20, append: 15, truncate: 4, overwrite_range: 12, rewrite: 3, punch_hole: 0.7, delete_file: 6, mkdir: 5, rmdir: 2, remove_tree: 1, rename: 3, rename_over: 2, symlink: 2, repoint_symlink: 1, delete_symlink: 1, hardlink: 0.7, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  set_xattr: {create_file: 20, append: 15, truncate: 4, overwrite_range: 12, rewrite: 3, punch_hole: 0.7, delete_file: 6, mkdir: 5, rmdir: 2, remove_tree: 1, rename: 3, rename_over: 2, symlink: 2, repoint_symlink: 1, delete_symlink: 1, hardlink: 0.7, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  chown: {create_file: 20, append: 15, truncate: 4, overwrite_range: 12, rewrite: 3, punch_hole: 0.7, delete_file: 6, mkdir: 5, rmdir: 2, remove_tree: 1, rename: 3, rename_over: 2, symlink: 2, repoint_symlink: 1, delete_symlink: 1, hardlink: 0.7, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
  mknod: {create_file: 20, append: 15, truncate: 4, overwrite_range: 12, rewrite: 3, punch_hole: 0.7, delete_file: 6, mkdir: 5, rmdir: 2, remove_tree: 1, rename: 3, rename_over: 2, symlink: 2, repoint_symlink: 1, delete_symlink: 1, hardlink: 0.7, mkfifo: 0.5, chmod: 3, set_mtime: 3, set_xattr: 0, chown: 0, mknod: 0}
xattr:
  names: [user.filegen.a, user.filegen.b, user.filegen.c, user.filegen.d]
  max_value: '256'
chown: {uids: [], gids: []}
mknod: {char_fraction: 0.5}
```

`set_xattr`, `chown` (which also needs ids in `chown.uids` or `chown.gids`,
and the privilege to give files away) and `mknod` (skipped unless root) are
off by default through zero columns in `weights`. Above 80% of
`max_total_size` a pressure rises linearly from 0 to 1 at the cap, damping
the kinds that grow the tree and boosting those that shrink it; the log
shows it as `pressure=`.
