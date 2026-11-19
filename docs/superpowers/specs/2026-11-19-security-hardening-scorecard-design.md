# Close the real security gaps, then add OpenSSF Scorecard

Issue: https://github.com/irsync-dev/irsync/issues/124
Follow-up (not part of this): https://github.com/irsync-dev/irsync/issues/126

## Intent

The issue asks for OpenSSF Scorecard: a scheduled GitHub job that grades a
repository on about 18 security habits and gives a score out of 10.

The maintainer wants the score to describe the project, not to be the goal.
So the work first closes the gaps that matter for a tool that takes data
from other machines, and adds Scorecard last.

The supply chain side is already strong (pinned Actions, read-only tokens,
Dependabot, `cargo deny`, attested and signed releases, a ruleset on
`main`, a settings check). The gaps found on 2026-11-19 are:

1. `deny.toml` still ignores RUSTSEC-2026-0459 (SQLite), marked
   "TEMPORARY". `cargo deny --locked check advisories` now warns that no
   crate matches it: `Cargo.lock` has rusqlite 0.42.0 and libsqlite3-sys
   0.40.0. The exception is stale.
2. Nothing fuzzes the code that reads untrusted input. (Fuzzing: feeding a
   parser huge amounts of random and hostile input to find crashes.)
3. No static analysis and no code scanning. About 17 `unsafe` blocks ship
   (`crates/irsync/src/fs/clone.rs`, `fs/meta.rs`, `session/reconnect.rs`,
   `telemetry/process.rs`, `ssh/setup.rs`, `index.rs`,
   `crates/fileset/src/cpu.rs`), checked by no tool.
4. GitHub's private vulnerability reporting is off. `SECURITY.md` gives
   only an email address.
5. Workflow leftovers: `ci.yml` has no top-level `permissions` block (each
   job has one), and it installs PyYAML with an unpinned `pip install`.

## Goals

- Each gap above is closed, in that order, by its own pull request.
- Every new check runs from `tools/ci.sh` where it can, so a local run and
  CI stay the same (the rule in `AGENTS.md`).
- Every new workflow follows the existing rules: Actions pinned by commit,
  least permissions, `persist-credentials: false`, covered by
  `test_workflow_policy.py`, passes `actionlint`, updated by Dependabot.
- The Scorecard workflow publishes its result, findings show under the
  repository's code scanning, and the README carries the badge.
- The last pull request closes #124.

## Non-goals

- A written threat model and a manual review of remote path handling.
  That is #126.
- Requiring an approving review on `main`. irsync has one maintainer; the
  ruleset stays at zero approvals.
- OSS-Fuzz or any hosted fuzzing service. Later, if at all.
- The OpenSSF Best Practices badge (a self-assessment form). Later.
- Changing `release.yml`. dist writes it; its piped installs of dist and
  rustup stay. (My call: the dist one is fetched from a fixed version URL,
  and the rustup one runs only in a container without cargo.)

## Anti-goals

- No change made only to raise the score. If a Scorecard check is low for
  a reason we accept, it stays low and the reason is written down.
- No fuzz or analysis job that makes a pull request slow or flaky. Long
  runs go on a schedule, not on every push.
- No new secret, and no token with more than it needs.

## Constraints

- All work goes through pull requests against `main`; `CI result` is the
  one required check, and it must need every CI job
  (`test_ci_result_needs_every_job`).
- The toolchain is pinned to stable 1.100.0 (`rust-toolchain.toml`).
  cargo-fuzz and Miri need nightly. Nightly must be pinned to a date and
  used only for those jobs.
- Tool versions live in one place and `tools/dev-setup.sh` installs them
  with checked hashes. New tools follow that.
- Two Scorecard checks stay low and we do not work around them:
  "Maintained" gives nothing to a repository younger than 90 days (created
  2026-11-08), and "Code-Review" needs a second reviewer.
- Scorecard only publishes results from a workflow that meets its rules:
  runs on the default branch, no top-level write permissions, the job has
  `id-token: write` and `security-events: write`, and only steps from its
  approved list of Actions. Read its current documentation before writing
  the workflow.

## Approaches

**A. Full, in risk order.** Chosen by the maintainer. One plan, six pull
requests: the SQLite exception, fuzzing, static analysis, reporting,
workflow leftovers, Scorecard. The biggest job, mostly because of fuzzing,
but it is the one that covers the gap most likely to hide a real bug.

**B. Lean.** The same without fuzzing, which becomes its own issue. Quick,
and scores well, but leaves the parsers untested against hostile input.

**C. Deep.** A, plus a threat model and a manual review of remote path
handling. The review is open-ended and easier after A has narrowed where
to look. Filed as #126.

Not considered further: Scorecard first, with its findings as the to-do
list. The maintainer wants it last.

## The six steps

Each is one pull request. Steps 1 to 5 mention #124 without closing it.

Merging (the maintainer's decision): the builder merges each pull request
once `CI result` is green, cleans up as `AGENTS.md` says (delete the
branch, back to an up-to-date `main`), and starts the next step from
there. No stacked branches. The one exception is anything that needs the
maintainer (see the end): the builder stops and asks.

### 1. Drop the stale SQLite exception

Remove the RUSTSEC-2026-0459 entry from `deny.toml` and any comment that
still describes the hold: `TODO.md` (around line 233) and
`docs/SUPPLY_CHAIN_DECISIONS.md` (around line 85). rusqlite has been at
0.42 since commit `e9a5a85`. Keep the test `lookups_are_never_re_prepared`
(`crates/irsync/tests/index.rs`) and bring its note in
`crates/irsync/src/index.rs` up to date.

Make a stale exception fail from now on: `cargo deny` reports it as
`advisory-not-detected`. Turn that warning into an error in the `deny` job
of `tools/ci.sh` and in `advisories.yml`, if cargo-deny at the pinned
version can. If it cannot, a small test that greps the output.

### 2. Fuzz the code that reads untrusted input

A `fuzz/` cargo-fuzz project at the repository root, outside the workspace
so the stable build never sees it. Targets, most exposed first:

- **Frames**: `read_body` and message decoding in
  `crates/irsync/src/net/frame.rs`, for each message type a peer can send.
- **Delta**: `delta::decode` in `crates/irsync/src/delta.rs`, with a
  fuzzer-chosen basis.
- **Glob**: compile and match in `crates/irsync-glob`, pattern and subject
  both from the fuzzer. Also checks that matching ends (no blow-up).
- **Fileset manifest**: open a fuzzer-written file as `.fileset.sqlite`
  through `fileset`'s normal open path.
- **Relative paths**: the constructor of `RelPath` and anything else that
  turns peer bytes into a path.

A target fails on a panic, a crash, a hang, or memory growing without
bound. Where a function has an inverse (encode then decode), the target
also checks the round trip.

The builder may add or drop a target after reading the code, and says so
in the pull request. If a function is private, prefer a small public
`#[doc(hidden)]` entry point or a `fuzzing` feature over copying code.

How it runs:

- `tools/ci.sh fuzz-build` builds every target and runs each for a few
  seconds on its checked-in seed inputs. It is a job in `ci.yml`, needed
  by `CI result`. It catches a target that no longer compiles.
- A new scheduled workflow, `fuzz.yml`, runs each target for a fixed time
  every night (start with 10 minutes each), keeps the corpus in the
  Actions cache, and uploads any crashing input as an artifact. A failure
  is a failed run, which GitHub emails about, as `advisories.yml` does.
- Every checked-in seed passes, always: `fuzz-build` is a required check.
  A crash found while building this step is a real bug. If the fix is
  small it goes in this pull request, with the input as a seed. If not,
  the fix gets its own pull request, merged first, and this one is
  rebased onto it. (My call.)
- A crash that looks exploitable is not put in a public issue or pull
  request. That target is left out of this pull request, the maintainer
  is told privately, and the other targets and steps 3 to 6 go on. The
  target comes back with the fix. (My call.)

The book's `contributing/testing.md` gets a short section: how to run a
target locally and how to add one. `tools/dev-setup.sh` installs
cargo-fuzz and the pinned nightly.

### 3. Static analysis

- **CodeQL** for Rust and for the workflows (`actions` language), as
  `codeql.yml`: on pull requests, pushes to `main`, and weekly. Findings
  go to code scanning. Not part of `CI result` at first; it becomes
  required once a few weeks show it is quiet. (My call.)
- **zizmor**, a linter for GitHub workflows, added to the `workflows` job
  of `tools/ci.sh` next to `actionlint`, pinned and installed by
  `dev-setup.sh`. Findings are fixed or, with a reason, ignored in its
  config file.
- **Miri**, which runs tests in an interpreter that catches undefined
  behaviour in `unsafe` code. Much of the `unsafe` here calls the
  operating system, which Miri cannot run. So: run Miri on the tests of
  the crates and modules where it works (start with `irsync-glob`,
  `irsync-proto`, `fileset`'s pure parts, `delta`), nightly in the same
  scheduled workflow as fuzzing or its own. The builder finds the set
  that works and lists what is left out and why.
- **Unsafe lints**: turn on `unsafe_op_in_unsafe_fn` and clippy's
  `undocumented_unsafe_blocks` for the workspace, and write the missing
  `// SAFETY:` comments. `#![forbid(unsafe_code)]` in every crate that has
  none today.

### 4. Reporting and the security policy

- Turn on private vulnerability reporting for the repository, and add it
  to the expected settings in `tools/github_settings.py` so the check
  reports it if it is turned off. The maintainer flips the setting; the
  pull request has the check and the docs.
- `SECURITY.md`: offer both ways to report (GitHub's "Report a
  vulnerability" button and the email), say that a report is answered within 7 days
  (the maintainer's number), and change the README's "alpha"
  badge to "beta": the version is `0.1.0-beta.5` and the README text
  says beta.
- `book/src/contributing/supply-chain.md` and
  `docs/SUPPLY_CHAIN_DECISIONS.md` get what steps 1 to 6 add.

### 5. Workflow leftovers

- A top-level `permissions: contents: read` in `ci.yml`, and a policy test
  that every workflow has a top-level block with no write permission
  (`release.yml`, written by dist, is the one listed exception).
- PyYAML installed at a pinned version with a hash
  (`pip install --require-hashes`), or from the Ubuntu archive like pandoc
  and minisign. The builder picks; the archive is simpler.
- Whatever zizmor (step 3) and a local Scorecard run still flag that is a
  real weakness. A local run: the `scorecard` command line tool against
  the repository with a read-only token.

### 6. Scorecard

- `scorecard.yml`: `ossf/scorecard-action`, on a weekly schedule, on push
  to `main`, and on `branch_protection_rule`. `publish_results: true`.
  Uploads its SARIF file to code scanning. Meets Scorecard's rules for
  publishing (see Constraints).
- Policy tests for it in `test_workflow_policy.py`, in the style of
  `test_advisories_daily`.
- The badge in `README.md`, after CI, linking to the result page.
- A short section in `supply-chain.md`: what Scorecard is, where to see
  the result, and the checks we accept as low with the reason for each
  (Maintained, Code-Review, and whatever else the first run shows:
  possibly CII-Best-Practices, Fuzzing if it does not recognise
  cargo-fuzz, Branch-Protection if it cannot read rulesets without an
  admin token).
- That section is written from the local Scorecard run of step 5, since
  the published result exists only after this pull request is merged.
  After the first run on `main`, read the published result. If it
  differs, a small follow-up pull request corrects the section; a real
  weakness becomes a fix or an issue. (My call.)
- `Closes #124`.

## Left to the builder

- The exact fuzz targets and their entry points, the nightly date, the
  fuzz time per target, and how the corpus is cached.
- Which tests Miri can run.
- Where zizmor and CodeQL are configured and which findings to ignore,
  each with a reason.
- Workflow names, cron times, job names.
- Whether steps 3 and 5 are better split or merged, if the diffs say so.

## Needs the maintainer

- Turning on private vulnerability reporting (step 4), and code scanning
  if it needs turning on.
- Looking at any fuzz crash that may be exploitable.
