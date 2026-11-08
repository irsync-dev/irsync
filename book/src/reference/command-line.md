# Command line

```text
irsync [OPTIONS] SRC... DEST
irsync [OPTIONS] HOST:[PATH]
irsync COMMAND ...
```

`irsync --help` prints the same flags with one line each; `irsync COMMAND
--help` does so for a subcommand.

## Operands

Each operand is a local path or `HOST:PATH`. `HOST` is a host alias from the
[configuration](configuration.md#hosts) or an `EndpointId`; `PATH` is resolved
on the daemon's machine, as described in
[Remote addresses](remote-addresses.md).

- A `:` before the first `/` makes an operand remote, as in rsync. To name a
  local file whose name has a colon, write `./name:with:colons`.
- Exactly one side is remote. A push (local to remote) may have several
  sources; each one runs as its own session, one after the other. A pull has
  one source.
- rsync's trailing-slash rule holds on both sides: `Photos/` copies the
  directory's contents, `Photos` copies the directory itself.
- A single remote operand lists instead of copying. `irsync HOST:` prints the
  exports this client may reach, one `NAME PATH ACCESS` line each, where
  `ACCESS` is `rw` or `ro` with the export's `read_only` applied.
  `irsync HOST:PATH` prints the files a pull would fetch and writes nothing; it
  recurses unless `--no-recursive` is given. A listing does not redial: if the
  connection fails, it exits instead of waiting out the reconnect window.

The daemon honours the client's flags in both directions, filters included.

## rsync-compatible flags

These have rsync's spelling and meaning.

| Flag | Meaning |
| --- | --- |
| `-a`, `--archive` | Archive mode, the same as `-rlptgoD`. |
| `-r`, `--recursive` | Recurse into directories (implied by `-a`). Without recursion only the top-level entries are transferred, and directories are skipped. |
| `--no-recursive` | Turn off the recursion that `-a` implies. Also spelled `--no-r`. |
| `-d`, `--dirs` | Without recursion, transfer directories without their contents. |
| `-l`, `--links` | Copy symlinks as symlinks. Without it they are skipped. |
| `-p`, `--perms` | Preserve permissions. |
| `-t`, `--times` | Preserve modification times. |
| `-g`, `--group` | Preserve the group, where the receiving user may set it. |
| `-o`, `--owner` | Preserve the owner, only when the receiving side runs as root. |
| `-D` | The same as `--devices --specials`. |
| `--devices` | Preserve character and block device nodes. |
| `--specials` | Preserve FIFOs. Sockets are never copied. |
| `-H`, `--hard-links` | Files linked together in the source are linked together in the destination. |
| `-S`, `--sparse` | Leave chunks that are all zeros as holes. Without it, only files that are sparse in the source are written sparse. |
| `-v`, `--verbose` | List each file as it is handled and end each session with a summary of time, CPU, memory and bytes. `-vv` adds debug logs, `-vvv` trace logs. |
| `-q`, `--quiet` | Print errors only. Also turns the live progress block off. |
| `-n`, `--dry-run` | Change nothing on either side. With `-v` or `-i`, list what would be done. |
| `-c`, `--checksum` | Compare by content, not by size and time: every file on both sides is read, no cached hash is trusted, and the index is refreshed from what was read. |
| `-u`, `--update` | Skip files that are newer on the destination. |
| `--delete` | Delete destination files that are gone from the source, after the transfer. Nothing is deleted without one of the `--delete` flags. |
| `--delete-before` | Delete before the transfer, once both sides are listed. |
| `--delete-during` | Delete each directory's extraneous entries as the run leaves that directory, so space is freed as the run goes. |
| `--delete-after` | Delete after the transfer; what `--delete` does. |
| `--delete-excluded` | Also delete destination files that the filters exclude. Implies `--delete`. |
| `--max-delete N` | Stop deleting after `N` removals. The run exits with code 25. |
| `--exclude PATTERN` | Exclude paths that match an rsync pattern: `*`, `**`, `?`, `[...]`; a leading `/` anchors the pattern to the root, a trailing `/` matches directories only. |
| `--include PATTERN` | Include paths that match, even if a later `--exclude` would hide them. |
| `--exclude-from FILE` | Read exclude patterns from `FILE`, one per line. Blank lines and lines that start with `#` or `;` are skipped. |
| `--include-from FILE` | Read include patterns from `FILE`, as `--exclude-from` does. |
| `--bwlimit RATE` | Limit the rate. A bare number is KiB/s. `K`, `M`, `G`, `T`, `P` (also `KiB` to `PiB`) are powers of 1024, `KB` to `PB` powers of 1000, `b` is bytes; decimals are allowed (`1.5M`). `0` means no limit. |
| `--numeric-ids` | Apply owner and group ids as sent. By default the sender's user and group names are mapped to the receiver's ids where they exist, as rsync does. |
| `--partial-dir DIR` | Keep the partial files of interrupted transfers under `DIR`, relative to the destination root, instead of beside their files. On a pull `DIR` may be absolute; on a push the daemon keeps them, and only a relative `DIR` inside the destination is allowed. |
| `--inplace` | Write a changed file into the destination itself instead of into a temporary file renamed over it. No second copy is needed on disk, but readers see a torn file during the transfer and a crash leaves a mixture. |
| `--append` | Extend a destination file that is shorter than the source with the source's tail, trusting what is already there. A destination that is not shorter is skipped. |
| `--append-verify` | Like `--append`, but the existing prefix is verified too. |
| `--ignore-errors` | Run the deletion pass even when a listing was cut short by I/O errors. |
| `--stats` | Print a block of transfer statistics at the end. |
| `-i`, `--itemize-changes` | Print rsync's per-file change summary, the itemize codes. |
| `--progress` | Accepted and ignored: the live progress block is always drawn when stdout is a terminal. |
| `--info SPEC` | Accepted and ignored, for example `--info=progress2`. |
| `--timeout SECS` | Drop a connection that moves no data and no control message for `SECS` seconds; the session then reconnects. `0`, the default, means no timeout. |
| `-h`, `--help` | Print the flags. |
| `-V`, `--version` | Print the version and the commit it was built from. |

Filter rules apply in command-line order, and the first rule that matches
wins.

### Deletions

Nothing is deleted that might merely have been unreadable. When a listing on
either side is incomplete, because a directory could not be read or an entry
could not be examined, deletions stop from that point on: `--delete-before`
and `--delete-after` then delete nothing, and `--delete-during` stops at the
directory where the gap was found. `--ignore-errors` deletes anyway. Combine
`--delete` with `-n` first to see what it would remove.

### Output

On a terminal the client draws a live block at the bottom of the screen: the
listing walk on both sides, a transfer bar with rsync's percent semantics, one
bar per file in flight, and the deletion pass. `-q` turns it off. When stdout
is not a terminal, or `TERM` is `dumb`, the block is not drawn and the output
is plain lines.

## irsync flags

These have no rsync counterpart.

| Flag | Meaning |
| --- | --- |
| `--parallel N` | Data streams a push keeps open at once, across all files. Default: `parallel` from the host entry, else from `[client]`, else 32. A pull uses the daemon's 32. See [Files in flight and streams](../explanation/transfer-algorithm.md#files-in-flight-and-streams). |
| `--reconnect DUR` | How long to keep redialling after a dropped connection, as `<N>[s\|m\|h\|d]`. `0` gives up at once, like rsync. Default: `reconnect` from the host entry, else from `[client]`, else 24h. |
| `--relay URL` | Use this relay server instead of the host entry's, `[client]`'s or n0's default. Cannot be combined with `--no-relay` or `--address`. |
| `--no-relay` | Direct connections only: never fall back to a relay. The address lookups stay on. Default: `relay_mode` from the host entry, else from `[client]`. |
| `--no-dht` | Do not look the daemon up in the mainline DHT. |
| `--address ADDR` | Dial the daemon at `ip:port`, `[ipv6]:port` or `host:port` directly; repeatable. Turns the relay, DNS, DHT, mDNS and the port mapper off. The daemon's `EndpointId` is still required and still checked. |
| `--config PATH` | The config file. Default: `~/.config/irsync/irsync.toml`. A `hosts.toml` or `exports.toml` beside it is read too. |
| `--identity FILE` | The private key to connect with. Default: `identity` in the config file, else `~/.config/irsync/id_ed25519` when it exists, else `~/.ssh/id_ed25519` when it exists, else a key generated at `~/.config/irsync/id_ed25519`. A named file is the whole answer: when it is missing, a key is generated there. |
| `--passphrase-file PATH` | Read the passphrase of an encrypted key from the first line of this file. Without it, `IRSYNC_PASSPHRASE` is used, else a prompt when stdin is a terminal. |
| `--no-resume` | Start a fresh plan instead of resuming a dormant session. Partial files are still picked up. |
| `--no-rename-detection` | Do not fill files from other files on the receiving side that hold the same content or parts of it, so a moved or copied file is transferred again. A changed file still reuses its own old version. |
| `--debug-addr ADDR` | Serve metrics and span timings over HTTP at `IP:PORT`; a bare port means `127.0.0.1`. The port stays open after the sync until SIGINT or SIGHUP. |
| `--debug-port-no-wait` | With `--debug-addr`, close the port when the sync ends instead of waiting. |

The connection settings resolve in this order: the flag, then the host
alias's table, then `[client]`, then the built-in default. See
[Configuration](configuration.md#client).

## Subcommands

| Command | What it does |
| --- | --- |
| `irsync daemon` | Run the daemon. See below. |
| `irsync index` | Hash the files under local roots into their index. See [Seed the index](../how-to/seed-the-index.md). |
| `irsync gc` | Remove abandoned partial files, dormant sessions and stale index entries. See [Clean up state](../how-to/clean-up.md). |
| `irsync host`, `export`, `grant`, `id`, `config`, `show` | Manage aliases, exports, grants, identities and settings. See [Management commands](management.md). |

### irsync daemon

```text
irsync daemon [--config PATH] [--passphrase-file PATH] [--listen ADDR]... [-v|-q]
```

Serves the exports in the config file to the peers they are granted to, until
it is interrupted. At start it prints `endpoint id: <EndpointId>` and one
`listening on: <socket>` line per bound socket on stdout. Logs go to stderr at
info level; `-v` gives debug, `-vv` trace, `-q` errors only, and `RUST_LOG`
overrides all of them.

| Flag | Meaning |
| --- | --- |
| `--config PATH` | The config file. A `hosts.toml` or `exports.toml` beside it is read too. |
| `--passphrase-file PATH` | Read the passphrase of an encrypted daemon key from the first line of this file. |
| `--listen ADDR` | Bind this socket (`ip:port`, `[ipv6]:port` or `host:port`; port 0 lets the system pick) instead of the config's `listen` list. Repeatable, at most one IPv4 and one IPv6. |
| `-v`, `--verbose` | Log more; repeat for more still. |
| `-q`, `--quiet` | Log errors only. |

### irsync index

```text
irsync index [-n] [-v|-q] [--config PATH] ROOT...
```

| Flag | Meaning |
| --- | --- |
| `-n`, `--dry-run` | Walk and count what would be hashed; read nothing. |
| `-v`, `--verbose` | Log more. |
| `-q`, `--quiet` | Log errors only and print no summary. |
| `--config PATH` | The config file, for `state_dir`. |

### irsync gc

```text
irsync gc [-n] [-v|-q] [--config PATH]
```

| Flag | Meaning |
| --- | --- |
| `-n`, `--dry-run` | List what would be removed without removing anything. |
| `-v`, `--verbose` | Log more. |
| `-q`, `--quiet` | Log errors only and print no summary. |
| `--config PATH` | The config file, for `state_dir`. |

## Environment

| Variable | Effect |
| --- | --- |
| `IRSYNC_PASSPHRASE` | The passphrase of an encrypted key, when no `--passphrase-file` is given. |
| `RUST_LOG` | The log filter, in place of the one `-v` and `-q` choose. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Export traces and metrics over OpenTelemetry to this endpoint. Overrides `[telemetry]` in the config file. |
| `TERM` | `dumb` turns the live progress block off. |

## Exit codes

A sync uses rsync's codes.

| Code | Meaning |
| --- | --- |
| 0 | Success. |
| 1 | Nothing got through: a configuration, identity or connection error; an operand pair `irsync` rejects (both sides local, both remote, several sources on a pull); or every source failed. |
| 2 | The command line could not be parsed: an unknown flag, a bad value, a missing operand. |
| 23 | Partial transfer: some files could not be transferred, or some sources of a push failed while others synced. |
| 25 | Deletions stopped at `--max-delete`. |

When several apply, 25 wins over 23. A file listing, `irsync HOST:PATH`, is a
dry-run pull and uses the same codes; the export listing, `irsync HOST:`,
exits 0 or 1.

The other commands exit 0 on success and 1 on an error, with these
additions: `irsync index` and `irsync export update` exit 23 when a file could
not be read or a directory could not be listed, and `irsync config get` exits
1 for a key that is not set and has no default.
