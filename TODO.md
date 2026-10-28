# Collection of open TODO items, in no particular order

- [ ] rsync-like "ad-hoc" mode that automatically spawns (and shuts down) a daemon on the other side
    - Boot-strap via ssh (similar to mosh)
    - Don't register in DHT for ad-hoc mode
- [ ] OpenTelemetry instrumentation, export on the daemon side, stats printing after a run
- [ ] Auto-update mode that updates irsync on the remote side, with correct architecture (via ssh / scp)
- [ ] Adversarial fuzz testing / hardening for the daemon mode
- [ ] Performance and resource measurements on daemon mode, potentially re-think execution model (fork vs. threads, ulimits)

# Open items found while working, 2026-10-28

After the single-config work landed on main. Nothing below is tracked
anywhere else: the code has no TODO markers, no ignored tests and no
unfinished plan workspaces.

## Known limitations (README "Known limitations in version one")

Rough cost next to each. The last two are not things to fix.

- [ ] `--delete-during` runs as `--delete-after`. Small: local to the delete
      code, delete per directory as it is transferred.
- [ ] Keys under `~/.ssh` are not read unless `identity` or `--identity`
      names them. Small: a default lookup. Hardware and ssh-agent keys stay
      out, since iroh needs the raw secret for its transport.
- [ ] `[daemon]` settings need a restart; exports, grants and aliases take
      effect on the next connection. Medium: a reload path for listen, relay
      and discovery.
- [ ] On a push the daemon applies full archive metadata whatever the
      client's `-p -t -o -g -l -D` say. Medium: a protocol change so the
      daemon receives the flags.
- [ ] Filters (`--exclude`, `--include`, ...) apply only on the sending
      side, so a push with `--delete` plus filters also needs
      `--delete-excluded`. Medium: a protocol change so the rules travel.
- [ ] `--partial-dir` is refused on a push. Medium.
- [ ] `--numeric-ids` is always on. Medium: owner and group name mapping.
- [ ] irsync always recurses; `-r` is accepted and implied. Only worth doing
      if a non-recursive copy is ever wanted.
- Relays are a fallback and much slower than direct connections. By design.
- Linux and macOS only. A port, not a fix.

## Flaky tests

Seen once each under full-suite load, pass on rerun, never investigated. A
looped run would show whether they are timing-dependent.

- [ ] `resume::incomplete_plan_resumes_with_fresh_listing`
- [ ] `progress_e2e::sender_file_start_means_a_stream_opened_not_a_plan`

## Config editing: behaviours ruled as standing in the single-config review

User-visible, left as is on purpose; revisit if they bite.

- [ ] Two concurrent edits of `irsync.toml` (`config set`, or any noun) can
      lose an update: each reads, edits and replaces the whole file with no
      lock.
- [ ] `config set exports.NAME.path` accepts export names the nouns would
      refuse.
- [ ] `config unset` of a table's last key leaves an empty `[table]` header.
- [ ] `config get` prints a set path verbatim (`~/...` stays `~/...`); only
      the daemon and client expand it.

## fileset: deferred findings never triaged

`docs/superpowers/plans/2026-10-19-fileset-deferred.md` lists the minors
deferred from the fileset reviews. The ones with user-visible effect:

- [ ] fjall sweep buffers every doomed key before one batch; memory grows
      with deletions, against the spec's memory bound.
- [ ] A crash during `create` before commit leaves an empty
      `.fileset.sqlite` that `create` then refuses; needs manual deletion.
- [ ] `create` failing after the connection is open leaves a partial file.
- [ ] sqlite `wal_checkpoint(TRUNCATE)` busy flag is discarded; close
      reports success even when blocked.
- [ ] `-j 0` is accepted unvalidated.
- [ ] Missing tests listed per task in that file (schema migration paths,
      mount-point non-crossing, relative `--manifest`, and others).
