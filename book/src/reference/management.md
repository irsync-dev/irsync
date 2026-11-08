# Management commands

These commands read and edit the config file, `~/.config/irsync/irsync.toml`
(see [Configuration](configuration.md)). There are three nouns, `host`,
`export` and `grant`, each with `add`, `remove` and `list`, plus
`export update`; `id` for the identities; `config` for any key by its dotted
path; and `show` for an overview.

Every management command takes `--config PATH` to use another file. For a
noun the flag may stand before or after the verb: `irsync export --config C
add ...` and `irsync export add ... --config C` are the same.

## Editing rules

- The commands keep the file's comments and layout. A missing file is the
  same as an empty one; the first edit creates it.
- While a command edits the file, `irsync.toml.lock` sits beside it, and a
  second editing command is refused. Delete the lock only if no `irsync` is
  running.
- The edited file is checked before it is written. An edit that would leave a
  file `irsync` cannot load is refused, and the file stays as it was.
- When a `hosts.toml` or `exports.toml` sits beside the config file, `irsync`
  reads it but never edits it. `host`, `export`, `grant` and `config set` refuse
  a table that such a file holds, and tell you to move the table into
  `irsync.toml` or to edit that file by hand.
- An export name may not be empty, contain `/`, or be `all`. An alias may not
  be empty, contain `:` or `/`, be `all` or `*`, or be an `EndpointId`.

A running daemon picks up `host`, `export` and `grant` changes on its next
connection; nothing needs a restart.

## host

| Command | Effect |
| --- | --- |
| `irsync host add ALIAS ID [--relay URL] [--relay-mode default\|disabled] [--address ADDR]... [--parallel N] [--reconnect DUR] [--dht yes\|no] [--mdns yes\|no]` | Add an alias for an `EndpointId`, or change the id and the keys given for an existing alias. Keys not given stay as they are. Prints `ALIAS: ID`, or `ALIAS: OLD -> NEW` when the id changed. |
| `irsync host remove ALIAS` | Remove an alias. |
| `irsync host list` | One `ALIAS: ID` line per alias, then one indented `key = value` line per other key. |

The flags of `host add` set the keys of the alias's table; see
[Configuration](configuration.md#hosts) for what each does. `--address`
replaces the stored list and may be repeated. `--dht` and `--mdns` take `yes`
or `no` (also `true` or `false`).

On a daemon's machine the same aliases name the clients that exports are
granted to.

## export

| Command | Effect |
| --- | --- |
| `irsync export add NAME:PATH [--ro] [--warm-index]` | Export a directory under `NAME`. Prints the daemon's `EndpointId` on stdout, generating the daemon's key if it does not exist yet. |
| `irsync export add PATH [--ro] [--warm-index]` | The same, named after the directory's last component. |
| `irsync export remove NAME` | Stop exporting a directory. Its grants go with it. |
| `irsync export list` | One block per export: `NAME: PATH`, then `read_only`, `warm_index` and `fsync` when set, then its grant lines. |
| `irsync export update [-n] [-v\|-q] NAME...` | Hash the files of the named exports into their indexes, so that files `irsync` did not place are reused from the next session on. `all` names every export. `-n` counts what would be hashed. See [Seed the index](../how-to/seed-the-index.md). |

`export add` joins a relative `PATH` onto the working directory, expands a
leading `~` (`irsync export add home:~` exports the home directory), and
requires an existing directory. The text before the first `:` is a name only
when it has no `/`. `--ro` sets `read_only`, which refuses pushes from
everyone; `--warm-index` sets `warm_index`.

## grant

| Command | Effect |
| --- | --- |
| `irsync grant add EXPORT WHO [--ro]` | Grant `WHO` read-write access to `EXPORT`, or read-only access with `--ro`. `EXPORT` may be `all`, for every export. An earlier grant of the same peer there is replaced. |
| `irsync grant remove EXPORT WHO` | Remove `WHO`'s grant on `EXPORT`. `irsync grant remove all WHO` removes `WHO` everywhere; `irsync grant remove EXPORT all` empties an export's grants. |
| `irsync grant list` | Every grant: an `all:` block, then one block per export. |
| `irsync grant list EXPORT` | One export's grants; `all` lists those that apply to every export. |
| `irsync grant list WHO` | What one peer may reach, one `NAME: rw` or `NAME: ro` line per export, marked `(all)` when the grant comes from `[all]` and `(read_only export)` when a read-write grant is capped by `read_only`. |

`WHO` is an alias, an `EndpointId`, a pasted `ssh-ed25519 AAAA...` public key
line, or the path of a `.pub` file holding one. A key's comment is kept as a
TOML comment beside the entry. There is no open access: `all` and `*` are
refused as `WHO` in `grant add`.

## id

| Command | Effect |
| --- | --- |
| `irsync id show [--passphrase-file PATH]` | Print the client's public key line and `EndpointId`, generating the key if no key serves. |
| `irsync id show --daemon` | The same for the daemon. Never generates the daemon's key: without one it fails and says how to make one. |
| `irsync id show -a` | Both, labelled `client:` and `daemon:`. |
| `irsync id generate [--daemon \| --out PATH] [-f]` | Write a new Ed25519 key pair at the client's path (default), the daemon's path, or `PATH`, with the public key beside it as `.pub`, and print the public key line and `EndpointId`. Refuses to overwrite an existing key or `.pub` file unless given `-f`/`--force`. |

The client's key is found the way ssh picks one; see
[Configuration](configuration.md#identity-and-state). After `irsync id
generate` has written `~/.config/irsync/id_ed25519`, `~/.ssh/id_ed25519` is no
longer consulted. Replacing a key with `--force` is permanent: every grant made
for the old key stops working.

## config

| Command | Effect |
| --- | --- |
| `irsync config get KEY` | Print a key: a bare scalar, one array element per line, or a table's keys as `key = value` lines, all as the file holds them (a path's `~` is not expanded). An unset setting prints its default; a key with no value and no default exits 1. |
| `irsync config set KEY VALUE...` | Set a key in its type: `true` or `false`, an integer, a duration such as `30m`, a path, a URL, an `EndpointId`. An array key takes one `VALUE` per element, and none for an empty array. Tables are created as needed. |
| `irsync config unset KEY` | Remove a key, or a whole table (`irsync config unset hosts.myserver`). A table left empty goes with it. |
| `irsync config list` | Every key the files set, as `dotted.key = value`, in file order. |

`KEY` is a dotted path such as `daemon.listen` or `hosts.myserver.relay`.
Quote a segment that holds a dot: `exports."www.example.com".path`. An unknown
key is an error.

`config` writes what you say; the nouns know what you mean. `export add` joins
a relative path onto the working directory and checks that the directory
exists, and `grant add` reads a pasted key line or a `.pub` file. In contrast,
`irsync config set exports.photos.path ./x` is refused, because the file wants
an absolute path, and `irsync config set exports.photos.rw ~/myclient.pub`
writes that string as a grant entry that matches nobody.

## show

```text
irsync show [--passphrase-file PATH]
```

Prints the config file's path, the client's `EndpointId` and public key line,
the file that holds the aliases and the aliases as `host list` prints them, the
file that holds the exports, the daemon's `EndpointId`, and the exports as
`export list` prints them, followed by the `[all]` grants. A split file is
marked `(also read)`. `show` generates either key if it does not exist yet.
