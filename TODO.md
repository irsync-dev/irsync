# Critical

- [ ] Sign release files and verify them against a public key built into
      irsync. Today a release download is checked only by SHA-256 over
      HTTPS, which catches a damaged file but not a swapped one. Applies
      to the cross-platform install over ssh (`ssh/release.rs`) and to
      `site/install.sh`.
    - Since 2026-11-15 every release file has a GitHub attestation, and
      anyone can check one by hand (`book/src/how-to/verify-a-download.md`).
      Neither of the two install paths checks it yet, so this stays open.

# Collection of open TODO items, in no particular order

- [ ] cargo-binstall packaging, so `cargo binstall irsync` installs the
      release binaries. `cargo install` builds from source and installs no
      man pages; see whether binstall can bring them.
- [ ] OpenTelemetry instrumentation, export on the daemon side, stats printing after a run
- [ ] Auto-update irsync on the remote side, with the correct architecture
    - [ ] Only happens when a run goes through ssh (first sync, `--ssh`, or the
          fallback when the daemon can't be reached); a reachable older daemon is
          not updated on its own
- [ ] Adversarial fuzz testing / hardening for the daemon mode
- [ ] Performance and resource measurements on daemon mode, potentially re-think execution model (fork vs. threads, ulimits)

# Open items found while working, 2026-10-28

After the single-config work landed on main. Nothing below is tracked
anywhere else: the code has no TODO markers, no ignored tests and no
unfinished plan workspaces. Every fixable entry of the known limitations
(now `book/src/reference/limitations.md`) was fixed by 2026-10-29; what the
book still lists there is by design (relay speed, Linux and macOS only).

## fileset: deferred findings

`docs/superpowers/plans/2026-10-19-fileset-deferred.md` is the ledger of
minors deferred from the fileset reviews. Everything in it has since been
fixed or covered by a test (2026-10-29), except two that need what the code
does not have yet:

- [ ] A test for the once-only "cannot read directory" warning on a `readdir`
      that fails mid-listing: needs fault injection into the walk.
- [ ] A test for a SQLite manifest migrating from an older schema: needs a
      second schema version to exist.

# Open items from SSH onboarding, 2026-11-10

Left over after `docs/superpowers/plans/2026-11-10-ssh-onboarding.md`
landed. Items the final review's fix wave closed are not listed.

## Grants and automatic exports

- [ ] `grant list WHO` still shows `immediate: rw` and auto-export lines when
      `daemon.immediate = false`, although the daemon ignores them
      (`list_client` builds its snapshot with immediate on). Over-reports, never
      under-reports.
- [ ] The "no exports defined" startup warning is skipped when `[immediate]`
      has entries, even if `daemon.immediate = false`.
- [ ] GC of an automatic export (often `$HOME`) is not limited to one
      filesystem.
- [ ] A session on an outer export that writes below a nested export's root
      leaves stale records in the outer index that no GC collects (the outer
      GC skips that subtree). Small leak, no data loss.
- [ ] `nested_roots` compares paths as written, so symlinked or
      non-canonical layouts fall back to the old GC behaviour for that pair.
- [ ] A push to a missing directory creates it before the canonical-root
      check and before the export is added; if either fails, the empty
      directory stays.
- [ ] `add_auto_export` stores `to_string_lossy()` and the name drops `.`/`..`;
      callers must pass canonical UTF-8 paths (the daemon does).
- [ ] Error text sent to an immediate peer when the config is locked includes
      the lock file's path.
- [ ] Where `$HOME` sits under a symlink, every connection takes the config
      lock to re-check an automatic export (correct, extra lock traffic).

## ssh-setup and the daemon lock

- [ ] Setup reports "started" once the daemon holds `daemon.lock`, which is
      before the endpoint is bound; a bind failure then looks like a dead
      daemon. Write the lock record (or wait for "endpoint id:") after binding.
      Since 2026-11-10 setup waits up to 10 s for `daemon.addr`, which is
      written after binding, but it still says "started" when the daemon
      dies in that time.
- [ ] Two `ssh-setup` runs at once: a probe can read the lock record as
      "owner unknown" (pid 0, reported as foreign), and the second run can
      fail with "daemon exited before it was ready". Re-probe briefly; treat a
      holder that is our exe and version as Running.
- [ ] An encrypted daemon key is a dead end: setup stops before grant and
      probe on every rerun, even after the user starts the daemon by hand.
      Could read the endpoint id from the `.pub` file. No test for the error.
- [ ] The spawned daemon keeps the ssh session's working directory
      (`current_dir("/")` would be cleaner); `daemon.log` grows without limit.
- [ ] The SIGTERM handler is registered inside a spawned task, so a very early
      SIGTERM uses the default action (harmless: the lock is released anyway).

## ssh client and onboarding

- [ ] A same-platform copy that does not run on the server (older glibc,
      NixOS build) is refused with a clear message since 2026-11-14. It
      could fall back to the release binary, which is static on Linux.
- [ ] The first run makes three ssh connections (probe, install, setup), so
      password users get three prompts. Consider ControlMaster or folding
      probe and setup together.
- [ ] Listings (`irsync myserver:`) do not fall back to ssh when the daemon
      is silent; only syncs do.
- [ ] A failed `connect_for` rebuild in the fallback loop returns from `sync`,
      dropping earlier sources' totals and a `SessionEnd`.
- [ ] The client identity is generated before onboarding, even when it then
      stops (other platform).
- [ ] An alias save that fails after setup has changed the server prints no
      summary of what was done there.
- [ ] The launchd plist in the how-to has a literal `$HOME` to replace by hand.
- [ ] The platform refusal omits the raw `uname -sm` text; `replaced` is empty
      when an unreadable file was overwritten; `run_setup` doesn't tell ssh's
      exit 255 from a remote failure; a whitespace-only `--rsh` ignores
      `IRSYNC_RSH`.
- [ ] The exit-3 sentinel could misreport a remote failure as a short
      upload.

## Tests

- [ ] CLI tests for `grant add immediate --ro` (usage error) and
      `grant remove immediate`; `grant_immediate` alias-vs-id dedup.
- [ ] Immediate peers: symlinked paths; `irsync -l newhost:` onboarding.
- [ ] `daemon_lock::acquire`'s retry path and the pid-0 probe result.
- [ ] `tests/fixtures/fake-ssh` takes `$1` as the destination (breaks if
      `--rsh` carries flags) and logs only the destination, not the command.
- [ ] Tidy: redundant `immediate: false` next to `..Grants::default()` in tests.

# Open items from the cross-platform install, 2026-11-14

Left over after `docs/superpowers/plans/2026-11-14-cross-platform-install.md`
landed. Minors from the final review; the signature check is under
"Critical" at the top.

## The download on the client (`ssh/release.rs`)

- [ ] No timeout or retry: a stalled connection hangs after "downloading
      irsync …" until Ctrl-C. Consider curl's `--connect-timeout`, `--retry`
      and `--speed-limit`/`--speed-time`.
- [ ] Ctrl-C during the download or upload leaves `irsync-release.*` (the
      tarball and the binary) in `TMPDIR`.
- [ ] The `wget` fallback runs with `-q`, so it prints nothing of its own on
      failure, and it has no https-only limit on redirects as curl does.
- [ ] A missing `tar`, or a GNU tar without `xz`, is noticed only after both
      downloads, and tar's multi-line error text lands in a one-line message.
- [ ] "Not in the tarball" is told from "tar failed" by tar's English "not
      found in archive"; under another locale a missing binary reads as
      "cannot unpack". Run tar with `LC_ALL=C`.
- [ ] GNU tar takes a `:` in the archive path for a remote host, so a
      `TMPDIR` with a colon breaks the unpack. Use `--force-local`, or run
      tar inside the temp directory with a relative name. bsdtar is fine.
- [ ] `~/.curlrc` is honoured, so an odd user config can change the
      download. `curl -q` would ignore it, proxy settings included.
- [ ] A clean build ahead of its release (`cargo install --git`, a committed
      branch) counts as a release build: every ssh run downloads and uploads
      the release binary before the server rejects it. Correct message, slow.
- [ ] The "downloading irsync …" line always goes to stderr; nobody checked
      whether a quiet flag should silence it.

## The upload command on the server (`ssh/client.rs`)

- [ ] A CPU that irsync does not know is reported as "an unknown CPU";
      pass the raw `uname -m` text through.

# Open items from the ssh install through the installer, 2026-11-17

What the change (`docs/superpowers/specs/2026-11-17-ssh-install-matches-installer-design.md`)
left open. It fixed the two items about the upload command that stood here
(the empty directory after a rejected install, the leftover temp file after
a failed step).

- [ ] The installer's login-shell probe is not run for csh and tcsh.
- [ ] A service file under another name, or in another place than the two
      the setup looks at, is not found. Neither is a service file that
      names the old path through a variable.
- [ ] A daemon run by a supervisor other than those two service files is
      restarted by the setup, as if the setup had started it.
- [ ] The installer's interactive login-shell probe can make zsh write
      `~/.zcompdump`. It has not been tried under `ssh -t`.
- [ ] `release::fetch` unpacks a downloaded tarball with a bare `tar -xJf`.
      Check hard-link members that point outside the directory.
- [ ] A failed download and a checksum mismatch both fall back to this
      machine's files on the same platform. Decide whether a mismatch
      should stop instead.

## Deferred review findings

- [ ] The installer's `login_finds` works only when called in a condition
      (`if`/`||`): under `set -e` a failing probe would end the installer.
      It also leaves an orphan `sleep` behind when it kills a slow shell,
      and sets globals (`probe`, `out`, `status`, `last`, …) that the rest
      of the script could trip over.
- [ ] `Staged::from_local` looks for `../share/man` beside the exe path as
      given, not the resolved one, so a symlinked `irsync` finds the pages
      only if they sit beside the link.
- [ ] No direct tests through `setup::run` for: the pid-0 guard, the
      `is_old_bin(&exe)` guard, an unreadable service file, and a home
      that is a symlink.
- [ ] Untested: the one-line fallback notice ("no release files for this
      build"), and the `--no-install` path on a recheck (a saved host whose
      daemon did not answer).
- [ ] The summary says what the client sent, not what the installer
      placed: with a man directory it cannot write to, the summary still
      says "with the manual pages".
- [ ] A half install (irsync placed, fileset not) is reported as current
      on the next run, which then never places fileset.
- [ ] Over ssh, the installer's "installed without fileset: … has none"
      lines name the work directory (`~/.irsync-install.XXXXXX/files`),
      which is gone by the time the user reads it.
- [ ] When a downloaded release lacks fileset or the pages, the summary
      blames "this machine" ("this machine has none beside its irsync").
- [ ] A FreeBSD client's tar (bsdtar) still writes extended attribute
      records into the files it sends: `--no-xattrs` is passed only on
      macOS, because BusyBox tar and GNU tar before 1.27 refuse it. A
      server whose tar does not know these records may print warnings.
      Not tried on a real FreeBSD client.
- [ ] `cargo publish` of the irsync crate would not build: the built-in
      installer is read from `packaging/installer/irsync-installer.sh.in`,
      outside the crate's directory, and a published crate holds only that
      directory. When publishing matters, keep a copy of the template
      inside `crates/irsync/`, with a test that it matches the one under
      `packaging/`. Publishing also waits on the iroh pin in `Cargo.toml`
      (`[patch.crates-io]`), which crates.io ignores.

# Open items from the supply chain work, 2026-11-15

What the supply chain work (`docs/SUPPLY_CHAIN_DECISIONS.md`) left open:
decisions, checks it couldn't run, and the minor findings of the final
review, deferred.

## Decisions

None open.

## Checks not yet run

- [ ] Check an `.rpm` signed by the signing subkey with `rpm -Kv` on Fedora and
      on the oldest supported RHEL, after `rpm --import` of `irsync.asc`. The
      beta.3 dry run checked the apt metadata (good signature by subkey
      `561F2581…B9D0`), but not the `.rpm` files, and `packages.yml` doesn't
      check them either. Do it before the first stable release.
- [ ] `itemize_over_a_slow_link_stays_within_the_out_budget` (backpressure)
      fails now and then on ubuntu-latest with "daemon session did not
      finish": 2026-11-12 three times, 2026-11-15 once, each passing on a
      re-run. The release gate runs it too, so a flake stops a release.

## Deferred review findings

- [ ] `[dist.github-action-commits]` in `dist-workspace.toml` doesn't follow
      Dependabot: when it bumps checkout or the artifact Actions elsewhere,
      `release.yml` stays on the old commits. A policy test that compares
      the SHAs would flag the drift on the Dependabot PR.
- [ ] `tools/github_settings.py` edges:
    - [ ] The `release` reviewer is whoever runs `apply`. A second
          maintainer running it would replace the first. Keep the list in
          the file instead.
    - [ ] `two_factor_requirement_enabled` is visible only to org owners;
          for anyone else the check wrongly reports 2FA as off.
    - [ ] The required `CI result` check has no `integration_id`, so any app
          with write access could post it. Pin it to GitHub Actions (15368).
    - [ ] `repos/{repo}/rulesets` lists org rulesets too; one with the same
          name as a repo ruleset would make `apply` PUT to the wrong ID.
- [ ] A tag on any commit, not only on `main`, can start a release, and a
      pre-release is public before any approval. Fine while there's one
      maintainer. For the group phase, add a tag ruleset with a `creation`
      rule and a maintainer bypass (see `book/src/contributing/supply-chain.md`,
      "When more maintainers join").
- [ ] Releases tagged before 2026-11-15 can't be finished with the resume
      workflow: their copy requires `main`, the new one refuses it. Only
      pre-releases exist from then, so nothing needs it today.
- [ ] `book/src/how-to/verify-a-download.md` calls the tap `irsync-dev/tap`
      (Homebrew's short name). The repository is `irsync-dev/homebrew-tap`.

# Open items from the CI speed-up, 2026-11-15

Pull request #21 cut the build time (less debug info) and cancels a pull
request's superseded runs. What is left, for discussion. Figures are from
CI runs of 2026-11-15, before #21: about 12 minutes a run on Linux
(4 cores) and 11 on macOS (3 cores), of which about 7 and 6 minutes are
running the tests. "Test time" below is the sum over tests, which the
runner divides by its cores.

## Checks not yet run

- [ ] The build saving on GitHub: measured only locally (a rebuild went
      from 54 s to 16 s). It shows from the second run of #21 on, once the
      cache is warm. The estimate is about 2 minutes a job.
- [ ] The cancelling: push twice to a pull request and see the first run
      cancelled.

## Tests that wait on real timeouts

- [ ] `onboard_e2e` takes about 410 s of test time on every machine, fast
      or slow, because it waits on production timeouts. On macOS that is
      36% of all test time. Five tests dial a dead daemon and wait out the
      60 s connection timeout (`net/endpoint.rs`, `max_idle_timeout`).
      Four more take about 20 s each; the 15 s `SSH_HOST_FIRST_DIAL` in
      `main.rs` is the likely cause, not traced. Option: let tests shorten
      these through a test-only setting, as `IRSYNC_TEST_RELEASE_URL` does
      for the release URL. Estimated saving: 1.5 minutes on Linux, 2 on
      macOS. Cost: these tests no longer run with the real values.

## Heavy tests

- [ ] `backpressure` and `blocking_pool` take 515 s of test time on Linux
      (macOS CI already skips them). Options: smaller data in the four or
      five heaviest tests, or run them only on `main`. Smaller data needs
      care: the tests must still fill the queues they are about. Estimated
      saving: up to 2 minutes on Linux.
- [ ] `itemize_over_a_slow_link_stays_within_the_out_budget` took 128 s
      and failed the `main` run of 2026-11-15 03:55 UTC with "daemon
      session did not finish". Flaky under load; not looked into.
- [ ] `concurrent_pulls_finish_when_their_walks_could_fill_the_blocking_pool`
      took 57 s on Linux CI against its own 60 s limit (21 s locally). One
      slow runner away from failing.
- [ ] The five netstress tests take 55 to 62 s each on Linux CI and 15 to
      21 s locally. They probably all wait about 40 s for the nested fault
      build; inferred from the timings, not measured. #21 should shorten
      that build too. Look again after it lands.

## Runner use

- [ ] Every merge runs the whole suite twice, on the pull request and
      again on the push to `main`, macOS included. Option: run macOS on
      only one of the two. On `main` only, a macOS-only failure shows up
      after the merge instead of before.

# Open items from the FreeBSD work, 2026-11-16

irsync builds and passes its tests on FreeBSD 14 (x86_64), in a workflow
started by hand (`.github/workflows/freebsd.yml`; see "Testing a version on
FreeBSD" in `book/src/contributing/releasing.md`). What is left. The design
is `docs/superpowers/specs/2026-11-16-freebsd-manual-build-design.md`.

## irsync on FreeBSD

- [ ] Socket files cannot be created on FreeBSD. `create_special`
      (`crates/irsync/src/fs/meta.rs`) makes them with `mknod`, which
      FreeBSD only allows for device nodes (`EINVAL`). A sync of a tree
      that holds a socket file reports an error for that entry. FIFOs had
      the same problem and now use `mkfifo`; a socket needs `bind`. No
      test creates a socket, so nothing fails today.
- [ ] Setting up a FreeBSD server over ssh is unchecked. irsync now
      recognises the name "FreeBSD", so a FreeBSD client can copy its own
      binary to a FreeBSD server. But the service hint it prints is the
      Linux (systemd) one (`crates/irsync/src/ssh/onboard.rs` only knows
      macOS apart from that); FreeBSD wants an `rc.d` script. From Linux
      or macOS, setup stops with "no release binary for that platform",
      because no FreeBSD binary is released.
- [ ] The open-file count in the usage report is a few too high on
      FreeBSD (8 against 5 in a test run). The `sysinfo` crate also counts
      the current folder, the root folder and the program's binary there.
      Accepted for now; the test in `telemetry/process.rs` allows up to
      five too many on FreeBSD. Fix it in `sysinfo` (report it upstream),
      or ask the kernel directly (`kern.proc.nfds`).

## Releases and packaging

- [ ] FreeBSD as a fourth release target, so the tarball is on the release
      page, `curl | sh` works there and the ssh setup can download it.
      Needs a way for dist to build it, and a decision on whether a
      FreeBSD failure stops a release. Wait until a few betas have passed
      the hand-started run.
- [ ] A port in FreeBSD's ports collection. A port builds with the ports
      tree's own Rust, not the one in `rust-toolchain.toml`, with no
      network and vendored crates. None of that has been tried.
- [ ] arm64 FreeBSD: not built or tested.
- [ ] The tarball is built on the newest 14.x (14.5 today). It should run
      on 15; nobody has tried it on an older 14.x.

## Checks not yet run

- [ ] Starting the workflow by hand, and its `ref` and `freebsd` inputs.
      It was tried through a scratch branch with a temporary push
      trigger, because a hand-started workflow only exists once it is on
      `main`. After the merge: `gh workflow run freebsd.yml`, then once
      more with `-f ref=<a tag>`.
- [ ] A version from before the workflow: the run should stop with "This
      version has no FreeBSD build". Try it with `-f ref=v0.1.0-beta.3`.

## Tests

- [ ] Timing-sensitive tests sometimes fail on a slow FreeBSD VM. Of nine
      full runs, five took 550 to 640 s of test time and passed. Four
      took over 800 s: one passed, three failed with "connection lost":
      `netsim::a_dead_zone_forces_a_reconnect_and_the_session_resumes` in
      all three, and in one of them also
      `netsim::single_file_push_resumes_after_blackout` and
      `backpressure::the_daemon_waits_for_a_slow_client_to_finish_reading`.
      The two netsim tests run on tokio's paused clock, so a slow machine
      should not matter to them; why it does is not understood. A guess,
      not checked: work on other threads takes real time while the paused
      clock jumps ahead, and the reconnect budget runs out. Until this is
      fixed, a red FreeBSD run with only these tests means "run it again".
- [ ] `posix_extras::links_to_files_placed_before_a_drop_are_checked`
      failed once on Linux in a full local run (3 link checks instead of
      4, pull) and passed 60 times alone and in every other full run. It
      counts frames around a dropped connection, so it depends on timing.
- [ ] A FreeBSD run builds everything from scratch: about 25 minutes, of
      which 4 are building cargo-nextest. The VM action can cache the
      prepared VM (`cache-after-prepare`); not tried.
- [ ] The FreeBSD run keeps its temp files in the test user's home, not
      `/tmp`: new files take their folder's group on FreeBSD, and `/tmp`
      belongs to `wheel`. Whether that slows the tests was not measured.

## Deferred review findings

- [ ] `tools/ci.sh` with no job on FreeBSD arm64 adds `freebsd-build`,
      which then refuses to run and stops everything.
- [ ] `test_no_workflow_calls_it` only looks for the text `freebsd.yml`.
      A FreeBSD job added straight into `ci.yml` would pass it; it could
      also check that no other workflow uses `vmactions/freebsd-vm`.
- [ ] A comment in `freebsd.yml` says a binary built on "the oldest
      supported release" runs on newer ones, but `14` picks the newest
      14.x.

# Open items from the progress rates, 2026-11-17

The `-P` transfer line shows current `sent` and `synced` rates and goodput
(#25, PR #69). The design is
`docs/superpowers/specs/2026-11-17-progress-rate-design.md`.

- [ ] A file's matched bytes can count twice toward `synced` and goodput.
      Each row holds its share of `Live::matched` (`matched_counted` in
      `crates/irsync/src/cli/progress/state.rs`), so a later `FileMatched`
      replaces it. But a row dropped after `ROW_EXPIRY` (30 s without an
      event) loses that share, and if the file then carries on, its next
      `FileMatched` adds the whole amount again. Rare; the figures stay
      high for the rest of the session.
- [ ] The `synced` rate jumps when a large, mostly unchanged file starts.
      `FileMatched` arrives when the file's chunks are requested, before
      any data moves, so a 5 GB file with little to send shows about
      1 GB/s synced for the 5-second window, then drops. Documented in
      the Output section of `book/src/reference/command-line.md`.
      Smoothing it (for example, adding the matched bytes as the file's
      position advances) is a design change.
