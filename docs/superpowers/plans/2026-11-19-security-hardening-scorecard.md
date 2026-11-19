# Security Hardening and OpenSSF Scorecard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close irsync's real security gaps in risk order, then add OpenSSF Scorecard and close issue #124.

**Architecture:** Six pull requests, one per task, each merged before the next starts. Every new check is a job in `tools/ci.sh` that a workflow calls, so local runs and CI match. Rules about workflows are pinned by tests in `.github/scripts/tests/test_workflow_policy.py`; rules about repository settings by `tools/github_settings.py`.

**Tech Stack:** Rust 1.100.0 (stable) plus one date-pinned nightly for cargo-fuzz and Miri; cargo-deny 0.20.2; GitHub Actions; CodeQL; zizmor; `ossf/scorecard-action`; Python `unittest` for the script and policy tests.

**Spec:** `docs/superpowers/specs/2026-11-19-security-hardening-scorecard-design.md`. Read it first. Also read `AGENTS.md`.

## Global Constraints

- All work goes through a pull request against `main`. Never push to `main`.
- Tasks 1 to 5 say `For #124` in the pull request description. Only Task 6 says `Closes #124`.
- No attribution of any kind in commits, branches, pull requests or comments (no `Co-Authored-By`, no "Generated with").
- Commit messages: a short summary line, a blank line, then why, in plain English.
- Every third-party Action is pinned as `owner/repo@<40 hex> # vX.Y.Z` (`test_third_party_actions_pinned`). Resolve the SHA at build time with `gh api repos/OWNER/REPO/git/ref/tags/vX.Y.Z`; pick a release at least 7 days old (the Dependabot cooldown). Never invent a SHA.
- Every checkout step has `persist-credentials: false`. Every workflow has a top-level `permissions` block with no write permission; write permissions go on the one job that needs them.
- No `pull_request_target`. No new secret.
- `CI result` must need every job in `ci.yml` (`test_ci_result_needs_every_job`).
- Stable toolchain stays 1.100.0. Nightly is used only by the `fuzz-build`, `fuzz` and `miri` jobs, and is pinned in one place: `fuzz/rust-toolchain.toml`.
- Tool versions are pinned in `ci.yml`; `tools/dev-setup.sh` reads them from there (see how `nextest_pin` and `deny_pin` are read) and gets tests in `tools/tests/test_dev_setup.py`.
- Do not edit `.github/workflows/release.yml`. dist writes it.
- No change made only to raise the Scorecard score.
- Book and Markdown text: short sentences, plain words, in the style of `book/src/contributing/supply-chain.md`.

## How every task ends (the pull request loop)

Referred to below as "Ship it".

1. `git switch main && git pull`, then `git switch -c <branch>` before the first edit of the task.
2. Before pushing: `tools/ci.sh check` if any Rust code or anything under `tools/` changed; `tools/ci.sh workflows` if anything under `.github/` changed; neither for a Markdown-only change.
3. `git push -u origin <branch>`, then `gh pr create --base main` with a description that says what and why, and the issue line.
4. `gh pr checks --watch`. If `CI result` fails and the branch is behind `main`, follow the "CI fails on a PR whose branch is behind" rule in `AGENTS.md`. Otherwise fix it on the branch.
5. When `CI result` is green: `gh pr merge --merge --delete-branch`, `git switch main && git pull`, delete the local branch.
6. Stop and ask the maintainer instead of merging when the task says "Needs the maintainer".

## Review Focus

Things the spec implies that no task's own test would catch otherwise. Each has a step in the task named.

1. **A fuzz target that tests nothing.** A target whose entry point rejects every input at the first check finds no bugs and looks green. Expected: each target has valid seed inputs and its coverage grows past the seeds. (Task 2, Step 6.)
2. **A crash the nightly run finds but nobody can get.** Expected: the crashing input is uploaded and the run is red. Proved once with a planted crash. (Task 2, Step 9.)
3. **A docs-only pull request.** The `changes` job skips the builds. Expected: a skipped `fuzz-build` counts as passed in `CI result`. (Task 2, Step 8.)
4. **A developer without nightly or cargo-fuzz runs `tools/ci.sh`.** Expected: a clear "not found; tools/dev-setup.sh installs it" line, as for the other tools; on macOS and FreeBSD the job is left out with a message, like `release-build`. (Task 2, Step 7.)
5. **Scorecard refuses to publish.** The workflow can pass and still publish nothing if it breaks Scorecard's rules. Expected: the first run on `main` shows a published result at the public API. (Task 6, Step 6.)
6. **The release gate calls `ci.yml`.** A top-level `permissions` block in a called workflow must not ask for more than the caller grants. Expected: `release-gate.yml`'s `test` job still starts. (Task 5, Step 3.)

---

### Task 0: Land the design and this plan

**Files:**
- Already committed on branch `security-hardening-design`: the spec.
- Create: this file.

- [ ] **Step 1:** Commit this plan on `security-hardening-design`.
- [ ] **Step 2:** Ship it (Markdown only, so no local checks). Description: `For #124`.

---

### Task 1: Drop the stale SQLite exception

Branch: `deny-stale-exception`.

**Files:**
- Modify: `deny.toml` (the `ignore` list), `tools/ci.sh` (`job_deny`), `.github/workflows/advisories.yml`, `.github/scripts/tests/test_workflow_policy.py` (`test_advisories_daily`), `TODO.md` (about line 233), `docs/SUPPLY_CHAIN_DECISIONS.md` (about line 85), `crates/irsync/src/index.rs` (the note near line 513), `book/src/contributing/supply-chain.md`.

**Interfaces:**
- Produces: the deny commands are now `cargo deny --locked check -D advisory-not-detected` and `cargo deny --locked check advisories -D advisory-not-detected`.

- [ ] **Step 1: Write the failing tests.** In `test_workflow_policy.py`, change `test_advisories_daily` to expect the run line `cargo deny --locked check advisories -D advisory-not-detected`. Add next to `test_deny_job_present`:

```python
def test_stale_advisory_exception_fails(self):
    with open(os.path.join(ROOT, "tools", "ci.sh")) as f:
        self.assertIn("run cargo deny --locked check -D advisory-not-detected", f.read())
```

- [ ] **Step 2:** Run `python3 -m unittest discover -s .github/scripts/tests`. Expected: those two fail.
- [ ] **Step 3:** Add `-D advisory-not-detected` to `job_deny` in `tools/ci.sh` and to the run line in `advisories.yml`. Leave `job_deny_license_check` alone (it checks licenses only).
- [ ] **Step 4:** Run `tools/ci.sh deny`. Expected: FAIL, naming RUSTSEC-2026-0459 as not encountered. This proves the flag works.
- [ ] **Step 5:** Remove the RUSTSEC-2026-0459 entry and its comment from `deny.toml`. Run `tools/ci.sh deny`. Expected: pass. If another ignore is also reported stale, remove it too.
- [ ] **Step 6:** Remove or correct the text about the held-back SQLite fix in `TODO.md`, `docs/SUPPLY_CHAIN_DECISIONS.md` and the note in `crates/irsync/src/index.rs`. Keep the test `lookups_are_never_re_prepared`. In `supply-chain.md`, add one sentence under "A dependency goes bad": an exception that no longer matches anything fails the check.
- [ ] **Step 7:** Run `tools/ci.sh workflows deny`, then `tools/ci.sh check` (a Rust file changed). Expected: pass.
- [ ] **Step 8:** Commit and Ship it.

---

### Task 2: Fuzz the code that reads untrusted input

Branch: `fuzz-targets`. The largest task.

**Files:**
- Create: `fuzz/Cargo.toml`, `fuzz/Cargo.lock`, `fuzz/rust-toolchain.toml`, `fuzz/.gitignore` (`target`, `artifacts`, `coverage`, and `corpus/` except what Step 6 checks in under `fuzz/seeds/`), `fuzz/fuzz_targets/{frame,delta,glob,manifest,relpath}.rs`, `fuzz/seeds/<target>/*`, `.github/workflows/nightly.yml`.
- Modify: `Cargo.toml` (`exclude = ["fuzz"]` under `[workspace]`), `tools/ci.sh`, `.github/workflows/ci.yml`, `tools/dev-setup.sh`, `tools/tests/test_ci_script.py`, `tools/tests/test_dev_setup.py`, `.github/scripts/tests/test_workflow_policy.py`, `book/src/contributing/testing.md`, `AGENTS.md` (the Tests section), `.github/dependabot.yml` (a second `cargo` entry for `/fuzz`, same schedule, cooldown and grouping; update `test_dependabot_config` to allow two cargo entries).

**Interfaces:**
- Produces:
  - `fuzz/rust-toolchain.toml` with `channel = "nightly-YYYY-MM-DD"`: the one nightly pin. Read it with `sed -n 's/^channel = "\(.*\)"/\1/p' fuzz/rust-toolchain.toml`.
  - `tools/ci.sh fuzz-build`: builds every target, then runs each on its seeds only (`cargo +$nightly fuzz run <target> fuzz/seeds/<target> -- -runs=0`).
  - `tools/ci.sh fuzz`: runs each target for `FUZZ_SECONDS` seconds (default 60) with corpus dir `fuzz/corpus/<target>` and the seeds dir as a second, read-only corpus; `-max_total_time=$FUZZ_SECONDS -timeout=10 -rss_limit_mb=2048`. Exits non-zero if any target fails, after running them all.
  - `cargo-fuzz@X.Y.Z` pinned in `ci.yml`, read by `dev-setup.sh` like `deny_pin`.

**Targets** (entry points are already public):

| Target | Calls | Checks beyond "no crash" |
|---|---|---|
| `frame` | `irsync::net::frame::read_body` on the input as a stream, then decodes the body as each `irsync_proto` message type a peer can send | a decoded message re-encodes and decodes to the same value |
| `delta` | `irsync::delta::decode`; input split into basis and ops with `arbitrary` | `decode(encode(new, basis), basis) == new` for fuzzer-chosen `new` and `basis` |
| `glob` | `irsync_glob::Glob::new` then `matches`; input split into pattern and subject | returns within the 10 s timeout |
| `manifest` | writes the input to a temp file named `.fileset.sqlite` and opens it through `fileset`'s normal open path, then reads every entry | none |
| `relpath` | `RelPath::new` | an accepted path has no `..` component, is not absolute and has no empty component |

Read each entry point first. If the crate layout makes one unreachable from `fuzz/` (a binary-only crate, a private module), add a `#[doc(hidden)] pub` entry point rather than copying code. You may add or drop a target; say so in the pull request.

- [ ] **Step 1: Pin the nightly and cargo-fuzz.** Pick a nightly at least 7 days old on which `cargo fuzz build` and `cargo miri setup` both work. Write `fuzz/rust-toolchain.toml` (components: `rust-src`, `miri`, `llvm-tools-preview`). Add `exclude = ["fuzz"]` to the root workspace. Run `cargo build --workspace` on stable. Expected: unchanged, no nightly needed.
- [ ] **Step 2: Scaffold `fuzz/`** with `cargo fuzz init`-style layout: `libfuzzer-sys`, `arbitrary` with `derive`, path dependencies on the crates under test, `[workspace]` empty table. Commit `fuzz/Cargo.lock`.
- [ ] **Step 3: Write the `relpath` target and its seeds first** (the smallest), run `cargo +$nightly fuzz run relpath -- -max_total_time=60`. Expected: runs, no crash, or a crash to handle as in Step 5.
- [ ] **Step 4: Write the other four targets**, one commit each, each run for 60 seconds locally.
- [ ] **Step 5: Handle every crash as the spec says.** Small fix: fix it here with a normal regression test in the crate's own tests, and add the input to `fuzz/seeds/<target>/`. Larger fix: its own branch and pull request, shipped first; then rebase this branch. Looks exploitable (memory unsafety, a write outside the destination tree, unbounded memory from a small input): **Needs the maintainer.** Do not push the input or describe it publicly; leave that target out of this pull request, tell the maintainer in the session, and go on.
- [ ] **Step 6: Seeds that reach the code (Review Focus 1).** For each target check in at least three valid inputs under `fuzz/seeds/<target>/`: for `frame`, real encoded messages; for `manifest`, a real small `.fileset.sqlite` made by `fileset`; for `delta`, a real encoded delta. Then run each target for 60 s and confirm in libFuzzer's output that `cov:` rises above its value after the seeds were loaded. A target whose coverage does not rise is not done.
- [ ] **Step 7: Add `fuzz-build` and `fuzz` to `tools/ci.sh`.** Tests first, in `tools/tests/test_ci_script.py`, in the style of `test_freebsd_build_refuses_elsewhere`:

```python
def test_help_lists_fuzz_jobs(self):          # usage text names fuzz-build and fuzz
def test_fuzz_build_needs_cargo_fuzz(self):   # without cargo-fuzz on PATH: exit != 0,
                                              # stderr has "cargo-fuzz not found; tools/dev-setup.sh"
def test_fuzz_seconds_reaches_libfuzzer(self):# FUZZ_SECONDS=7 -> "-max_total_time=7" in the printed command
```

  Then: add both names to the usage comment and the `case`; add `needs_fuzz_build` / `needs_fuzz` (cargo-fuzz, and the pinned nightly via `rustup run $nightly rustc --version`); add `fuzz-build` to `all_jobs` on Linux only, with the "leaving out" message elsewhere (Review Focus 4). `fuzz` is never in `all_jobs`: it runs when named.
- [ ] **Step 8: Add the `fuzz-build` job to `ci.yml`** after `deny`: `needs: changes`, `if: needs.changes.outputs.code == 'true'`, `permissions: contents: read`, checkout with `persist-credentials: false`, install the nightly from `fuzz/rust-toolchain.toml`, install `cargo-fuzz@X.Y.Z` with `taiki-e/install-action`, `Swatinem/rust-cache` with `workspaces: fuzz`, run `tools/ci.sh fuzz-build`. Add `fuzz-build` to `ci-result`'s `needs`. Confirm `.github/scripts/ci-result.sh` treats `skipped` as passed (Review Focus 3); if it has a test file, add the `fuzz-build: skipped` case to it. Add the `cargo-fuzz` pin to `dev-setup.sh` with tests like those for cargo-deny.
- [ ] **Step 9: Add `nightly.yml`**, job `fuzz`. Policy test first:

```python
def test_nightly_fuzz(self):
    wf = load_workflows()["nightly.yml"]
    on = triggers(wf)
    self.assertTrue(on["schedule"][0]["cron"])
    self.assertIn("workflow_dispatch", on)
    self.assertEqual(wf["permissions"], {"contents": "read"})
    text = job_text("nightly.yml", "fuzz")
    self.assertIn("tools/ci.sh fuzz", text)
    self.assertIn("FUZZ_SECONDS: 600", text)
    self.assertIn("if: failure()", text)        # the crash upload
    self.assertIn("fuzz/artifacts", text)
```

  The job: restore `fuzz/corpus` with `actions/cache` (key by run id, restore-keys by prefix, so the corpus carries over), run `tools/ci.sh fuzz` with `FUZZ_SECONDS: 600`, and on failure upload `fuzz/artifacts` with `actions/upload-artifact`. `timeout-minutes: 90`. A cron time away from the hour.
  **Prove it (Review Focus 2):** on the branch, temporarily add `if data.starts_with(b"PLANTED") { panic!() }` to `relpath` and a seed `PLANTED`, push, start the workflow by hand on the branch (`gh workflow run nightly.yml --ref fuzz-targets`), and confirm the run is red and the artifact holds the input. Then remove the plant and the seed in a later commit. Say in the pull request that this was done, with the run link.
- [ ] **Step 10: Docs.** `book/src/contributing/testing.md`: a "Fuzzing" section (what is fuzzed, `tools/ci.sh fuzz-build`, `FUZZ_SECONDS=300 tools/ci.sh fuzz`, running one target, adding a target and its seeds, what to do with a crash including the private path for an exploitable one). `AGENTS.md`: `fuzz-build` in the list of `tools/ci.sh` jobs and in the dev-setup tool list.
- [ ] **Step 11:** Run `tools/ci.sh workflows check fuzz-build`. Expected: pass. Commit and Ship it.

---

### Task 3: Static analysis

Branch: `static-analysis`.

**Files:**
- Create: `.github/workflows/codeql.yml`, `.github/zizmor.yml` (only if an ignore is needed).
- Modify: `tools/ci.sh` (`job_workflows`, `needs_workflows`, new `job_miri`), `.github/workflows/ci.yml` (zizmor install in `workflows`), `.github/workflows/nightly.yml` (job `miri`), `tools/dev-setup.sh` and its tests, `Cargo.toml` (`[workspace.lints]`), every `crates/*/Cargo.toml` (`[lints] workspace = true`), `crates/{irsync-glob,irsync-proto,netstress}/src/lib.rs` or `main.rs` (`#![forbid(unsafe_code)]`), the files with `unsafe` blocks (SAFETY comments), `.github/scripts/tests/test_workflow_policy.py`, `tools/tests/test_ci_script.py`, `book/src/contributing/testing.md`.

**Interfaces:**
- Consumes: the nightly pin in `fuzz/rust-toolchain.toml` and `nightly.yml` from Task 2.
- Produces: `tools/ci.sh workflows` also runs `zizmor .github/workflows`; `tools/ci.sh miri` runs Miri on the crates in a `miri_packages` list in `ci.sh`.

- [ ] **Step 1: Unsafe lints.** In the root `Cargo.toml`:

```toml
[workspace.lints.rust]
unsafe_op_in_unsafe_fn = "deny"

[workspace.lints.clippy]
undocumented_unsafe_blocks = "deny"
```

  `[lints] workspace = true` in every crate. Run `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`. Expected: fails on each undocumented block.
- [ ] **Step 2:** Write a `// SAFETY:` comment for every reported block, saying why that call is sound (which invariant, who upholds it). Read the code; do not write a generic line. If a block turns out not to be sound, that is a finding: fix it with a test, or if it looks exploitable, **Needs the maintainer** as in Task 2 Step 5. Add `#![forbid(unsafe_code)]` to `irsync-glob`, `irsync-proto` and `netstress` (none has `unsafe` today; if generated code in `irsync-proto` objects, use `deny` there and say why). Run both clippy lines from `job_check`. Expected: pass. Commit.
- [ ] **Step 3: zizmor.** Tests first: in `test_workflow_policy.py`, assert `ci.sh` contains `run zizmor` and that the `workflows` job in `ci.yml` installs a pinned zizmor (a `ZIZMOR_VERSION` and hash like `ACTIONLINT_*`, or `taiki-e/install-action` with `tool: zizmor@X.Y.Z` if it offers it); in `test_dev_setup.py`, the pin is read from `ci.yml`. Then implement: `need zizmor zizmor --version` in `needs_workflows`, `run zizmor --offline .github/workflows` in `job_workflows`. Fix what it reports. Ignore a finding only in `.github/zizmor.yml`, each with a one-line reason; `release.yml` findings are ignored as "written by dist". Leftovers that are real but belong to Task 5 (PyYAML, top-level permissions) may be fixed here if zizmor fails on them; then Task 5 shrinks. Commit.
- [ ] **Step 4: CodeQL.** Policy test first:

```python
def test_codeql(self):
    wf = load_workflows()["codeql.yml"]
    on = triggers(wf)
    self.assertIn("pull_request", on)
    self.assertEqual(on["push"]["branches"], ["main"])
    self.assertTrue(on["schedule"][0]["cron"])
    self.assertEqual(wf["permissions"], {"contents": "read"})
    job = next(iter(jobs(wf).values()))
    self.assertEqual(job["permissions"], {"contents": "read", "security-events": "write"})
    self.assertEqual(sorted(job["strategy"]["matrix"]["language"]), ["actions", "rust"])
```

  Then `codeql.yml`: `github/codeql-action/init` and `/analyze`, pinned, `build-mode: none`. It is not a job in `ci.yml` and not needed by `CI result`. If the repository needs code scanning switched on first: **Needs the maintainer.**
- [ ] **Step 5: Miri.** Test in `test_ci_script.py` that the usage text names `miri` and that it needs the pinned nightly. Add `job_miri`: `cargo +$nightly miri test -p <crate>` for each crate in `miri_packages`. Start with `irsync-glob` and `irsync-proto`; try `fileset` and `irsync --lib` with a test filter for `delta` and add what passes in reasonable time (under 15 minutes in total). Above the list in `ci.sh`, a comment naming what is left out and why (system calls Miri cannot run). `miri` is never in `all_jobs`. Add job `miri` to `nightly.yml` running `tools/ci.sh miri`, and extend `test_nightly_fuzz`'s class with a `test_nightly_miri` asserting `tools/ci.sh miri` is in that job's text.
- [ ] **Step 6:** A short "Static analysis" section in `testing.md`: the three tools, how to run zizmor and Miri locally, where CodeQL's findings show.
- [ ] **Step 7:** Run `tools/ci.sh workflows check miri`. Expected: pass. Commit and Ship it. After the merge, look at the first CodeQL run on `main`; a real finding becomes a fix or an issue (private if exploitable).

---

### Task 4: Reporting and the security policy

Branch: `vulnerability-reporting`.

**Files:**
- Modify: `tools/github_settings.py`, `tools/tests/test_github_settings.py`, `SECURITY.md`, `README.md` (the status badge), `book/src/contributing/supply-chain.md`, `docs/SUPPLY_CHAIN_DECISIONS.md`.

**Interfaces:**
- Produces: `desired()[MAIN]["private_vulnerability_reporting"] = True`; `fetch(repo)` returns `state["private_vulnerability_reporting"]: bool`, read from `repos/{repo}/private-vulnerability-reporting` (`{"enabled": bool}`); `apply` turns it on with `PUT` on the same path.

- [ ] **Step 1: Failing tests** in `test_github_settings.py`, in the style of the existing `immutable_releases` tests: `desired()` has the key for `MAIN` only; `diff` reports it when `have` is `False`; `apply` issues the `PUT`.
- [ ] **Step 2:** Run `python3 -m unittest discover -s tools/tests -k github_settings`. Expected: fail.
- [ ] **Step 3:** Implement in `github_settings.py`. Run again. Expected: pass.
- [ ] **Step 4: `SECURITY.md`.** Under "Report a vulnerability": two ways, GitHub's "Report a vulnerability" button on the repository's Security tab, or email `security@irsync.org`; "We answer within 7 days." Keep the rest. "Supported versions" keeps "beta".
- [ ] **Step 5: `README.md`:** the status badge becomes `status-beta`. `supply-chain.md`: a short "Someone finds a vulnerability" part (the two ways to report, the 7 days, the settings check). `SUPPLY_CHAIN_DECISIONS.md`: the decisions of Tasks 1 to 4, one entry each, in that file's existing form.
- [ ] **Step 6:** Run `tools/ci.sh check` (a file under `tools/` changed). Commit, push, open the pull request.
- [ ] **Step 7: Needs the maintainer.** Ask them to run `tools/github_settings.py apply` (it needs an org admin), or to switch on "Private vulnerability reporting" by hand. Then run `tools/github_settings.py check`. Expected: no difference. Merge only after that, so `SECURITY.md` never points at a button that is not there.

---

### Task 5: Workflow leftovers

Branch: `workflow-leftovers`. Skip any step Task 3 already did.

**Files:**
- Modify: `.github/workflows/ci.yml`, `.github/scripts/tests/test_workflow_policy.py`, `tools/dev-setup.sh` (the PyYAML hint) and its test, `book/src/contributing/supply-chain.md`.

- [ ] **Step 1: Failing policy tests.**

```python
def test_top_level_permissions_read_only(self):
    for name, wf in load_workflows().items():
        if name == "release.yml":      # written by dist
            continue
        perms = wf.get("permissions")
        self.assertIsInstance(perms, dict, f"{name}: no top-level permissions block")
        self.assertNotIn("write", perms.values(), name)

def test_no_unpinned_pip_install(self):
    for name in workflow_files():
        self.assertNotRegex(read(name), r"pip3? install(?!.*--require-hashes)", name)
```

- [ ] **Step 2:** Run the policy tests. Expected: both fail on `ci.yml`.
- [ ] **Step 3:** Add top-level `permissions: contents: read` to `ci.yml`. Install PyYAML with `sudo apt-get install -y python3-yaml` in the same step as minisign (one `apt-get update`). Check Review Focus 6: `release-gate.yml`'s `test` job grants `contents: read`, so the called workflow asks for no more; `actionlint` must pass, and the job comment says why the block is safe for the caller.
- [ ] **Step 4: A local Scorecard run.** Install the `scorecard` command line tool at a pinned release into the scratch directory (not the repository), run `GITHUB_AUTH_TOKEN=$(gh auth token) scorecard --repo=github.com/irsync-dev/irsync --format=json --show-details`, and save the output outside the repository. For each check below 10: is it a real weakness? Fix real ones here if small, or file an issue. List the rest with the reason; Task 6 needs that list. `Maintained` and `Code-Review` are accepted (spec, Constraints). Note that the local run sees `main`, so it already includes Tasks 1 to 4.
- [ ] **Step 5:** Put the list from Step 4 into the pull request description. Run `tools/ci.sh workflows`. Commit and Ship it.

---

### Task 6: Scorecard

Branch: `scorecard`.

**Files:**
- Create: `.github/workflows/scorecard.yml`.
- Modify: `.github/scripts/tests/test_workflow_policy.py`, `README.md`, `book/src/contributing/supply-chain.md`, `docs/SUPPLY_CHAIN_DECISIONS.md`, `SECURITY.md` (one line linking the result).

- [ ] **Step 1: Read the current README of `ossf/scorecard-action`**, the part on `publish_results` and its workflow restrictions. They are strict (which Actions a job may use, no top-level write permission, which triggers) and change between versions. Write the workflow to those rules, not from memory.
- [ ] **Step 2: Failing policy test.**

```python
def test_scorecard(self):
    wf = load_workflows()["scorecard.yml"]
    on = triggers(wf)
    self.assertEqual(on["push"]["branches"], ["main"])
    self.assertTrue(on["schedule"][0]["cron"])
    self.assertIn("branch_protection_rule", on)
    self.assertEqual(wf["permissions"], "read-all")
    job = next(iter(jobs(wf).values()))
    self.assertEqual(job["permissions"],
                     {"security-events": "write", "id-token": "write"})
    steps = job["steps"]
    card = next(s for s in steps if step_uses(s, "ossf/scorecard-action"))
    self.assertIs(card["with"]["publish_results"], True)
    self.assertTrue(any(step_uses(s, "github/codeql-action/upload-sarif") for s in steps))
    self.assertFalse(any("run" in s for s in steps), "Scorecard allows no run steps here")
```

  `read-all` at the top is what Scorecard's own template uses; if Step 1 shows `contents: read` is enough, use that and change the test, and make `test_top_level_permissions_read_only` (Task 5) accept the string `read-all`. Drop the last assertion if Step 1 shows `run` steps are allowed.
- [ ] **Step 3:** Write `scorecard.yml`: checkout (`persist-credentials: false`), `ossf/scorecard-action` with `results_file: results.sarif`, `results_format: sarif`, `publish_results: true`, then `github/codeql-action/upload-sarif` at the same pin as `codeql.yml`. Weekly cron. Run `tools/ci.sh workflows`. Expected: pass.
- [ ] **Step 4: `README.md`:** after the CI badge:

```markdown
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/irsync-dev/irsync/badge)](https://scorecard.dev/viewer/?uri=github.com/irsync-dev/irsync)
```

- [ ] **Step 5: `supply-chain.md`:** a section "The Scorecard": what it is in two sentences, where the result is, and a table of the checks we accept as low with the reason for each, from Task 5 Step 4. `SUPPLY_CHAIN_DECISIONS.md`: the decision to add it last and not to chase the score.
- [ ] **Step 6:** Commit and Ship it, with `Closes #124`. After the merge, wait for the first run on `main` (Review Focus 5): `gh run list --workflow scorecard.yml --limit 1` is green, and `curl -s https://api.scorecard.dev/projects/github.com/irsync-dev/irsync` returns a JSON result with a score. If it publishes nothing, read the run's log, fix the workflow in a follow-up pull request.
- [ ] **Step 7:** Compare the published checks with the table from Step 5. If they differ, a small follow-up pull request corrects the table; a real weakness becomes a fix or an issue. Report the final score and the accepted-low list to the maintainer.

---

## What needs the maintainer, in one place

- Task 2 Step 5 and Task 3 Step 2: any finding that looks exploitable.
- Task 3 Step 4: switching on code scanning, if GitHub asks.
- Task 4 Step 7: `tools/github_settings.py apply`, before that pull request is merged.
