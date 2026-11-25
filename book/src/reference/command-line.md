# Command line

```text
irsync sync [OPTIONS] SRC... DEST
irsync list [OPTIONS] HOST[:PATH]
irsync COMMAND ...

irsync [OPTIONS] SRC... DEST        # the same as irsync sync
irsync [OPTIONS] HOST:[PATH]        # the same as irsync list
```

`irsync` copies directory trees from one machine to another, in one
direction, in the mould of rsync. It runs on both ends: the client runs
`irsync`, and the server runs `irsync daemon`, the *daemon*, which serves its
directories. A push copies from the client to the server, a pull the other
way. If you can log in to the server with ssh, `irsync pair` sets the server
up for you (see [irsync pair](#irsync-pair)).

The terms used on this page:

- **iroh** is the network library `irsync` connects through. With it, two
  machines find each other and get through NATs and firewalls without port
  forwarding, dynamic DNS or ssh tunnels.
- An **`EndpointId`** is the public half of a machine's key, printed as 64
  hexadecimal digits. The client and the daemon each have one. A client
  reaches a daemon by its `EndpointId`, not by an address. A server entry in the
  config file gives a daemon's `EndpointId` a name, such as `myserver`.
- A **relay** is a server both sides can reach. A connection starts through a
  relay and moves to a direct path when it can. The relay passes on encrypted
  packets that it cannot read. n0 runs public relays, which are the default.
- An **export** is a directory the daemon shares under a name.
- A **grant** lets a client use an export, read-write or read-only. The daemon
  refuses every client that no grant names.
- The **`all_paths` right** lets a client reach any path the daemon's account
  can, read-write or read-only. The daemon then makes an export for each new
  path the client uses. `all_exports` reaches every export you made by hand.
  Both are set with `irsync client set`.

The last two lines of the synopsis above are the rsync-compatible shorthand:
a command written for rsync works with `irsync` in its place. The named forms
`irsync sync` and `irsync list` do the same and cannot be mistaken for a
command that a later irsync adds, which makes them the safer choice in
scripts.

`irsync --help` lists irsync's flags, one line each; `irsync COMMAND --help`
lists a subcommand's.

## Examples

Set up a server you can log in to with `ssh myserver`, then push the contents
of `~/Photos`:

```sh
irsync pair myserver
irsync -av ~/Photos/ myserver:/srv/photos/
```

`pair` is the only step that uses ssh. Later runs of the push go over iroh and
send only what changed.

Pull the copy back into a new directory:

```sh
irsync -av myserver:/srv/photos/ ~/Photos-copy/
```

List the exports this client may reach, then the files a pull would fetch:

```sh
irsync myserver:
irsync myserver:/srv/photos/
```

`irsync list myserver` and `irsync list myserver:/srv/photos/` do the same.

Push again and delete on the server what is gone from the source. Run it
with `-n` first to see what would change:

```sh
irsync -avn --delete ~/Photos/ myserver:/srv/photos/
irsync -av --delete ~/Photos/ myserver:/srv/photos/
```

## Operands

Each operand is a local path or `HOST:PATH`. `HOST` is a server name from the
[configuration](configuration.md#servers) or an `EndpointId`; `PATH` is resolved
on the daemon's machine, as described in
[Remote addresses](remote-addresses.md). A `HOST` that is neither is an error that
names `irsync pair`; see [irsync pair](#irsync-pair).

- A `:` before the first `/` makes an operand remote, as in rsync. To name a
  local file whose name has a colon, write `./name:with:colons`.
- Exactly one side is remote. A push (local to remote) may have several
  sources; each one runs as its own session, one after the other. A pull has
  one source.
- rsync's trailing-slash rule holds on both sides: `Photos/` copies the
  directory's contents, `Photos` copies the directory itself.
- A source may be one file, pushed or pulled. It is placed as rsync would
  place it: into `T` if that is a directory, as `T` if it does not exist, over
  `T` if that is a file. See
  [Copying one file](remote-addresses.md#copying-one-file).
- A single remote operand lists instead of copying, as `irsync list` does;
  there the colon may be left out (`irsync list HOST`). `irsync HOST:` prints the
  exports this client may reach, one `NAME PATH ACCESS` line each, where
  `ACCESS` is `rw` or `ro` with the export's `read_only` applied.
  `irsync HOST:PATH` prints the files a pull would fetch (just that file, if
  `PATH` is a file) and writes nothing; it
  recurses unless `--no-recursive` is given. A listing does not redial: if the
  connection fails, it exits instead of waiting out the reconnect window.

The daemon honours the client's flags in both directions, filters included.

### irsync pair

```text
irsync pair [OPTIONS] HOST
```

`pair` sets a server up over ssh. It is the only command that uses ssh: a
sync, push, pull or listing never does. `HOST` is an ssh destination, such as
`myserver` or `me@myserver`, or a saved server name. `pair` then:

- installs `irsync`, `fileset` and the manual pages on the server (the same
  files as `install.sh`, in `~/.local/bin`) when the server has no matching
  build;
- gives this client `all_paths = "rw"`, access to any path, and `admin = "rw"`, the right to manage the server remotely;
- starts the daemon and makes it a user service: a systemd service on Linux, a
  launchd agent on macOS;
- saves `HOST` as a server with `ssh = "HOST"`.

It prints what it did and ends with `paired with myserver; syncs go over
iroh, not ssh`. The account you log in to must not be root.

A release build downloads the release files on this machine, for every server
platform. If that fails and the server has the same OS and CPU, it installs
this machine's own files instead. The server needs no internet access.

On Linux the service is `~/.config/systemd/user/irsync.service`, and `pair`
turns on linger so that it runs when you are logged out. On macOS it is
`~/Library/LaunchAgents/org.irsync.daemon.plist`, which starts after a reboot
only once someone logs in on the Mac. A service file that irsync did not write
is left alone. With no service manager, a foreign service file or
`--no-service`, the daemon runs in the background until the server reboots and
`pair` prints the steps to make a service yourself. See
[Run the daemon](../how-to/run-the-daemon.md#run-the-daemon-as-a-user-service).

For a saved server, `pair` checks it again: it updates irsync, starts the
daemon if it has stopped and updates the server's `endpoint_id` if it
changed. It never adds back `all_paths` removed on the server. A sync
to a server `pair` saved dials once and waits at most 15 seconds for the
daemon to answer. It retries only when you give `--dial-retries N` on the
command line: then it tries N more times first. When the daemon does not
answer, the sync stops and names `irsync pair` as the way to start it again.
See [Your first sync](../tutorial/first-sync.md#the-quick-way-over-ssh).

| Flag | Meaning |
| --- | --- |
| `--rsh CMD` | The remote shell, split on spaces like rsync's `-e`, for example `--rsh "ssh -p 2222"`. Default: `IRSYNC_RSH`, else `ssh`. |
| `--no-install` | Copy nothing to the server: use the irsync already at `~/.local/bin/irsync` there, and stop if it is missing or another version. Default: `ssh_install` in the config file, else install. |
| `--no-dht` | Turn the server's DHT publishing off (`[daemon.discovery] dht = false`); a running daemon re-reads its settings. Without it the server's setting is left alone, and new servers publish. |
| `--no-service` | Do not make the daemon a user service: start it in the background, until the server reboots. For a daemon you run your own way. |
| `--identity FILE` | The private key to pair with. Default as for a sync. |

## rsync-compatible flags

These have rsync's spelling and meaning. Flags with rsync's spelling and a
different meaning have a section of their own:
[rsync spelling, different meaning](#rsync-spelling-different-meaning).

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
| `-v`, `--verbose` | List each file transferred, and end with rsync's `sent … received …` and `total size is …` lines. `-vv` adds a summary of time, CPU, memory and bytes at the end of each session, and info logs; `-vvv` debug logs, `-vvvv` trace logs, `-vvvvv` also every protocol message (see [Trace and capture the protocol](../contributing/protocol-debugging.md#the-text-trace)). |
| `-q`, `--quiet` | Print errors only. Also turns the live progress block off. |
| `-n`, `--dry-run` | Change nothing on either side. With `-v` or `-i`, list what would be done. |
| `-c`, `--checksum` | Compare by content, not by size and time: every file on both sides is read, no cached hash is trusted, and the index is refreshed from what was read. |
| `-u`, `--update` | Skip files that are newer on the destination. |
| `--delete-before` | Delete before the transfer, once both sides are listed. |
| `--delete-during` | Delete each directory's extraneous entries as the run leaves that directory, so space is freed as the run goes. |
| `--delete-after` | Delete after the transfer; what `--delete` does. |
| `--max-delete N` | Stop deleting after `N` removals. The run exits with code 25. |
| `--exclude PATTERN` | Exclude paths that match an rsync pattern: `*`, `**`, `?`, `[...]`; a leading `/` anchors the pattern to the root, a trailing `/` matches directories only. |
| `--include PATTERN` | Include paths that match, even if a later `--exclude` would hide them. |
| `--exclude-from FILE` | Read exclude patterns from `FILE`, one per line. Blank lines and lines that start with `#` or `;` are skipped. |
| `--include-from FILE` | Read include patterns from `FILE`, as `--exclude-from` does. A pattern may be at most 4,096 bytes long; one run takes at most 16,384 rules, with at most 1 MiB of patterns together. |
| `--bwlimit RATE` | Limit the rate at which file data is sent, on a push and on a pull. On a pull the daemon does the limiting. A bare number is KiB/s. `K`, `M`, `G`, `T`, `P` (also `KiB` to `PiB`) are powers of 1024, `KB` to `PB` powers of 1000, `b` is bytes; decimals are allowed (`1.5M`). `0` means no limit. |
| `--numeric-ids` | Apply owner and group ids as sent. By default the sender's user and group names are mapped to the receiver's ids where they exist, as rsync does. |
| `--partial-dir DIR` | Keep the partial files of interrupted transfers under `DIR`, relative to the destination root, instead of beside their files. On a pull `DIR` may be absolute; on a push the daemon keeps them, and only a relative `DIR` inside the destination is allowed. |
| `--inplace` | Write a changed file into the destination itself instead of into a temporary file renamed over it. No second copy is needed on disk, but readers see a torn file during the transfer and a crash leaves a mixture. |
| `--append` | Extend a destination file that is shorter than the source with the source's tail, trusting what is already there. A destination that is not shorter is skipped. |
| `--append-verify` | Like `--append`, but the existing prefix is verified too. |
| `--fsync` | Flush each received file to disk before renaming it into place, on whichever side receives (the daemon, on a push). Off by default, as in rsync: a power loss or kernel crash in the seconds after a run can then leave a file at its name with the right size and time but not its content, which the next run's quick check does not notice (`-c` does). An export's `fsync = true` turns it on for every push into it. |
| `--paranoid` | Guard against source files that change during the sync, including writers that leave the times alone (memory-mapped databases; tested on ZFS 2.4 clones only). The sending side (on a pull, the daemon) sends each file from a reflink clone where the filesystem can make one (btrfs, xfs, ZFS 2.2 and newer with block cloning, bcachefs, APFS), and otherwise hashes the file a second time before the receiver places it. At the end it checks every listed file again and sends those that changed. Files that vanished give exit 24. Cost: one clone and one delete per sent file where cloning works; an extra full read per sent file elsewhere; one `stat` per listed file at the end. Clones live in `.irsync-clones-<uid>` in a writable directory on the file's filesystem. See [Files that change during a sync](../explanation/correctness.md#files-that-change-during-a-sync). |
| `--ignore-errors` | Run the deletion pass even when a listing was cut short by I/O errors. |
| `--stats` | Print a block of transfer statistics at the end. |
| `-i`, `--itemize-changes` | Print rsync's per-file change summary, the itemize codes. |
| `-V`, `--version` | Print the version and the commit it was built from. |

Filter rules apply in command-line order, and the first rule that matches
wins.

### rsync spelling, different meaning

| Flag | irsync | rsync |
| --- | --- | --- |
| `--delete` | Delete destination files that are gone from the source, after the transfer, the same as `--delete-after`. Nothing is deleted without one of the `--delete` flags. | The same as `--delete-during`. |
| `--delete-excluded` | Also delete destination files that the filters exclude. Implies `--delete`, so the deletions run after the transfer. | Implies `--delete`, so the deletions run during the transfer. |
| `-S`, `--sparse` | Leave chunks that are all zeros as holes. Without it, only files that are sparse in the source are written sparse. | Finds runs of zeros at write granularity, not only whole chunks. |
| `--timeout SECS` | Drop a connection that moves no data and no control message for `SECS` seconds; the session then reconnects. `0`, the default, means no timeout. | A connection idle for `SECS` seconds ends the run with code 30. |
| `--progress` | Draws the live progress block (see [Output](#output)) when stdout is a terminal. | Prints the progress of each file. |
| `-P` | The same as `--progress`. irsync always keeps partial files. | The same as `--partial --progress`. |
| `--info SPEC` | Only `progress1` and `progress2` are read, as `--progress`; the rest is ignored. | Selects informational output, such as the progress of the whole transfer with `--info=progress2`. |
| `-h`, `--help` | Print the flags and exit, also when other flags or operands are given. | `-h` is `--human-readable`, except on its own, where it prints the help. |

### Deletions

Nothing is deleted that might merely have been unreadable. When a listing on
either side is incomplete, because a directory could not be read or an entry
could not be examined, deletions stop from that point on: `--delete-before`
and `--delete-after` then delete nothing, and `--delete-during` stops at the
directory where the gap was found. `--ignore-errors` deletes anyway. Combine
`--delete` with `-n` first to see what it would remove.

### Output

As in rsync, a sync prints nothing on stdout by default. Errors and warnings
go to stderr, each failed file as it fails. `-v` lists each file transferred
and ends with the `sent … received …` and `total size is …` lines; `-q`
prints errors only.

With `--progress` (or `-P`) on a terminal, the client draws a live block at the bottom of the screen: the
listing walk on both sides, a transfer bar with rsync's percent semantics, one
row per file that is being hashed or has a data stream open and is not yet
sent in full, and the deletion pass. The rows are as many as half the
terminal's height allows. The files that are moving come first, fastest
first, so that progress is what you see: then files being hashed, then idle
ones, then stalled ones. A file much larger than the others (four times the
typical file in flight, at most two of them) keeps its place at the top
while it is not stalled, so you can follow it. A last line `… and N more`
counts the files without a row, as in `… and 117 more (23 stalled)` when
some of them are stalled. The transfer bar's `N in flight` counts these files,
including those beyond the rows drawn. It is not the session's window of up to
4,096 files in flight described in
[Files in flight and streams](../explanation/transfer-algorithm.md#files-in-flight-and-streams):
a file in that window that is neither being hashed nor has a stream open has
no row and is not counted. On a pull the client cannot see the server's
streams, so a file gets its row when its first data arrives; until then it
is waiting its turn, not stalled.

The transfer line shows two rates over the last 5 seconds, as in
`41.20MB/s sent  380.00MB/s synced  goodput 9.2×`. `sent` is the file data
that crossed the network. `synced` adds the data the receiver took from
files it already had (matched data), so it is the rate at which the
destination catches up. Goodput is synced data divided by sent data for the
session: `100%` means nothing was matched, and above 2 it is shown as a
ratio, so `9.2×` means each byte sent put about nine in place. Once the
transfer is done, the line shows the same three numbers as averages over the
whole run. A session that ended with an error says `failed` there instead of
`done`, and its deletion pass, if it never started, reads `not run`. No rate is shown in a session's first second, and both read
`0B/s` while the client reconnects. Matched data counts as soon as a file's
chunks are requested, before any of its data moves, so `synced` jumps for a
few seconds when a large, mostly unchanged file starts. On a narrow terminal
the transfer count goes first, then goodput, then the synced rate. While the
block is drawn, failed files show in its
`errors` section instead of scrolling, and the full list goes to stderr once the
block is cleared at the end.

A push of several sources draws the block once per source, one after the
other. Each one starts with a line that names it, such as
`session 2/3  ~/Photos -> myserver:backup`: the source's place in the run
and the two operands as typed. When a source is finished, that line and
its `done` lines stay on the terminal. A run with one source has no such
line.

`-q` turns the block off. When stdout is not a
terminal, or `TERM` is `dumb`, the block is not drawn and the output is plain
lines.

## irsync flags

These have no rsync counterpart.

| Flag | Meaning |
| --- | --- |
| `--parallel N` | Data streams a push keeps open at once, across all files. Default: `parallel` from the server entry, else from `[client]`, else 128. A pull uses the daemon's 128. See [Files in flight and streams](../explanation/transfer-algorithm.md#files-in-flight-and-streams). |
| `--reconnect DUR` | How long to keep redialling after a dropped connection, as `N[s\|m\|h\|d]`. `0` gives up at once, like rsync. Default: `reconnect` from the server entry, else from `[client]`, else 24h. This window applies only once the server has answered; before that, `--dial-retries` applies. |
| `--dial-retries N` | How often to retry a server that has not answered yet, before the run gives up. The waits grow: about 1, 2, 4, 8 seconds and so on, capped at a minute. `0` gives up on the first failure. Default: `dial_retries` from the server entry, else from `[client]`, else 4. For a server `irsync pair` saved (one with `ssh`), the default is 0 and only this flag changes it; each try there waits at most 15 seconds. |
| `--relay URL` | Use this relay server instead of the server entry's, `[client]`'s or n0's default. Cannot be combined with `--no-relay` or `--address`. |
| `--no-relay` | Direct connections only: never fall back to a relay. The address lookups stay on. Default: `relay_mode` from the server entry, else from `[client]`. |
| `--pq-key-exchange prefer\|require` | `require`: refuse a daemon that does not offer the post-quantum key exchange, such as an older irsync; nothing is sent to it. `prefer`: use that key exchange when the daemon offers it. Default: `pq_key_exchange` from the server entry, else from `[client]`, else `prefer`. See [Post-quantum key exchange](configuration.md#post-quantum-key-exchange). |
| `--no-dht` | Do not look the daemon up in the mainline DHT. |
| `--address ADDR` | Dial the daemon at `ip:port`, `[ipv6]:port` or `host:port` directly; repeatable. Turns the relay, DNS, DHT, mDNS and the port mapper off, and binds the client to the local address it dials from (see [Keep to one path](../how-to/direct-and-relay.md#keep-to-one-path)). The daemon's `EndpointId` is still required and still checked. |
| `--config PATH` | The config file. Default: `~/.config/irsync/irsync.toml`. |
| `--identity FILE` | The private key to connect with. Default: `identity` in the config file, else `~/.config/irsync/id_ed25519` when it exists, else `~/.ssh/id_ed25519` when it exists, else a key generated at `~/.config/irsync/id_ed25519`. A named file is the whole answer: when it is missing, a key is generated there. |
| `--passphrase-file PATH` | Read the passphrase of an encrypted key from the first line of this file. Without it, `IRSYNC_PASSPHRASE` is used, else a prompt when stdin is a terminal. |
| `--no-resume` | Start a fresh plan instead of resuming a dormant session. Partial files are still picked up. |
| `--no-rename-detection` | Do not fill files from other files on the receiving side that hold the same content or parts of it, so a moved or copied file is transferred again. A changed file still reuses its own old version. |
| `--debug-addr ADDR` | Serve metrics and span timings over HTTP at `IP:PORT`; a bare port means `127.0.0.1`. The port stays open after the sync until SIGINT or SIGHUP. |
| `--debug-port-no-wait` | With `--debug-addr`, close the port when the sync ends instead of waiting. |

The connection settings resolve in this order: the flag, then the server's
table, then `[client]`, then the built-in default. See
[Configuration](configuration.md#client).

## Subcommands

| Command | What it does |
| --- | --- |
| `irsync sync` | Copy files; the same as the rsync-style form. See [Operands](#operands). |
| `irsync list` | List a host's exports, or the files under `HOST:PATH`. See [Operands](#operands). |
| `irsync pair` | Set a server up over ssh and save it as a server. See [irsync pair](#irsync-pair). |
| `irsync remote` | Manage a server's exports, clients and grants from this client. See [irsync remote](#irsync-remote). |
| `irsync daemon` | Run the daemon. Its flags are under [irsync daemon](#irsync-daemon). |
| `irsync index` | Hash the files under local roots into their index. See [Seed the index](../how-to/seed-the-index.md). |
| `irsync gc` | Remove abandoned partial files, dormant sessions and stale index entries. See [Clean up state](../how-to/clean-up.md). |
| `irsync server`, `client`, `export`, `grant`, `id`, `config`, `show` | Manage servers, clients, exports, grants, identities and settings. See [Management commands](management.md). Each of the first six alone runs its `list`. They take `-v`, `-q`, `--config` and `--passphrase-file` too. `irsync client set NAME all_paths=rw` lets a client reach any path, read-write; it is what `irsync pair` sets. |

### irsync remote

```text
irsync remote SERVER COMMAND
```

Runs a management command on a server's daemon. `SERVER` is a saved server
or the daemon's `EndpointId`. This client needs `admin = "rw"` on the server
to change things, and `admin = "ro"` or `"rw"` to read. `remote` dials the
server like a sync does. It never sets one up: use `irsync pair` for that.

`COMMAND` is one of `export list|add|remove`, `grant list|add|remove`,
`client list|add|remove|rename|set`, `show`, `config get KEY` and
`config list`. Each takes the arguments it takes on the server; see
[Management commands](management.md#remote-use). Each call sends one change,
which the server applies all or not at all. Quote a path that starts with
`~`: `irsync remote myserver export add 'photos:~/Photos'`.

The book has a how-to, "Manage a server from your client".

### Flags every command takes

`-v`, `-q`, `--config PATH` and `--passphrase-file PATH` are accepted before
or after any command: `irsync -v gc` and `irsync id show -vv` both work. A
command name right after these flags is the command. So a directory called
`gc` is written `./gc`: `irsync -v ./gc host:path`. The same goes for `sync`
and `list`: `irsync -av ./sync myserver:` pushes the directory `sync`.

- `-v` asks for the command's own extra output. Where a command has none,
  `-v` shows nothing more. `-vv` gives info logs, `-vvv` debug, `-vvvv`
  trace, and `-vvvvv` trace plus every protocol message.
- `-q` logs errors only and prints no summary. It wins over `-v`.
- `RUST_LOG` overrides the log level.
- `--config PATH` names the config file for every command:
  `irsync --config f server list`.
- `--passphrase-file PATH` gives the passphrase of an encrypted key to the
  commands that load one. The passphrase is the first line of the file; its
  line ending (LF or CR+LF) is not part of it, and nothing else is trimmed.
  Commands that load no key ignore the flag. `irsync id generate` uses it to
  write the new key encrypted. A key generated automatically is never
  encrypted. So the commands that would generate a missing key (a sync, a
  listing, `id show`, `id list`, `show`, `export add` and the daemon) fail
  instead. They write nothing and name `irsync id generate --passphrase-file
  PATH`. The file must be for its owner only (`chmod 600`); see
  [File permissions](configuration.md#file-permissions).
- `irsync show --passphrase-file PATH` reads both the client's and the
  daemon's key. Both must exist already: without the flag it would generate
  the missing one.

Any other sync option given before a command is an error, for example
`-a is a sync option and cannot be used with 'gc'`. That holds for `sync`
and `list` too: write `irsync sync -av …`, not `irsync -av sync …`.

### irsync daemon

```text
irsync daemon [--config PATH] [--passphrase-file PATH]
              [--listen ADDR]... [-v] [-q]
              [--run-as-root-i-know-what-i-am-doing]
```

Serves the exports in the config file to the peers they are granted to, until
it is interrupted. At start it prints `endpoint id: <EndpointId>` and one
`listening on: <socket>` line per bound socket on stdout. Logs go to stderr at
warning level, counted like the sync's `-v`: `-vv` gives info, `-vvv` debug,
`-vvvv` trace, `-vvvvv` trace plus every protocol message, `-q` errors only,
and `RUST_LOG` overrides all of them. When stdout is a terminal, the daemon
first prints a banner with its version, relay and discovery settings.

| Flag | Meaning |
| --- | --- |
| `--config PATH` | The config file. |
| `--passphrase-file PATH` | Read the passphrase of an encrypted daemon key from the first line of this file. |
| `--listen ADDR` | Bind this socket (`ip:port`, `[ipv6]:port` or `host:port`; port 0 lets the system pick) instead of the config's `listen` list. Repeatable, at most one IPv4 and one IPv6. |
| `--run-as-root-i-know-what-i-am-doing` | Start even as root. Without it, or `allow_root = true` under `[daemon]`, the daemon refuses to run as root: it has not had a security audit, and as root any client with write access can create setuid files and device nodes. |
| `-v`, `--verbose` | Nothing extra; `-vv` and up log more. See [Flags every command takes](#flags-every-command-takes). |
| `-q`, `--quiet` | Log errors only. |

### irsync index

```text
irsync index [-n] [-v] [-q] [--config PATH] ROOT...
```

| Flag | Meaning |
| --- | --- |
| `-n`, `--dry-run` | Walk and count what would be hashed; read nothing. |
| `-v`, `--verbose` | Nothing extra; `-vv` and up log more. |
| `-q`, `--quiet` | Log errors only and print no summary. |
| `--config PATH` | The config file, for `state_dir`. |

### irsync gc

```text
irsync gc [-n] [-v] [-q] [--config PATH]
```

| Flag | Meaning |
| --- | --- |
| `-n`, `--dry-run` | List what would be removed without removing anything. |
| `-v`, `--verbose` | Nothing extra; `-vv` and up log more. |
| `-q`, `--quiet` | Log errors only and print no summary. |
| `--config PATH` | The config file, for `state_dir`. |

## Environment

| Variable | Effect |
| --- | --- |
| `IRSYNC_PASSPHRASE` | The passphrase of an encrypted key, when no `--passphrase-file` is given. |
| `IRSYNC_RSH` | The remote shell for `irsync pair`, when no `--rsh` is given. |
| `RUST_LOG` | The log filter, in place of the one `-v` and `-q` choose. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Export traces and metrics over OpenTelemetry to this endpoint. Overrides `[telemetry]` in the config file. |
| `TERM` | `dumb` turns the live progress block off. |

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success. |
| 1 | Nothing got through: a configuration, identity or connection error; an operand pair `irsync` rejects (both sides local, both remote, several sources on a pull); or every source failed. |
| 2 | The command line could not be parsed: an unknown flag, a bad value, a missing operand. |
| 23 | Partial transfer: some files could not be transferred, or some sources of a push failed while others synced. |
| 24 | Some source files vanished. Only `--paranoid`, in its final re-check, gives this. |
| 25 | Deletions stopped at `--max-delete`. |

Of these, 23, 24 and 25 have rsync's meaning; 1 and 2 do not (rsync uses 1 for a
syntax or usage error and 2 for a protocol incompatibility).

When several apply, 1 wins over 25, 25 over 23, and 23 over 24. A file listing, `irsync HOST:PATH`, is a dry-run pull and
uses the same codes; the export listing, `irsync HOST:`, exits 0 or 1.

The other commands exit 0 on success and 1 on an error, with these
additions: `irsync index` and `irsync export update` exit 23 when a file could
not be read or a directory could not be listed, and `irsync config get` exits
1 for a key that is not set and has no default.
