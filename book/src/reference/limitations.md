# Known limitations

- **Idle connections are dropped.** The daemon drops an authenticated
  connection that has had no traffic and no local progress, such as hashing or
  writing, for `idle_timeout`, 30 minutes by default. The client reconnects
  and resumes. See [Configuration](configuration.md#daemon).
- **Only file-based Ed25519 keys.** Hardware keys and ssh-agent keys cannot be
  used. Of the keys under `~/.ssh`, only `id_ed25519` is tried, unless
  `identity` in the config file or `--identity` names another. See
  [Configuration](configuration.md#identity-and-state).
- **Relays are slow.** The throughput target, filling a 1 Gbit/s link, applies
  to direct connections. Relays are a fallback and are much slower. When one
  end sits behind a symmetric NAT and the other behind a symmetric or
  port-restricted NAT, hole punching fails and the connection stays on the
  relay; then run a self-hosted relay and point `relay` in
  [`[client]`](configuration.md#client) and
  [`[daemon]`](configuration.md#daemon) at it. See
  [Connections](../explanation/connections.md#relays).
- **No compression.** `irsync` has no counterpart to rsync's `-z`; data
  crosses the link as it is stored.
- **Linux and macOS only**, on x86_64 and aarch64, as client and as daemon.
- **No extended attributes or ACLs.** `irsync` copies neither; see the next
  section.

## rsync options irsync does not have

`irsync` rejects these with exit code 2. The list is not every rsync option,
only those an rsync user is likely to reach for.

| rsync option | In `irsync` |
| --- | --- |
| `-X`, `--xattrs` | Extended attributes are not copied. |
| `-A`, `--acls` | ACLs are not copied. |
| `-x`, `--one-file-system` | The walk does not stop at filesystem boundaries. |
| `-R`, `--relative` | Paths are relative to the source operand; a trailing `/` copies a directory's contents. |
| `--files-from` | Select files with `--include` and `--exclude`. |
| `--link-dest`, `--compare-dest` | No hard-link or comparison directories. Chunks of any file the receiver's index knows are reused. |
| `--backup` | Replaced and deleted files are not kept. |
| `-e`, `--rsh` | The transport is always iroh QUIC; there is no remote shell. |
| `-W`, `--whole-file` | No switch to turn the delta off. |
| `-z`, `--compress` | No compression. |
| `-L`, `--copy-links` | Symlinks are copied as symlinks with `-l`, or skipped. |
| `--partial`, `-P` | Partial files are kept and resumed without a flag; `--partial-dir` moves them. |
| `--size-only`, `--ignore-existing`, `--existing`, `--remove-source-files`, `--chmod` | Not available. |
