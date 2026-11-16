# Management commands default to `list`

Date: 2026-11-16

Issue: #32

## Intent

A study user typed `irsync id` and got a help page. They expected to see the
identity. They also tried `irsync id list`, because `host`, `export`, `grant`
and `config` all have `list`, and got "unrecognized subcommand".

Typing a noun on its own should show what is there. That is the read-only
thing a user most likely wants, and it makes the nouns behave alike.

## Goals

- `irsync id list` exists and lists the identities this machine has.
- `irsync id`, `irsync host`, `irsync export` and `irsync grant` with no
  subcommand run their `list`.
- `irsync id show` keeps its exact behaviour and output.

## Non-goals

- Changing `show`, `generate` or any existing `list` output.
- `-v` on the management commands (#31).
- `host rename` and anything else on other issues.

## Anti-goals

- A bare noun must never change exports, grants or hosts. It only lists.
- No existing command line that works today may stop working or print
  something else.

## Approaches

1. **A real `list` command for `id`, and an optional subcommand on each
   noun that falls back to `list`.** Chosen. `list` shows up in `--help`,
   the parser's own errors stay correct, and `show` is not touched.
2. **A hidden flag on `show`**, with `list` mapped onto it. Rejected:
   `list` is to stay separate from `show`.
3. **Rewrite the arguments before parsing** (`irsync id` becomes
   `irsync id list`). Rejected: the default is invisible in `--help`, and
   usage errors would name a command the user did not type.

## Design

### `irsync id list`

Decided: it prints the client identity, and the daemon identity when one
exists. A machine without a daemon identity is not an error.

```
$ irsync id list
client:
ssh-ed25519 AAAA... irsync
a3e4753e5ea74e64e745e9c4d10b0fd751dbe03049d742eb0ea545c89703dfba
daemon:
ssh-ed25519 AAAA... irsync-daemon
4a29f16a4f80a3fd171ef5573c106dbda470ec24f5a97933752746b567ab9aff
```

On a machine with no daemon identity, only the `client:` block is printed
and the exit code is 0.

My calls, not yet confirmed:

- The blocks are the ones `show -a` prints, built by the same code.
- No stderr note about the daemon identity. `show` prints one because it
  hides the daemon; `list` shows it.
- A missing client identity is created, as `show` and a sync do. Otherwise
  `irsync id` on a fresh machine would print nothing.
- A missing daemon identity is never created.
- `irsync id list` takes `--passphrase-file`, as `show` does.

### Bare nouns

Decided: `id`, `host`, `export` and `grant` with no subcommand run `list`.

- `irsync id` is `irsync id list`.
- `irsync host` is `irsync host list`.
- `irsync export` is `irsync export list`.
- `irsync grant` is `irsync grant list` with no target: every grant.

My calls, not yet confirmed:

- **`irsync config` too.** It has a `list` and would otherwise be the only
  noun that still prints help. Drop this if `config` should stay as it is.
- A bare noun takes only `--config`. Options of `list` itself, such as
  `--passphrase-file` or a grant target, need `list` typed out.
- `irsync NOUN --help` still prints the help page, and its usage line shows
  the subcommand as optional.

### Docs

- `book/src/reference/management.md`: add the `irsync id list` row, and one
  sentence saying a noun on its own runs `list`.
- The sentences and error messages that say "plain `irsync id show` prints
  the client's" stay. They are still true.

## Tests

- `id list` with and without a daemon identity: output and exit code.
- `id list` on a fresh config directory creates the client identity and not
  the daemon's.
- Each bare noun prints exactly what its `list` prints.
- `id show`, `id show --daemon` and `id show -a` are unchanged (the existing
  tests cover this).

## Left to the builder

- How the fallback is expressed in the command-line definition.
- Where the shared printing code for `show` and `list` lives.
