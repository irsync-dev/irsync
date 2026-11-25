# Clean up state

An interrupted transfer leaves state behind on purpose, so that the next run
can resume: partial files beside their destinations (or under
`--partial-dir`), and a dormant session in the index under `state_dir`.
Garbage collection removes what nobody is coming back for. It also forgets the
index entries of files that are gone or were replaced.

Most of the time you need not do anything: the client and the daemon collect
on their own. Run `irsync gc` when you want the space back now, or after you
stopped syncing a directory for good.

## What runs on its own

- **The client**, at the start of each sync that is not a dry run, collects
  under that sync's local root. It walks the root at most once a day, or
  sooner once a partial-file record has gone an hour without being touched.
  It removes partial files and sessions older than 7 days.
- **The daemon** checks each export once an hour, starting a minute after the
  export's index is opened. It walks an export at most once a day, unless
  partial-file records have become collectable since the last walk. It removes
  partial files and sessions older than `dormant_session_timeout`, 7 days by
  default (see [Configuration](../reference/configuration.md#daemon)). Each
  walk logs a summary at `info`. A file it cannot remove is logged and
  reported, and the walk carries on.
- **The daemon** also drops a client key's oldest dormant sessions as soon as
  the key has more than `max_dormant_sessions_per_key` of them in an export,
  25 by default. Their partial files stay until the hourly check removes them.

A partial file is kept as long as a stored session still plans to write it.
An export that holds another export (such as a whole home exported
automatically) leaves that inner export's tree to its own collection; `irsync
gc` does the same for indexes whose roots lie inside one another.

## Collect by hand

1. See what would go:

   ```sh
   irsync gc -n
   ```

   It lists every partial file it would remove, then a summary line:
   `N part file(s), N session(s), N index entries would be removed`.

2. Remove it:

   ```sh
   irsync gc
   ```

`irsync gc` visits every root that has an index under `state_dir`: each local
directory this account has synced or indexed, and the exports of a daemon that
runs in this account with the same config. For each root it removes partial
files and dormant sessions older than 7 days, and the index entries of regular
files that are gone or were replaced by another file. It also removes the
index files an older irsync left behind, unless a process still has one
open. Use `--config PATH` when
the `state_dir` you mean is set in another config file.

An entry under a directory that could not be read is kept, since the file may
only be unreadable. A walk that finds no file at all removes no entries, since
the root may be a mount point whose file system is not mounted.

## Uninstall irsync

If you installed with the install script, run it again with `--uninstall`.
Until v0.1.0 is out, the channel flag `--beta` has to come first:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://irsync.org/install.sh | sh -s -- --beta --uninstall
```

It removes these files, and says so for each one it finds:

- `~/.local/bin/irsync` and `~/.local/bin/fileset`
- `~/.local/share/man/man1/irsync.1` and `~/.local/share/man/man1/fileset.1`
- `~/.local/share/man/man5/irsync.toml.5`

It leaves the directories, your config (`~/.config/irsync`) and your state
(`~/.local/state/irsync` by default, or wherever `state_dir` points).
Delete those by hand when you want them gone. If you installed with
Homebrew, apt or dnf, remove irsync with that tool instead.

## See also

- [Seed the index](seed-the-index.md)
- [Command line](../reference/command-line.md#irsync-gc): the flags of
  `irsync gc`.
