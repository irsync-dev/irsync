# A release branch for every release line: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** every release (alpha, beta, stable) is cut from `release-X.Y`; a new workflow creates or rebases that branch onto `main` before each alpha, beta and `X.Y.0`.

**Architecture:** the rules live in small bash scripts under `.github/scripts/` (as today), tested from `.github/scripts/tests/test_release_scripts.py` with the fake `gh` and, for git work, real throwaway repositories. A new workflow `release-prepare.yml` runs the checks without secrets, then pushes with a deploy key that only it can use (environment `release-branch`) and that the `Release branches` ruleset lets through. The gate always runs on `release-X.Y`; `release-resume.yml` stops creating branches.

**Tech Stack:** bash, GitHub Actions, Python 3 unittest, git.

**Spec:** `docs/superpowers/specs/2026-11-26-release-branch-per-line-design.md`

## Global Constraints

- Branch names: `release-X.Y` (as today). Pins stay `pinned/vX.Y.Z...`.
- Versions: `X.Y.Z`, `X.Y.Z-beta.N`, `X.Y.Z-alpha.N` (`check-release-tag.sh`). Pre-releases only of `X.Y.0`.
- Order on a line: alpha after a lower alpha; beta after an alpha or a lower beta; stable after an alpha, a beta or a lower stable. A line may start with a beta or a stable: allowed with a `::warning::` saying it is not the usual order. Everything else: `::error::`, exit 1.
- Rebasing is allowed only while `main`'s version is the version being prepared (so `main` is on the line), the tag `vX.Y.0` does not exist, and no draft release, no `pinned/vX.Y.*` branch and no running release gate exist.
- Only `release-prepare.yml` force-pushes `release-X.Y`; its secret lives in the environment `release-branch` (branch policy `main`, the maintainer as reviewer). The check job holds no secret.
- Scripts follow the house style: `set -euo pipefail`, `::error::` lines on stderr with the exact next command to run, answers from `gh` into a variable before `grep -q` (`NoPipeIntoGrepQTests` enforces it), outputs as `key=value` lines for `$GITHUB_OUTPUT`.
- Every new script is covered by `tools/ci.sh shell-scripts`; every workflow change passes `tools/ci.sh workflows` (actionlint, zizmor).
- Commit messages: summary line, blank line, why. No attribution lines.
- Concurrency with the gate (ruling against the spec's "shares a concurrency group per line"): GitHub expressions cannot derive `X.Y` from the version input, so `release-prepare.yml` uses one group of its own (`release-prepare`) and `prepare-check.sh` refuses while a `release-gate.yml` run is queued or in progress. Same protection, no per-line group.

## Review Focus

- Two lines at once: `main` on `1.4.0-alpha.1`, `release-1.3` at `1.3.0-beta.2`: preparing `1.3.0` must refuse (main is not on 1.3) and say to bump with a pull request to `release-1.3`. (Task 4 test `test_main_left_the_line`.)
- A leftover fixed branch name that is itself protected (`release-1.3-fix`, `pinned/x`): refused before anything is pushed. (Task 4 test `test_fixed_branch_name_refused`.)
- The stray tags `v0.1.0-alpha.1`, `v0.1.0-alpha.2` (no GitHub Release) count as earlier releases of line 0.1 for the order check. (Task 1 test `test_old_tags_count`.)
- A rebase where `main` already has a branch commit (a cherry-pick): git drops it; the push still succeeds. (Task 4 test `test_rebase_drops_commit_main_has`.)
- `1.20` vs `1.2`: tags `v1.20.*` never count for line `1.2`. (Task 1 test `test_line_prefix_is_not_a_match`.)

---

### Task 1: The order check, `release-line.sh`

**Files:**
- Create: `.github/scripts/release-line.sh`
- Modify: `.github/scripts/tests/fake_gh.py` (only if `matching-refs/tags/vX.Y.` with a prefix needs it; it already serves `matching-refs/tags/` prefixes — check)
- Test: `.github/scripts/tests/test_release_scripts.py` (new class `ReleaseLineTests(FakeGh)`, constant `LINE = os.path.join(SCRIPTS, "release-line.sh")`)

**Interfaces:**
- Produces: `release-line.sh <version>` (no `v`). Reads the tags `vX.Y.*` from GitHub (`gh api --paginate repos/$GH_REPO/git/matching-refs/tags/vX.Y.`, then keeps only exact `vX.Y.<rest>` release tags). Prints `line=X.Y` and `first=true|false` on stdout. Exit 0 when allowed (with the warning on stderr when the line starts with a beta or stable), 1 with `::error::` otherwise. Version comparison in an inline `python3` (as `check-release-tag.sh` does for TOML): key `(Z, rank, N)` with rank alpha 0, beta 1, stable 2.

- [ ] **Step 1: Write the failing tests** (each sets `tags=[...]` in the fake state and runs `LINE` with a version)

  - `test_first_alpha`: tags `[]`, `1.3.0-alpha.1` → exit 0, stdout `line=1.3\nfirst=true\n`, no `::warning::`.
  - `test_first_beta_warns` / `test_first_stable_warns`: tags `[]`, `1.3.0-beta.1` / `1.3.0` → exit 0, stderr contains `::warning::` and `not the usual order`.
  - `test_allowed_steps`: for (tags, version) in `(["v1.3.0-alpha.1"], "1.3.0-alpha.2")`, `(["v1.3.0-alpha.2"], "1.3.0-beta.1")`, `(["v1.3.0-beta.1"], "1.3.0-beta.2")`, `(["v1.3.0-alpha.2"], "1.3.0")`, `(["v1.3.0-beta.2"], "1.3.0")`, `(["v1.3.0"], "1.3.1")`, `(["v1.3.0", "v1.3.1"], "1.3.2")` → exit 0, `first=false`, no warning.
  - `test_refused_steps`: `(["v1.3.0-beta.1"], "1.3.0-alpha.3")`, `(["v1.3.0"], "1.3.0-beta.3")`, `(["v1.3.0-beta.2"], "1.3.0-beta.2")`, `(["v1.3.0-beta.2"], "1.3.0-beta.1")`, `(["v1.3.1"], "1.3.1")` → exit 1, stderr `::error::` naming the newest release of the line (`1.3.0-beta.1` etc.), stdout empty.
  - `test_prerelease_of_patch_refused`: tags `["v1.3.0"]`, `1.3.1-beta.1` → exit 1, stderr contains `Pre-releases are only of X.Y.0`.
  - `test_line_prefix_is_not_a_match`: tags `["v1.20.0-beta.1"]`, `1.2.0-alpha.1` → exit 0, `first=true`.
  - `test_old_tags_count`: tags `["v0.1.0-alpha.1", "v0.1.0-alpha.2", "v0.1.0-beta.6"]`, `0.1.0-alpha.3` → exit 1.
  - `test_failed_lookup_stops`: `api_fails=["git/matching-refs/tags/"]` → exit 1, stderr `Could not read`.

- [ ] **Step 2: Run them to see them fail**

  Run: `python3 -m unittest .github/scripts/tests/test_release_scripts.py -k ReleaseLine -v` (from the repo root; check how `tools/ci.sh` invokes it and use the same)
  Expected: FAIL / ERROR, the script does not exist.

- [ ] **Step 3: Write `release-line.sh`** with the interface above. The header comment states the rules exactly as in Global Constraints.

- [ ] **Step 4: Run the tests**

  Same command. Expected: PASS. Then `shellcheck .github/scripts/release-line.sh`: no output.

- [ ] **Step 5: Commit** `Add release-line.sh: the order of releases on a line`

### Task 2: Where a release is cut and where its bump goes

**Files:**
- Modify: `.github/scripts/release-branch.sh`, `.github/scripts/bump-version.sh`
- Create: `.github/scripts/bump-branch.sh`
- Test: `test_release_scripts.py` (`ReleaseBranchTests`, `BumpVersionTests`, new `BumpBranchTests`)

**Interfaces:**
- Produces: `release-branch.sh <version>` now always prints `release-X.Y` (the branch every release is cut from, and the gate's ref). Same errors for bad names and a leading `v`.
- Produces: `bump-branch.sh <version> <main-version>` prints the branch the version bump goes to: `release-X.Y` for a patch; for a pre-release or `X.Y.0`: `main` unless `<main-version>` is on a later line than `X.Y` (compare `(X, Y)` numerically), then `release-X.Y`.
- `bump-version.sh` reads `main`'s version with `git show origin/main:Cargo.toml` (`[workspace.package] version`), uses `bump-branch.sh` for the branch it must be run on (the existing check against the current branch stays), and prints the next commands: the pull request (`--base <bump-branch>`), then, when the bump branch is `main`, `gh workflow run release-prepare.yml --ref main -f version=V`, then `gh workflow run release-gate.yml --ref release-X.Y -f version=V`.

- [ ] **Step 1: Update and add the failing tests**

  - `ReleaseBranchTests.test_branches`: `("1.3.0-beta.1", "release-1.3")`, `("1.3.0-alpha.2", "release-1.3")`, `("1.3.0", "release-1.3")`, `("1.2.1", "release-1.2")`, `("1.20.3", "release-1.20")`.
  - `BumpBranchTests.test_bump_branches`: `("1.3.0-beta.1", "1.3.0-alpha.2") → main`, `("1.3.0", "1.3.0-beta.2") → main`, `("1.3.0-alpha.1", "1.2.4") → main` (the first release of a new line is bumped on `main`, which is still on the old line), `("1.3.0-beta.3", "1.4.0-alpha.1") → release-1.3`, `("1.3.0", "1.4.0-alpha.1") → release-1.3`, `("1.3.1", "1.3.0") → release-1.3`, `("1.3.1", "1.3.1") → release-1.3`.
  - `BumpBranchTests.test_bad_input`: a bad version or bad main version → exit ≠ 0, empty stdout.
  - `BumpVersionTests`: the existing `test_prerelease_not_on_release_branch` / `test_patch_on_release_branch` keep their meaning; add `test_prints_prepare_then_gate` (on `main`, `1.3.0-beta.1`, a fake `origin/main` at `1.3.0-alpha.1`: stdout contains `gh workflow run release-prepare.yml --ref main -f version=1.3.0-beta.1` before `gh workflow run release-gate.yml --ref release-1.3 -f version=1.3.0-beta.1`) and `test_line_main_has_left` (main at `1.4.0-alpha.1`, run on `main` for `1.3.0` → exit 1, stderr names `release-1.3`).

- [ ] **Step 2: Run them to see them fail.** `-k "ReleaseBranch or BumpBranch or BumpVersion"`. Expected: the new and changed cases FAIL.

- [ ] **Step 3: Implement** as in Interfaces. Correct the rule comments in both scripts' headers ("This is the one place that rule lives").

- [ ] **Step 4: Run the tests.** Expected: PASS; `shellcheck` clean.

- [ ] **Step 5: Commit** `Cut every release from release-X.Y; bump on main while it is on the line`

### Task 3: The gate runs on `release-X.Y` and checks the order

**Files:**
- Modify: `.github/scripts/release-start-check.sh`, `.github/workflows/release-gate.yml` (header comment, `check` job outputs)
- Test: `test_release_scripts.py` (`StartCheckTests`), `.github/scripts/tests/test_workflow_policy.py` (`GateTests`)

**Interfaces:**
- Consumes: `release-branch.sh` (Task 2), `release-line.sh` (Task 1).
- Produces: `release-start-check.sh <version>` prints `tag=`, `prerelease=`, `branch=` (always `release-X.Y`); the `new-branch=` line is gone. It requires `GITHUB_REF=refs/heads/release-X.Y` (when set) and that `release-X.Y` exists on GitHub; then runs `release-line.sh` (its warning passes through to stderr).

- [ ] **Step 1: Update the failing tests**

  - `test_prerelease_from_release_branch` (replaces `test_prerelease_from_main`): branches `["main", "release-1.3"]`, `1.3.0-beta.1` on `refs/heads/release-1.3` → stdout `tag=v1.3.0-beta.1\nprerelease=true\nbranch=release-1.3\n`.
  - `test_minor_from_release_branch` (replaces `test_minor_from_main_makes_branch`): tags `["v1.3.0-beta.2"]` with releases, `1.3.0` on `release-1.3` → `branch=release-1.3\n`, no `new-branch`.
  - `test_wrong_branch`: `1.3.0-beta.1` on `refs/heads/main` → exit 1, stderr `gh workflow run release-gate.yml --ref release-1.3 -f version=1.3.0-beta.1`.
  - `test_branch_missing_points_to_prepare`: branches `["main"]`, `1.3.0-beta.1` → exit 1, stderr `gh workflow run release-prepare.yml --ref main -f version=1.3.0-beta.1`.
  - `test_order_refused`: tags `["v1.3.0"]` (with release), `1.3.0-beta.3` on `release-1.3` → exit 1, no `tag=` on stdout.
  - `test_first_beta_warns`: no tags, `1.3.0-beta.1` on `release-1.3` → exit 0, `::warning::` on stderr.
  - Delete `test_minor_when_branch_exists` (no longer an error). Keep `test_patch_without_branch`, `test_branch_prefix_is_not_a_match`, `test_failed_lookup_stops` (adjust the branch state).
  - `GateTests`: `set(jobs(gate)["check"]["outputs"]) == {"tag", "branch"}`; the header comment contains `--ref release-1.2 -f version=1.2.0` and `release-prepare.yml`.

- [ ] **Step 2: Run them to see them fail.** `-k StartCheck` and `test_workflow_policy.py -k Gate`.
- [ ] **Step 3: Implement.** Update the gate's header comment for the new flow (bump, prepare, gate on `release-X.Y`).
- [ ] **Step 4: Run the tests.** Expected: PASS.
- [ ] **Step 5: Commit** `Run the release gate on release-X.Y and check the order of the line`

### Task 4: Preparing the branch: checks and the rebase

**Files:**
- Create: `.github/scripts/prepare-check.sh`, `.github/scripts/prepare-branch.sh`
- Test: `test_release_scripts.py` (`PrepareCheckTests(FakeGh)`, `PrepareBranchTests(unittest.TestCase)` with real git)

**Interfaces:**
- Consumes: `release-line.sh`, `check-release-tag.sh --versions`.
- Produces: `prepare-check.sh <version> [<fixed-branch>]`, run in a checkout of `main`, no secrets. Prints `line=X.Y`, `branch=release-X.Y`, `fixed=<name or empty>`. Refuses (exit 1, `::error::` with the next command) when: `GITHUB_REF` (if set) is not `refs/heads/main`; the version is a patch; `Cargo.toml`/`dist.toml` on `main` do not hold the version (`check-release-tag.sh --versions`; message: bump on `main` first, or, if `main` is on a later line, bump with a pull request to `release-X.Y` and start the gate there); the tag `vX.Y.0` or `v<version>` exists; `release-line.sh` refuses; a draft release of any `vX.Y.*` exists (`gh api repos/$GH_REPO/releases --jq '.[] | select(.draft) | .tag_name'`); a branch `pinned/vX.Y.*` exists; a `release-gate.yml` run is queued or in progress (`gh run list --workflow release-gate.yml --json status`); `<fixed-branch>` is given and matches `release-*`, `pinned/*` or `main`, or does not exist on GitHub.
- Produces: `prepare-branch.sh <line> [<fixed-branch>]`, run in a full clone whose `origin` can be pushed to (the deploy key in CI; a local bare repo in tests). With no `release-X.Y` on `origin`: `git push origin origin/main:refs/heads/release-X.Y`, prints `created release-X.Y at <sha>`. Otherwise, without a fixed branch: rebase `origin/release-X.Y` onto `origin/main` in a detached work tree; on a conflict `git rebase --abort`, `::error::` that lists the conflicting files and the commands to rebase by hand into a branch of one's own and to start `release-prepare.yml` again with `-f fixed-branch=NAME`, exit 1, nothing pushed. On success: `git push --force-with-lease=release-X.Y:<old tip> origin HEAD:refs/heads/release-X.Y`, prints `rebased release-X.Y onto <main sha>: <old> -> <new>`. With a fixed branch: check that `origin/main` is an ancestor of it, that `git rev-list --merges origin/main..origin/<fixed>` is empty and that the list of `author<TAB>subject` of `origin/main..origin/<fixed>` (oldest first) is a subsequence of that of `origin/main..origin/release-X.Y`; then the same lease push of `origin/<fixed>`, then `git push origin --delete <fixed>`. Any failed check: exit 1, nothing pushed.

- [ ] **Step 1: Write the failing tests**

  `PrepareCheckTests` (fake `gh`, `version_fixture` for `main`'s files, `GITHUB_REF=refs/heads/main`):
  - `test_ok`: tags `["v1.3.0-alpha.1"]`, files `1.3.0-beta.1` → exit 0, stdout `line=1.3\nbranch=release-1.3\nfixed=\n`.
  - `test_not_on_main`: `GITHUB_REF=refs/heads/release-1.3` → exit 1, `--ref main` in stderr.
  - `test_patch_refused`: `1.3.1` → exit 1, `gh workflow run release-gate.yml --ref release-1.3` in stderr.
  - `test_main_left_the_line`: files `1.4.0-alpha.1`, version `1.3.0` → exit 1, stderr contains `--base release-1.3`.
  - `test_stable_out_refused`: tags `["v1.3.0"]` (with release), files and version `1.3.0-beta.3` → exit 1, stderr names `v1.3.0`.
  - `test_draft_refused`: `releases={"v1.3.0-beta.1": dict(REL, isDraft=True)}` (adapt to how the fake serves `releases` lists) → exit 1, stderr names `v1.3.0-beta.1`.
  - `test_pin_refused`: branches `["main", "pinned/v1.3.0-beta.1"]` → exit 1.
  - `test_gate_running_refused`: a running `release-gate.yml` run in the fake's run list → exit 1.
  - `test_fixed_branch_name_refused`: fixed `release-1.3-fix`, `pinned/x`, `main` → exit 1 each, nothing else called.
  - `test_fixed_branch_ok`: branches include `fix-1.3` → `fixed=fix-1.3`.
  - Extend `fake_gh.py` for `gh run list --workflow ... --json status` (state key `runs: [{"workflow": "release-gate.yml", "status": "in_progress"}]`) and the releases `draft` field, documenting both in its header.

  `PrepareBranchTests` (helper: a bare `origin.git`, a seed clone pushing `main`, and a fresh clone where the script runs):
  - `test_creates_branch`: no `release-1.3` → after the run, `origin`'s `release-1.3` == `main`'s tip.
  - `test_fast_forward`: `release-1.3` behind `main`, no own commits → equals `main`'s tip.
  - `test_rebase_keeps_own_commit`: `release-1.3` has one own commit (`fix.txt`), `main` moved on → new tip's parent is `main`'s tip, `fix.txt` present, subject kept.
  - `test_rebase_drops_commit_main_has`: the own commit was also cherry-picked to `main` → new tip == `main`'s tip.
  - `test_conflict_changes_nothing`: own commit and `main` change the same line → exit 1, `origin`'s `release-1.3` unchanged, stderr `fixed-branch=`.
  - `test_lease_protects`: install a `pre-push` hook in the script's clone that pushes a new commit to `origin`'s `release-1.3` from another clone; run the script → exit 1, `origin`'s `release-1.3` is the hook's commit.
  - `test_fixed_branch_ok`: fixed branch = own commits replayed by hand on `main` with a resolved conflict → `release-1.3` == fixed tip, fixed branch deleted on `origin`.
  - `test_fixed_branch_adds_commit_refused` / `test_fixed_branch_not_on_main_refused` / `test_fixed_branch_with_merge_refused`: exit 1, `origin` unchanged.

- [ ] **Step 2: Run them to see them fail.** `-k "PrepareCheck or PrepareBranch"`.
- [ ] **Step 3: Implement both scripts** as in Interfaces. `prepare-branch.sh` sets `user.name`/`user.email` only for the rebase (`git -c`), committer `irsync release bot`; it never runs `git push --force` without a lease.
- [ ] **Step 4: Run the tests.** Expected: PASS; `shellcheck` clean.
- [ ] **Step 5: Commit** `Add the scripts that create or rebase release-X.Y`

### Task 5: The workflow `release-prepare.yml`

**Files:**
- Create: `.github/workflows/release-prepare.yml`
- Test: `test_workflow_policy.py` (new `PrepareTests`; extend `EnvironmentTests` and `RELEASE_SECRET`)

**Interfaces:**
- Consumes: `prepare-check.sh`, `prepare-branch.sh` (Task 4).
- Produces: `on: workflow_dispatch` with inputs `version` (string, required) and `fixed-branch` (string, default `""`). `permissions: contents: read` at the top. `concurrency: {group: release-prepare, cancel-in-progress: false}`. Job `check` (no environment, `contents: read`, checkout with `persist-credentials: false`) runs `prepare-check.sh "$VERSION" "$FIXED"` into `$GITHUB_OUTPUT`; outputs `line`, `fixed`. Job `push` (`needs: check`, `environment: release-branch`, `permissions: {}`): `actions/checkout` (pinned SHA as elsewhere) with `ssh-key: ${{ secrets.RELEASE_BRANCH_KEY }}`, `fetch-depth: 0`, then `prepare-branch.sh "$LINE" "$FIXED"`; ends with a step summary naming the next command `gh workflow run release-gate.yml --ref release-X.Y -f version=V`. Inputs reach scripts only through `env:`, never `${{ }}` inside `run:`.

- [ ] **Step 1: Write the failing tests**
  - `PrepareTests.test_starts_only_by_hand`: triggers `{"workflow_dispatch"}`, inputs `{"version", "fixed-branch"}`.
  - `test_check_job_has_no_secrets`: `secrets.` not in `job_text("release-prepare.yml", "check")`; no `environment`.
  - `test_push_job_in_release_branch_env`: `env_name(push) == "release-branch"`, `needs` contains `check`, its checkout step's `with.ssh-key == "${{ secrets.RELEASE_BRANCH_KEY }}"`.
  - `test_inputs_only_through_env`: no `${{ inputs.` inside any `run:` of the file.
  - `EnvironmentTests.test_branch_key_only_in_release_branch_env`: every job of every workflow whose text has `secrets.RELEASE_BRANCH_KEY` has environment `release-branch`.
- [ ] **Step 2: Run them to see them fail.** `test_workflow_policy.py -k "Prepare or Environment"`.
- [ ] **Step 3: Write the workflow** with a header comment in the style of `release-gate.yml` (what it does, how to start it, the conflict path).
- [ ] **Step 4: Run** the tests, then `tools/ci.sh workflows`. Expected: PASS; actionlint and zizmor clean (if zizmor flags the persisted SSH key in `push`, add a justified entry to `.github/zizmor.yml` as the file's other entries do).
- [ ] **Step 5: Commit** `Add release-prepare.yml, which creates or rebases release-X.Y`

### Task 6: The resume run stops creating branches

**Files:**
- Modify: `.github/scripts/resume-check.sh`, `.github/workflows/release-resume.yml`
- Test: `test_release_scripts.py` (`ResumeTests`), `test_workflow_policy.py` (`GateTests.test_resume_makes_release_branch`)

**Interfaces:**
- Produces: `resume-check.sh` prints `packages=`, `homebrew=`, `notes=` only. `release-resume.yml` has no `branch` job and no `new-branch` output; its header comment no longer mentions creating `release-X.Y`.

- [ ] **Step 1: Update the tests**: every expected `resume-check.sh` stdout loses its `new-branch=` line; the tests that checked branch creation (around lines 384–398) are deleted; `test_resume_makes_release_branch` becomes `test_resume_makes_no_branch` (`"branch" not in jobs(resume)`, `"new-branch"` not in the file's text).
- [ ] **Step 2: Run them to see them fail.** `-k Resume`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the tests.** Expected: PASS. `tools/ci.sh workflows` clean.
- [ ] **Step 5: Commit** `Stop creating release-X.Y in the resume run`

### Task 7: GitHub settings: the environment, the key and the bypass

**Files:**
- Modify: `tools/github_settings.py`
- Test: `tools/tests/test_github_settings.py`

**Interfaces:**
- Produces: in `desired()`: environment `release-branch`: `{"policies": [{"name": "main", "type": "branch"}], "reviewers": [maintainer_id], "prevent_self_review": False}`. Ruleset `Release branches`: `bypass_actors: [{"actor_id": None, "actor_type": "DeployKey", "bypass_mode": "always"}]` (check against GitHub's ruleset API docs that this is the shape GitHub returns, and match `normalize_ruleset`). New key `deploy_keys: [{"title": "release-branch", "read_only": False}]`, read in `fetch` from `repos/{repo}/keys` (title and read_only only) and compared as a list: a `DeployKey` bypass covers every deploy key of the repository, so exactly this one may exist. `apply` does not create keys (the private half must never pass through this tool); `check` reports a missing or extra key with the instruction to add it by hand (the guide's setup section).
- Rename the test `test_desired_release_branches_like_main` to keep the rules check and assert the new bypass.

- [ ] **Step 1: Write the failing tests**: `test_release_branches_bypass_only_deploy_keys` (the bypass list above), `test_release_branch_env` (the environment above), `test_extra_deploy_key_reported` (`diff` of want vs a `have` with two keys gives a line mentioning `deploy_keys`), `test_apply_creates_no_keys` (no `gh` call with `keys` and `-X POST`).
- [ ] **Step 2: Run them to see them fail.** `python3 -m unittest discover -s tools/tests -k github_settings`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the tests.** Expected: PASS.
- [ ] **Step 5: Commit** `Let only the release-branch deploy key past the release branch rule`

### Task 8: The guide and the full check

**Files:**
- Modify: `book/src/contributing/releasing.md` (sections "Where a release is cut from", "Cutting a release", "Copying a fix to a release branch", "What the workflow does", "If a release fails", "One-time setup", "Creating `release-X.Y` by hand"), `book/src/contributing/supply-chain.md` where it lists the rulesets and environments, `book/src/contributing/website.md:109` if its wording about `origin/release-X.Y` no longer holds.

- [ ] **Step 1: Rewrite the sections** to the spec: the life cycle and order rules (with the warning); the three cases of cutting a release (`main` on the line: bump on `main`, `release-prepare.yml`, gate on `release-X.Y`; `main` has left the line: bump with a pull request to `release-X.Y`, gate; patch: as before); copying a fix (before `X.Y.0`: a pull request to `release-X.Y` adds a commit the next rebase carries; after: as today); "Only the newest `release-X.Y` branch gets fixes" becomes "lines that have not reached `X.Y.0`, and the newest stable line"; resolving a rebase conflict (the exact commands: `git fetch origin`, `git switch -c fix-X.Y origin/release-X.Y`, `git rebase origin/main`, resolve, `git push origin fix-X.Y`, `gh workflow run release-prepare.yml --ref main -f version=V -f fixed-branch=fix-X.Y`); one-time setup: `ssh-keygen -t ed25519 -N '' -f release-branch`, add `release-branch.pub` as the deploy key `release-branch` with write access, store the private key as `RELEASE_BRANCH_KEY` in the environment `release-branch`, `tools/github_settings.py apply`, then delete both files; moving over: `git push origin v0.1.0-beta.6:refs/heads/release-0.1` (by an admin with the ruleset briefly off, as the existing section says).
- [ ] **Step 2: Run the full checks**

  Run: `tools/ci.sh check` and `tools/ci.sh workflows` and `tools/ci.sh shell-scripts`.
  Expected: all pass. (`python3 -m unittest discover -s tools/tests` is part of `check`; the book chapters are not man-page sources.)
- [ ] **Step 3: Commit** `Describe release branches for every release line in the guide`

## After merging (for the maintainer, not part of the branch)

1. Create the key, the deploy key and the environment secret (guide, one-time setup); `tools/github_settings.py apply`; `tools/github_settings.py check` reports nothing.
2. Create `release-0.1` at `v0.1.0-beta.6`.
3. The next 0.1 release is the first through `release-prepare.yml`: watch it.
