# Introduction to fileset

`fileset` checks that a copy of a directory tree is the same as the original.
You use it for two things:

- **Verify transfers and backups made with other tools,** such as rsync or
  rclone.
- **Check important data sets in storage for bitrot.** Bitrot is data that
  changes on disk without anyone writing to it.

`fileset` is a separate tool. It shares no code with `irsync`, and you do not
need `irsync` to use it. That is on purpose: a check made with the same code
as the copy can repeat the copy's mistakes. This part of the book assumes you
have never used `irsync`.

## The idea

You record a manifest of the tree once. A manifest is a small database that
holds each file's checksum and metadata. Later you check a tree against it.
`fileset` lists every difference. It changes nothing.

```text
fileset init  ~/Photos     record a manifest of the tree
fileset check <copy>       compare a copy, or the same tree later, with it
fileset update ~/Photos    bring the manifest up to date after real changes
```

The manifest is a file named `.fileset.sqlite` at the top of the tree. A
copy tool that copies the whole tree copies the manifest too, so the copy can
check itself.

## Install

```sh
cargo install --git https://github.com/irsync-dev/irsync fileset
```

Install it on every machine where you want to run a check.

## Where to go next

- [Tutorial](tutorial.md): record a manifest, copy with `rsync`, check the
  copy.
- [Verify an rsync or rclone transfer](verify-other-tools.md)
- [Verify an irsync transfer](verify-irsync.md)
- [Check data for bitrot](bitrot.md)
- [Keep a manifest current](keep-current.md)
- [Reference](reference.md): every command, option, output line and exit
  code.
- [How fileset works](how-it-works.md): what a manifest records, and what
  `fileset` cannot catch.
