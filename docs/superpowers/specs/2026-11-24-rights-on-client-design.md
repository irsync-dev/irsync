# Rights on the client entry (short design)

Issue: #284. Prepares #203 (admin service), which adds a third key, `admin`.

## Why

Server-wide rights live in lists today (`[all]`, `[immediate]`). Moving them
onto the client entry puts every server-wide right in one place, and lets #203
add `admin` in the same form.

## What

```toml
[daemon]
allow_all_paths = true    # was immediate = true

[clients.mylaptop]
client_id = "3f1a…"
all_exports = "rw"        # was [all]; "rw" | "ro"; left out: none
all_paths = "ro"          # was [immediate]; "rw" | "ro" (ro is new); left out: none
```

- `[all]`, `[immediate]` and `[daemon] immediate` are no longer read. No
  upgrade step (no users yet).
- Only named clients hold these rights.
- `all_paths = "ro"`: read any path, write none. Its automatic exports list
  the client under `ro`. A client lowered from `rw` to `ro` is capped at
  read-only on the automatic exports made for it earlier, at once.
- Unchanged: an export's own list wins over `all_exports`; `all_exports`
  never reaches automatic exports; automatic exports count only while their
  client has `all_paths` and `allow_all_paths` is on; an unknown key is
  refused before any message is read.

## Approaches

The config shape was settled in #203 and the issue. For setting the values:

- **`irsync client set NAME KEY=VALUE...` (picked).** One command for all
  server-wide rights; values `rw`, `ro`, `none`; several pairs applied
  together; #203 adds `admin=` without new commands.
- `grant add all|immediate` kept, pointing at the new keys: keeps two ways
  to say one thing, and has no place for `admin`.
- A separate `client unset`: one more command to remember for no gain over
  `=none`.

## Decided

- `grant add/remove` lose `all` and `immediate`; `grant add all ...`
  answers with a hint to `client set`.
- `client list`, `show` and the grant listing show each client's
  server-wide rights.
- The SSH setup sets `all_paths = "rw"` on its client.
- Reader, writer and `client set` are shaped so a third key of the same form
  is a small addition.

## Left to the builder

Proto layout of the new fields, the exact wording of listings and hints, how
the authorizer represents the access levels.
