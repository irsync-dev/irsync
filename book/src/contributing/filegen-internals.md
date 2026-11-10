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

Every operation is an `OpKind` (in `crates/filegen/src/kind.rs`). There are
22 of them. The chain (`crates/filegen/src/chain.rs`) is a 22 by 22 table
of weights. Each row is the previous kind. Each column is the next kind. To
pick the next operation, filegen takes the row of the operation it just did
and draws a column, with probability in proportion to the weights.

The first step starts from `mkdir`, so a fresh run opens as if a directory
had just been made.

The defaults are in `DEFAULT_TABLE` in `crates/filegen/src/config.rs`. The
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

### Locality

Real use also stays in one place. filegen remembers the directory the last
operation touched, called the anchor. The next target is chosen near it
(`crates/filegen/src/pick.rs`):

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
`grows` and `shrinks` in `kind.rs`. At full pressure, growing kinds are
never drawn. The log shows the pressure on each step as `pressure=`.

### Sizes, names and content

- **Sizes** (`sizes.rs`). A new file is empty, or falls in a small, medium
  or large band, chosen by weight. Inside the band the size is
  log-uniform, so small files are common and big ones rare. Appends,
  overwritten ranges and truncations use the same idea.
- **Names** (`names.rs`). Each name has a style: plain, with spaces, a
  dotfile, unicode, long, or awkward. Names are unique in their directory
  even when case is ignored and unicode is normalised, so the tree is safe
  on any platform.
- **Content** (`content.rs`). Most bytes are random. A fraction
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
(`crates/filegen/src/state.rs`). On `--resume`, filegen does not trust its
picture of the tree. It walks the tree again to rebuild its model
(`model.rs`, `walk.rs`), then carries on from the saved generator. If the
tree is as the last run left it, the result is the same as a run that
never stopped. If someone changed the tree in between, the run goes on
from the changed tree. It is still repeatable, but no longer identical.

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
cell per row, so `crates/stress/tools/matrix.sh` has a small shell function, `col`, that
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
next. If it must be the default, edit `DEFAULT_TABLE` in `config.rs`. A
test in `chain.rs` checks the shape of the default table, so keep it
passing.

**A new operation.** Add it in these places:

1. A variant in `OpKind` in `kind.rs`. Update `ALL`, `name`, and the
   array sizes that say 22. Decide if it `grows`, `shrinks`, and whether
   it is `default_enabled`.
2. A row and a column in `DEFAULT_TABLE` in `config.rs`. A kind that is off
   by default has a zero column.
3. Its config keys, if it has any, as a new struct in `config.rs`, next to
   `ChmodConfig` or `XattrConfig`.
4. The operation itself, a function in a file under
   `crates/filegen/src/ops/` (`file.rs`, `tree.rs`, `link.rs` or `meta.rs`),
   and a line for it in `dispatch` in `ops/mod.rs`. It picks a target with
   the functions in `pick.rs`, changes the disk, and returns an `Applied`
   that says what happened, so the log and the model stay right. It must
   not touch the disk in a dry run, but it must make the same random draws and
   the same model updates as a real run (see the top of `ops/mod.rs`).
5. The model. If the operation creates something the model has no node
   kind for, add one to `NodeKind` in `model.rs`, and teach `walk.rs` to
   see it.

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
