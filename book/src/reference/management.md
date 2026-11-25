# Management commands

These commands read and edit the config file, `~/.config/irsync/irsync.toml`
(see [Configuration](configuration.md)). There are four nouns, `server`,
`client`, `export` and `grant`, each with `add`, `remove` and `list` (`server`
and `client` also `rename`), plus `export update`; `id` for the identities; `config` for any key by its dotted
path; and `show` for an overview.

A noun on its own runs its `list`: `irsync server`, `irsync client`, `irsync export`,
`irsync grant`, `irsync id` and `irsync config` print what is there and
leave the config file alone (`irsync id` creates the client's key when
there is none). Arguments of `list`, such as a grant target, need `list`
typed out.

Every command here accepts `-v` and `-q`, before or after it; see the
[command-line reference](command-line.md#flags-every-command-takes).

Every management command takes `--config PATH` to use another file. For a
noun the flag may stand before the noun, or before or after the verb:
`irsync --config C export add ...`, `irsync export --config C add ...` and
`irsync export add ... --config C` are the same.

## Editing rules

- The commands keep the file's comments and the order of its servers, clients and
  exports. They write the tables in a fixed order and each value in one
  spelling: a duration `"90"` becomes `"90s"`. Blank lines and inline tables
  are not kept. URLs and paths stay as you wrote them. The first edit of a
  file rewrites it this way. A missing file is the same as an empty one; the
  first edit creates it.
- A file that does not load must be fixed by hand. The error names the line.
- While a command edits the file, `irsync.toml.lock` sits beside it, and a
  second editing command is refused. Delete the lock only if no `irsync` is
  running.
- The edited file is checked before it is written. An edit that would leave a
  file `irsync` cannot load is refused, and the file stays as it was.
- An export name may not be empty, contain `/`, or be `all`. A server or client name may not
  be empty, contain `:` or `/`, be `all` or `*`, or be an `EndpointId`.

## Remote use

Most of these commands also work on a server from afar, with
`irsync remote SERVER` in front: `irsync remote myserver export list`. See
[irsync remote](command-line.md#irsync-remote) and
the how-to "Manage a server from your client". It needs
`admin` on the server.

| Remote | Not remote |
| --- | --- |
| `export list`, `add`, `remove` | `export update` |
| `grant list`, `add`, `remove` | `config set`, `config unset` |
| `client list`, `add`, `remove`, `rename`, `set` | `id` |
| `show`, `config get`, `config list` | `server` |

A remote `export add` takes an absolute path or one that starts with `~`,
quoted so that your shell leaves it alone. A client cannot remove or lower its
own `admin` right this way.

A running daemon picks up `client`, `export` and `grant` changes on its next
connection; nothing needs a restart.

## server

| Command | Effect |
| --- | --- |
| `irsync server add NAME ID [--relay URL] [--relay-mode default\|disabled] [--address ADDR]... [--parallel N] [--reconnect DUR] [--dial-retries N] [--dht yes\|no] [--mdns yes\|no] [--pq-key-exchange prefer\|require]` | Add a server: a name for the `EndpointId` of a daemon you connect to. For an existing name, change the id and the keys given. Keys not given stay as they are. Prints `NAME: ID`, or `NAME: OLD -> NEW` when the id changed. |
| `irsync server remove NAME` | Remove a server. Grants never name servers, so none can stop it. |
| `irsync server rename OLD NEW` | Rename a server. It keeps its place, keys and comments. Refused, with nothing changed, when `OLD` is no server, `NEW` is not a valid name or `NEW` is a server already. Only the config file changes: scripts and commands that use `OLD` keep using it. |
| `irsync server list` | One `NAME: ID` line per server, then one indented `key = value` line per other key. |

The flags of `server add` set the keys of the server's table; see
[Configuration](configuration.md#servers) for what each does. `--address`
replaces the stored list and may be repeated. `--dht` and `--mdns` take `yes`
or `no` (also `true` or `false`). `--pq-key-exchange require` refuses this
server when it does not offer post-quantum key exchange.

## client

| Command | Effect |
| --- | --- |
| `irsync client add NAME ID` | Add a client: a name for the `EndpointId` a machine has as a client (what `irsync id show` prints on that machine). For an existing name, change the id; grants that name it then apply to the new id, and a note on stderr says so. Prints `NAME: ID`. |
| `irsync client remove NAME [-f]` | Remove a client. Refused while a grant names it, and the error lists where: without the client those grants would match nobody. A grant written as the `EndpointId` counts as naming the client, unless another client has the same id. `-f`/`--force` removes those grants, then the client. |
| `irsync client rename OLD NEW` | Rename a client, and every grant that names it: in every export. Its `all_exports`, `all_paths` and `admin` stay with it. A grant written as the `EndpointId` counts as naming the client. The client keeps its place and comments. Refused, with nothing changed, when `OLD` is no client, `NEW` is not a valid name or `NEW` is a client already. Only the config file changes. |
| `irsync client set NAME KEY=VALUE...` | Set the server-wide rights of a client. `KEY` is `all_exports`, `all_paths` or `admin`, and `VALUE` is `rw`, `ro` or `none` (`none` removes the right). Give several pairs to change them together: if one is wrong, nothing changes. `irsync client set myclient all_paths=rw` lets `myclient` reach any path this account can. Prints the client's rights afterwards. See [Rights on a client](configuration.md#rights-on-a-client). |
| `irsync client list` | One `NAME: ID` line per client, followed by `all_exports=VALUE` `all_paths=VALUE` and `admin=VALUE` for each right it has. |

A server and a client may have the same name; the two commands do not touch each other's
entries.

## export

| Command | Effect |
| --- | --- |
| `irsync export add NAME:PATH [--ro] [--warm-index]` | Export a directory under `NAME`. Prints the daemon's `EndpointId` on stdout, generating the daemon's key if it does not exist yet. |
| `irsync export add PATH [--ro] [--warm-index]` | The same, named after the directory's last component. |
| `irsync export remove NAME` | Stop exporting a directory. Its grants go with it. |
| `irsync export list` | One block per export: `NAME: PATH`, then `read_only`, `warm_index` and `fsync` when set, then its grant lines. |
| `irsync export update [-n] [-v] [-q] NAME...` | Hash the files of the named exports into their indexes, so that files `irsync` did not place are reused from the next session on. `all` names every export. `-n` counts what would be hashed. See [Seed the index](../how-to/seed-the-index.md). |

`export add` joins a relative `PATH` onto the working directory, expands a
leading `~` (`irsync export add home:~` exports the home directory), and
requires an existing directory. The text before the first `:` is a name only
when it has no `/`. `--ro` sets `read_only`, which refuses pushes from
everyone; `--warm-index` sets `warm_index`.

## grant

| Command | Effect |
| --- | --- |
| `irsync grant add EXPORT WHO [--ro]` | Grant `WHO` read-write access to `EXPORT`, or read-only access with `--ro`. An earlier grant of the same peer there is replaced. For every export or any path, use `irsync client set`; `grant add all` is refused, with a hint. |
| `irsync grant remove EXPORT WHO` | Remove `WHO`'s grant on `EXPORT`. `irsync grant remove all WHO` removes `WHO` from every export's own list. It leaves their `all_exports`, `all_paths` and `admin` alone, and says so when `WHO` still has one; those go with `irsync client set`. `irsync grant remove EXPORT all` empties an export's grants. |
| `irsync grant list` | Every grant: an `all_exports:` block, an `all_paths:` block, an `admin:` block (the clients that hold each right), then one block per export. |
| `irsync grant list EXPORT` | One export's grants. `all_exports`, `all_paths` and `admin` list the clients that hold that right. |
| `irsync grant list WHO` | What one peer may reach: `all_paths: rw` or `all_paths: ro` and `admin: rw` or `admin: ro` first when it has those rights, then one `NAME: rw` or `NAME: ro` line per export, marked `(all_exports)` when the access comes from `all_exports` and `(read_only export)` when a read-write grant is capped by `read_only`. |

A client paired over ssh is listed by its host name, which `irsync pair` saved
in `[clients]`.

`WHO` is a client name, an `EndpointId`, a pasted `ssh-ed25519 AAAA...` public key
line, or the path of a `.pub` file holding one. A key's comment is kept as a
TOML comment at the end of the entry's line. Removing the grant removes that
comment; comment lines above it stay. A peer that `[clients]` has a name for is stored
by that name, however it was named. A server name is not a `WHO`. There is no open access: `all` and `*` are
refused as `WHO` in `grant add`. The words `all`, `all_exports`, `all_paths` and `admin` are reserved: no export has one of them as its name.

## id

| Command | Effect |
| --- | --- |
| `irsync id list [--passphrase-file PATH]` | Print every identity this machine has: `client:` with the client's public key line and `EndpointId`, then `daemon:` with the daemon's when a daemon key exists. Generates the client's key if no key serves; never the daemon's. |
| `irsync id show [--passphrase-file PATH]` | Print `client:`, then the client's public key line and `EndpointId`, generating the key if no key serves. On a machine that also has a daemon key, a note on stderr says that clients connect to the daemon's `EndpointId` and that `irsync id show --daemon` prints it. |
| `irsync id show --daemon` | The same for the daemon, under `daemon:`. Never generates the daemon's key: without one it fails and says how to make one. |
| `irsync id show -a` | Both, each under its label. |
| `irsync id generate [--daemon \| --out PATH] [-f] [--passphrase-file PATH]` | Write a new Ed25519 key pair at the client's path (default), the daemon's path, or `PATH`, with the public key beside it as `.pub`, and print the public key line and `EndpointId`. Refuses to overwrite an existing key or `.pub` file unless given `-f`/`--force`. With `--passphrase-file`, the private key is written encrypted with the passphrase on the first line of that file (its line ending, LF or CR+LF, is not part of it). A file that is missing or empty is an error and nothing is written. `IRSYNC_PASSPHRASE` does not encrypt a new key. |

Some commands generate a missing key automatically: a sync, a listing,
`id show`, `id list`, `show`, `export add` and the daemon. A key generated
automatically is never encrypted. So with `--passphrase-file`, these commands
do not generate a missing key. They fail, write nothing, and tell you to run
`irsync id generate --passphrase-file PATH` first. Add `--daemon` for the
daemon's key.

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
| `irsync config unset KEY` | Remove a key, or a whole table (`irsync config unset servers.myserver`). A table left empty goes with it. Removing a client this way is refused while a grant names it, as with `client remove`. |
| `irsync config list` | Every key the files set, as `dotted.key = value`, in the order the file is written in: the tables in a fixed order, and each table's keys in a fixed order. |

`KEY` is a dotted path such as `daemon.listen` or `servers.myserver.relay`. A client has one key, `clients.NAME.client_id`. `config_version` can be read but not set.
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
the servers as `server list` prints them under `servers:`, the clients (with their rights) as `client list` prints them under `clients:`, and under `exports:` the
daemon's `EndpointId` and the exports as `export list` prints them. `show` generates
either key if it does not exist yet.
