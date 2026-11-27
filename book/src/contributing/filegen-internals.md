# How filegen works

Part of [Testing](testing.md): how the filegen tool chooses what to do.

This page explains how filegen decides what to do to a tree: the chain
that picks the next operation, how it picks a target, how big things are
and what they are called, and how a run is reproduced and resumed. It also
says where to change filegen when it needs to do more. For how to run it
and every option, see [Generating test trees with filegen](filegen.md).

## Why a Markov chain

Real use of a filesystem comes in patterns. A file is created and then
appended to. A directory is made and then filled. A file is written and
then renamed. Sync bugs tend to hide in exactly these situations.

Independent random operations rarely build them. A chain does. In a
Markov chain, the next operation depends on the one before it. So filegen
can make "create, then append" likely, and still be exactly repeatable
from a seed.

## How it works

### The table

Every operation is an `OpKind` (in [`kind.rs`](../../../crates/filegen/src/kind.rs)). There are
22 of them. The chain ([`chain.rs`](../../../crates/filegen/src/chain.rs)) is a 22 by 22 table
of weights. Each row is the previous kind. Each column is the next kind. To
pick the next operation, filegen takes the row of the operation it just did
and draws a column, with probability in proportion to the weights.

The first step starts from `mkdir`, so a fresh run opens as if a directory
had just been made.

The defaults are in `DEFAULT_TABLE` in [`config.rs`](../../../crates/filegen/src/config.rs). The
full block is in the
[Configuration](filegen.md#configuration) section of the filegen page, and
is not repeated here.

### Switching a kind off

A kind is off when its column is 0 in every row. It can then never be
drawn. `set_xattr`, `chown` and `mknod` are off by default this way,
because they need privileges or filesystem support. Turning one on means
giving its column a weight, as in [Tuning without code](#tuning-without-code).

A row may not be all zeros, and no weight may be negative. filegen refuses
to start with such a table.

A run can also switch kinds off by itself: the ones the system or the machine
cannot do. See [Platforms](#platforms).

### Locality

Real use also stays in one place. filegen remembers the directory the last
operation touched, called the anchor. The next target is chosen near it
([`pick.rs`](../../../crates/filegen/src/pick.rs)):

- with probability `locality.same_dir` (0.6), from below the anchor,
- with probability `locality.parent` (0.2), from below its parent,
- otherwise (0.2, `locality.anywhere`), from anywhere in the tree.

If the chosen scope holds nothing suitable, the search widens. If no
target exists at all for the drawn kind (a `rmdir` in a tree with no
directories, say), filegen draws another kind, up to 16 times. After that
it creates a file in the root. Only if even that fails is the step skipped.

### Size pressure

filegen keeps the tree under `max_total_size`. Below 80% of that cap
nothing changes. Above it, a pressure rises in a straight line from 0 to 1
at the cap. As it rises, the kinds that grow the tree get less likely and
the kinds that shrink it get more likely. Which kinds those are is set by
`grows` and `shrinks` in [`kind.rs`](../../../crates/filegen/src/kind.rs). At full pressure, growing kinds are
never drawn. The log shows the pressure on each step as `pressure=`.

### Sizes, names and content

- **Sizes** ([`sizes.rs`](../../../crates/filegen/src/sizes.rs)). A new file is empty, or falls in a small, medium
  or large band, chosen by weight. Inside the band the size is
  log-uniform, so small files are common and big ones rare. Appends,
  overwritten ranges and truncations use the same idea.
- **Names** ([`names.rs`](../../../crates/filegen/src/names.rs)). Each name has a style: plain, with spaces, a
  dotfile, unicode, long, or awkward. Names are unique in their directory
  even when case is ignored and unicode is normalised, so the tree is safe
  on any platform.
- **Content** ([`content.rs`](../../../crates/filegen/src/content.rs)). Most bytes are random. A fraction
  (`content.pattern_fraction`) uses a pattern instead: zeros, one repeated
  byte, or a repeated block.

### The seed

All choices come from a generator seeded by `--seed`. The bytes of file
content come from a second generator, seeded by one draw from the first.
The functions that draw sizes and names take the same number of draws
whatever they return. So two runs that differ only in what the caps allow
stay in step.

### Resume

The state file records the generator, the last kind and the anchor
([`state.rs`](../../../crates/filegen/src/state.rs)). On `--resume`, filegen does not trust its
picture of the tree. It walks the tree again to rebuild its model
([`model.rs`](../../../crates/filegen/src/model.rs), [`walk.rs`](../../../crates/filegen/src/walk.rs)), then carries on from the saved generator. If the
tree is as the last run left it, the result is the same as a run that
never stopped. If someone changed the tree in between, the run goes on
from the changed tree. It is still repeatable, but no longer identical.

### Platforms

Every call into the operating system sits in one module per system:
[`unix.rs`](../../../crates/filegen/src/platform/unix.rs) and [`windows.rs`](../../../crates/filegen/src/platform/windows.rs), behind
the plain functions of [`platform/mod.rs`](../../../crates/filegen/src/platform/mod.rs). A build has one of them, so nothing
dispatches at run time. No file outside [`crates/filegen/src/platform/`](../../../crates/filegen/src/platform) names `nix`, `libc`,
`xattr`, `std::os::unix` or `std::os::windows`, or a Windows crate: a test
([`platform_rule.rs`](../../../crates/filegen/tests/platform_rule.rs)) fails when one does. Other code asks the
platform's constants and functions instead, for instance `MAX_LINKS`,
`name_ok` or `EDGE_NAMES`. On Unix each of them leaves filegen as it was
before Windows.

What a machine can do is `Caps` ([`caps.rs`](../../../crates/filegen/src/caps.rs)): `symlinks` and `sparse`. A run
finds them once, at the start, with `platform::probe_caps`. On Unix that
tries nothing and returns every capability. On Windows it tries to make a
symlink and to mark a file sparse, in the tree for a real run and in the
output directory for a dry run. `caps::disabled` then names the kinds that
cannot run on this system with these capabilities, and `caps::without`
sets their weights to 0 before the chain is built. One notice names them.
That is why a single config serves both systems, and why the choice of kinds
differs between systems: the seed's run is the same only for the same
platform family and capability set.

The state file records the platform and the capabilities (`format_version`
4), and a resume refuses a state whose values differ from this machine's, in
[`run.rs`](../../../crates/filegen/src/run.rs). A version 3 state is read as a Unix state with every capability
([`state.rs`](../../../crates/filegen/src/state.rs)).

Names are bytes outside the platform module. On Windows they are WTF-8, made
and read by [`wtf8.rs`](../../../crates/filegen/src/wtf8.rs), which is plain safe code that every build compiles and
tests. [`names.rs`](../../../crates/filegen/src/names.rs) draws the edge names (an unpaired surrogate, a reserved
device name) only where `platform::EDGE_NAMES` is set, and takes no draw from
the generator elsewhere, so the names a seed gives on Unix are unchanged.

Ctrl-C is the one thing here that differs in kind. On Unix a signal handler
sets a flag that the loop reads between steps. On Windows the `ctrlc` crate
does the same for console events, but it does not say which event arrived, so
the log names all of them `Ctrl-C`. Closing the console window, logoff and
shutdown are such events too, and Windows ends the process shortly after
the handler returns, so a state save is not guaranteed for those.

The Windows module has two `unsafe` blocks, one reading a reparse point's tag
(`GetFileInformationByHandleEx`) and one for `DeviceIoControl`, which marks a
file sparse and zeroes a range. Miri cannot run either: it cannot make system
calls, and the nightly job runs on Linux. Their tests cover them. `punch_hole`
is crate-private, because the soundness argument of the second block holds
only for files this crate opens itself.

## Tuning without code

Most changes need no code. Use `--config` with a YAML file, or `--set` with
a dotted key, as described on the [filegen page](filegen.md#command).

One weight is one cell of the table: previous kind, then next kind. This
makes a hard link much more likely right after a `create_file`. It is a
valid one-step check:

```sh
filegen run --seed 1 --steps 1 --dry-run --out-dir ~/fg \
    --set weights.create_file.hardlink=10 ~/src-tree
```

[irsync-stress](stress.md) and [irsync-netstress](netstress.md) pass the
same keys through with `--filegen-set`. To raise a whole column you need one
cell per row, so [`matrix.sh`](../../../crates/stress/tools/matrix.sh) has a small shell function, `col`, that
writes them all. `col hardlink 10` expands to
`--filegen-set weights.create_file.hardlink=10`, and the same for the other
21 rows. Each campaign row there is a few of these.

## Extending filegen

filegen can be changed. Two kinds of change are expected:

- to cover a new critical user journey: a sequence of filesystem
  operations that matters to real users,
- to cover a feature that only some filesystems have.

Where the change goes depends on its kind.

**A new sequence of existing operations.** Often no code is needed. The
sequence is a pattern in the table: raise the weight from one kind to the
next. If it must be the default, edit `DEFAULT_TABLE` in [`config.rs`](../../../crates/filegen/src/config.rs). A
test in [`chain.rs`](../../../crates/filegen/src/chain.rs) checks the shape of the default table, so keep it
passing.

**A new operation.** Add it in these places:

1. A variant in `OpKind` in [`kind.rs`](../../../crates/filegen/src/kind.rs). Update `ALL`, `name`, and the
   array sizes that say 22. Decide if it `grows`, `shrinks`, and whether
   it is `default_enabled`.
2. A row and a column in `DEFAULT_TABLE` in [`config.rs`](../../../crates/filegen/src/config.rs). A kind that is off
   by default has a zero column.
3. Its config keys, if it has any, as a new struct in [`config.rs`](../../../crates/filegen/src/config.rs), next to
   `ChmodConfig` or `XattrConfig`.
4. The operation itself, a function in [`file.rs`](../../../crates/filegen/src/ops/file.rs),
   [`tree.rs`](../../../crates/filegen/src/ops/tree.rs), [`link.rs`](../../../crates/filegen/src/ops/link.rs) or
   [`meta.rs`](../../../crates/filegen/src/ops/meta.rs), and a line for it in `dispatch` in [`ops/mod.rs`](../../../crates/filegen/src/ops/mod.rs). It picks a target with
   the functions in [`pick.rs`](../../../crates/filegen/src/pick.rs), changes the disk, and returns an `Applied`
   that says what happened, so the log and the model stay right. It must
   not touch the disk in a dry run, but it must make the same random draws and
   the same model updates as a real run (see the top of [`ops/mod.rs`](../../../crates/filegen/src/ops/mod.rs)).
5. The model. If the operation creates something the model has no node
   kind for, add one to `NodeKind` in [`model.rs`](../../../crates/filegen/src/model.rs), and teach [`walk.rs`](../../../crates/filegen/src/walk.rs) to
   see it.

**A call into the operating system.** Put it in [`crates/filegen/src/platform/`](../../../crates/filegen/src/platform), as a function
that both [`unix.rs`](../../../crates/filegen/src/platform/unix.rs) and [`windows.rs`](../../../crates/filegen/src/platform/windows.rs) provide. Where a system cannot do the
thing, add a capability or list the kind in `caps::disabled`, so the weight
becomes 0 there.

**A filesystem-specific feature.** Make it a new `OpKind`, off by default.
Such an operation should return `NoTarget` when it has nothing to do, as
`set_xattr` does when `xattr.names` is empty and `mknod` does unless run as
root. Filegen then draws another kind.

### The effect must be observable

A new effect only helps if `fileset check` can see it. Otherwise the
stress loop cannot tell whether the sync carried it.

By default, `fileset check` compares type, content, symlink target, device
numbers, mode, mtime and size. Link count, owner, group and extended
attributes are compared only with `--paranoid` or `--require`. Holes in
sparse files are not recorded at all.

So before adding an operation, ask what in that list it changes. If it
changes something fileset does not compare by default, either fileset
needs a matching change, or the stress run must ask for the stronger
check. If fileset cannot see it at all, the operation will run but prove
nothing.
