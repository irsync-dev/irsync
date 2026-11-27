# Manage a server from your client

`irsync remote` changes a server's exports, clients and grants from your
own machine, without ssh. It uses the same connection a sync does.

## Give a client the right

On the server, give the client `admin`:

```sh
irsync client set myclient admin=rw
```

A client paired with `irsync pair` already has it: `pair` sets
`admin = "rw"` together with `all_paths = "rw"`.

There are two levels:

- `admin = "rw"` reads, and changes exports, clients and grants.
- `admin = "ro"` reads only. It lets the client run `list`, `show`,
  `config get` and `config list`, and nothing that changes the server.
  `config get` and `config list` show every setting in the server's config
  file, such as its `state_dir`, the ids and addresses of the servers it has
  saved, and its `[defaults]` settings. None of them is a secret.

`admin = "rw"` is the whole account, like `all_paths = "rw"`: a client that
can add an export and grant itself access to it can reach any path. Give it
only to machines you trust that far. See the
[threat model](../explanation/threat-model.md).

The server must run the same irsync version as the client. An older server
answers `myserver runs an older irsync: update it there`.

## Run a command

Name the server first, then the command. `myserver` is a saved server (see
[Pair with a server over ssh](pair.md)) or the daemon's `EndpointId`.

```sh
irsync remote myserver export add 'photos:~/Photos'
irsync remote myserver grant add photos myclient
irsync remote myserver show
```

Quote a home path, as in `'photos:~/Photos'`. Without the quotes your own
shell turns `~` into your home directory here, and the server would get the
wrong path. `irsync remote` notices a path that starts with your own home
and stops before it connects, with the command to use instead.

The commands that work remotely:

| Command | Needs |
| --- | --- |
| `export list`, `export add`, `export set`, `export rename`, `export remove` | `ro` to list, `rw` to change |
| `grant list`, `grant add`, `grant remove` | `ro` to list, `rw` to change |
| `client list`, `client add`, `client remove`, `client rename`, `client set` | `ro` to list, `rw` to change |
| `show` | `ro` |
| `config get KEY`, `config list` | `ro` |
| `index` | `ro` to list, `rw` to start |

They take the same arguments as on the server; see
[Management commands](../reference/management.md). Not remote: `config set`,
`config unset`, `id` and `server`. Do those on the server.

The server answers each command. The lines it prints may name the server's
own config path.

## What a change does

Each call is one change. The server applies it all or not at all, while it
holds the config lock. If it fails, the config file is as it was, and the
error says why. When another command is editing the file, the server waits up
to 2 seconds and then answers `busy`. Run the command again.

A client cannot remove its own `admin` right, or lower it to `ro`, from afar.
Do that on the server. This keeps you from locking yourself out by mistake.

Reads answer from the saved config file. The last change wins: there is no
check that someone else did not change the same thing a moment ago.

## Index an export

A new export with files in it helps a sync only once those files are
hashed into its index (see [Seed the index](seed-the-index.md)). Ask the
server to do it:

```sh
irsync remote myserver export add 'photos:~/Photos'
irsync remote myserver index photos
```

The server hashes the files in the background. `irsync remote` waits, asks
every second how far it got, and prints the summary at the end, the line
`irsync index` prints on the server, such as
`… 1204 file(s), 1204 hashed (5120000 bytes)`. Ctrl-C stops
the waiting, not the hashing. Each folder is hashed by one run at a time: if
one is going already, the command waits for it.

To see what the server is hashing, or how its last runs ended:

```sh
irsync remote myserver index
```

The answer counts the files that could not be read but does not name them,
even to a client with `admin = "rw"`. The server's log names them, one
`warn` line each. The list is kept in memory: it starts empty each time the
daemon starts.

## What the server logs

The server logs each change at `info`, with the client's name, and each
indexing run a client starts, as `admin: mylaptop started indexing export photos`. A refused
attempt, such as a client with no `admin` right or a change that fails, is
logged at `warn`, with the name. Reads are not logged.

A client without `admin` gets only `not allowed`. It learns nothing about the
server.

The text trace (`-vvvvv`) and the capture file show the admin messages, on
both sides, like any other. A capture of an admin session holds the config
contents it carries: export paths, client names and ids. It holds no secrets.
See [Trace and capture the protocol](../contributing/protocol-debugging.md).

## See also

- [Management commands](../reference/management.md)
- [Rights on a client](../reference/configuration.md#rights-on-a-client)
- [Change exports and settings](change-exports.md)
