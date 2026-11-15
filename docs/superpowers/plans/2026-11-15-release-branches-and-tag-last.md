# Release branches and tag-last releases: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A release is started by running the release gate with a version, and
the `v*` tag is created only at the very end, together with the GitHub
Release; stable patch releases come from `release-X.Y` branches.

**Architecture:** `release-gate.yml` changes from a tag-push trigger to
`workflow_dispatch` with a `version` input, run on the source branch
(`main` for pre-releases and `X.Y.0`, `release-X.Y` for patches). The commit
it tests is `GITHUB_SHA`. It pins that commit on a short-lived branch
`pinned/vX.Y.Z`, launches the generated `release.yml` on that branch (so
`release.yml`'s `github.sha`, which `gh release create --target` tags, is the
tested commit), then for `X.Y.0` creates `release-X.Y`, and finally launches
`release-resume.yml` on the new tag for packages, Homebrew and notes (the
`release` environment keeps opening only on `v*` tags). Stray tags (a `v*`
tag with no GitHub Release) are caught by a script that the gate and
`tools/github_settings.py check` both run. A hard block on hand-pushed tags is
tried last, as a separate, optional task.

**Tech Stack:** GitHub Actions YAML, bash scripts in `.github/scripts/`,
Python `unittest` tests with the fake `gh` (`.github/scripts/tests/fake_gh.py`),
`tools/github_settings.py`, mdBook docs.

**Spec:** `docs/superpowers/specs/2026-11-15-release-branches-and-tag-last-design.md`

## Global Constraints

- Never edit `.github/workflows/release.yml` by hand; the `release.yml is up
  to date` job in `release-tooling.yml` must keep passing.
- Version names: `X.Y.Z`, `X.Y.Z-beta.N`, `X.Y.Z-alpha.N` (the rules in
  `.github/scripts/check-release-tag.sh`; tags are `v` + version).
- Source branch: pre-releases and `X.Y.0` from `main`; `X.Y.Z` with Z > 0 and
  no suffix from `release-X.Y`.
- Release branches are named `release-X.Y` (e.g. `release-1.2`). Bump PR
  branches are named `bump-vX.Y.Z`. Pin branches are named `pinned/vX.Y.Z`.
- Tags can never be moved or deleted, by anyone. The `Release tags` ruleset
  keeps `update`, `deletion`, `non_fast_forward` with no bypass actors.
- The `release` environment keeps its policy `v*` tags only.
- The tag lands on exactly the commit the gate tested (`GITHUB_SHA` of the
  gate run).
- Known stray tags from before this change, accepted forever:
  `v0.1.0-alpha.1`, `v0.1.0-alpha.2`.
- Every third-party action stays pinned to a commit (`test_workflow_policy.py`).
- Commit messages: a short summary line, a blank line, a brief why. No
  attribution trailers of any kind.
- Script tests run with `python3 -m unittest discover -s .github/scripts/tests`;
  settings tests with `python3 -m unittest discover -s tools/tests`. Before
  pushing: `tools/ci.sh check` and `tools/ci.sh workflows`.

## Review Focus

1. **A push to the source branch while the tests run.** The tag must still
   land on the tested commit. Pinned by Task 4's
   `test_gate_pins_the_tested_commit`.
2. **Re-running the gate after the GitHub Release was published.** It must
   stop at once and point to `release-resume.yml`, not try to publish again.
   Pinned by Task 2's `test_tag_exists_points_to_resume`.
3. **Retrying after a run that died between pinning and publishing.** A
   leftover `pinned/vX.Y.Z` branch must not block the retry, but a
   `release.yml` run from the earlier attempt that is still going must stop
   the retry. Pinned by Task 4's `test_gate_clears_stale_pin` and
   `test_gate_refuses_while_old_release_run_active`.
4. **Starting the gate on the wrong branch** (e.g. `1.2.1` on `main`, or
   `1.3.0-beta.1` on `release-1.2`). It must refuse and print the right
   command. Pinned by Task 2's `test_wrong_branch`.
5. **A hand-pushed tag**, either for the version being released or any other.
   The gate refuses and names it. Pinned by Task 1's tests and Task 2's
   `test_stray_tag_blocks`.

---

### Task 1: Stray-tag check

**Files:**
- Create: `.github/scripts/stray-tags.sh`
- Modify: `.github/scripts/tests/fake_gh.py` (the `api` branch)
- Test: `.github/scripts/tests/test_release_scripts.py` (new class `StrayTagTests(FakeGh)`)

**Interfaces:**
- Produces: `stray-tags.sh` (no arguments; needs `GH_REPO`, gh logged in).
  Prints one stray tag per line on stdout, sorted with `sort -V`. Exit 0 if
  none, 1 if any (with an `::error::` line on stderr naming them and saying
  release tags are created only by the release gate). A stray tag is a
  `v*` tag that has no GitHub Release (draft or not), other than
  `v0.1.0-alpha.1` and `v0.1.0-alpha.2`.
  Tags from `gh api --paginate repos/$GH_REPO/git/matching-refs/tags/v --jq '.[].ref | ltrimstr("refs/tags/")'`;
  releases from `gh api --paginate repos/$GH_REPO/releases --jq '.[].tag_name'`.
- Produces (fake gh): `api repos/R/git/matching-refs/tags/<prefix>` prints the
  state's `tags` that start with `<prefix>`; `api repos/R/git/matching-refs/heads/<prefix>`
  prints the state's `branches` (new key, default `[]`) that start with
  `<prefix>`; `api repos/R/releases` prints the keys of `releases`. The
  existing `tags/v` behaviour stays.

- [ ] **Step 1: Write the failing tests**

```python
class StrayTagTests(FakeGh):
    def stray(self):
        return self.run_script(STRAY)

    def test_no_tags(self):
        r = self.stray()
        self.assertEqual((r.returncode, r.stdout), (0, ""))

    def test_every_tag_has_a_release(self):
        self.set_state(releases={"v1.0.0": REL, "v1.1.0-beta.1": REL},
                       tags=["v1.0.0", "v1.1.0-beta.1"])
        self.assertEqual(self.stray().returncode, 0)

    def test_old_alphas_are_accepted(self):
        self.set_state(releases={}, tags=["v0.1.0-alpha.1", "v0.1.0-alpha.2"])
        self.assertEqual(self.stray().returncode, 0)

    def test_hand_pushed_tag_is_stray(self):
        self.set_state(releases={"v1.0.0": REL}, tags=["v1.0.0", "v1.0.1", "v1.0.10"])
        r = self.stray()
        self.assertEqual(r.returncode, 1)
        self.assertEqual(r.stdout, "v1.0.1\nv1.0.10\n")
        self.assertIn("::error::", r.stderr)
        self.assertIn("v1.0.1", r.stderr)

    def test_draft_release_counts(self):
        self.set_state(releases={"v1.0.0": dict(REL, isDraft=True)}, tags=["v1.0.0"])
        self.assertEqual(self.stray().returncode, 0)
```

`STRAY = os.path.join(SCRIPTS, "stray-tags.sh")` next to the other script
paths; `REL = {"body": "", "isDraft": False, "isPrerelease": False}` at module
level (reuse it in `ResumeTests` if that class already builds the same dict).

- [ ] **Step 2: Run them to see them fail**

Run: `python3 -m unittest discover -s .github/scripts/tests -k StrayTag`
Expected: FAIL (script missing; fake gh rejects `repos/.../releases`).

- [ ] **Step 3: Extend `fake_gh.py` and write `stray-tags.sh`**

Header comment in the style of the other scripts (usage, what it checks, what
it needs). `set -euo pipefail`. The allowlist is a bash array at the top with a
comment saying why those two exist.

- [ ] **Step 4: Run the whole script suite**

Run: `python3 -m unittest discover -s .github/scripts/tests`
Expected: all pass (the notes tests still use `matching-refs/tags/v`).

- [ ] **Step 5: Commit**

```bash
git add .github/scripts/stray-tags.sh .github/scripts/tests/
git commit -m "Add a check for release tags without a release" -m "Once the release gate creates the tags, a v* tag with no GitHub Release can only have been pushed by hand. The gate and the settings check will refuse to go on while one exists."
```

---

### Task 2: Start check for the release gate

**Files:**
- Create: `.github/scripts/release-start-check.sh`
- Test: `.github/scripts/tests/test_release_scripts.py` (new class `StartCheckTests(FakeGh)`)

**Interfaces:**
- Consumes: `check-release-tag.sh --versions <tag>` (prints `prerelease=…`),
  `stray-tags.sh` (Task 1), fake gh `branches`/`tags` state (Task 1).
- Produces: `release-start-check.sh <version>` (version without `v`). Run in a
  checkout of the commit to release. Needs `GH_REPO`; reads `GITHUB_REF` if
  set. Prints for `$GITHUB_OUTPUT`, in this order:
  `tag=vX.Y.Z`, `prerelease=true|false`, `branch=<source branch>`,
  `new-branch=<release-X.Y or empty>`. Exits 1 with an `::error::` line on the
  first failed check:
  1. The version is valid and both files hold it (`check-release-tag.sh --versions "v$version"`).
     A leading `v` in the input is an error that says to drop it.
  2. Source branch from the version (Global Constraints). If `GITHUB_REF` is
     set and is not `refs/heads/<branch>`, error with the exact command:
     `gh workflow run release-gate.yml --ref <branch> -f version=<version>`.
  3. The tag does not exist (exact match in `matching-refs/tags/<tag>`). If it
     exists and has a GitHub Release: error saying the release is out and to
     finish it with `gh workflow run release-resume.yml --ref <tag> -f tag=<tag>`.
     If it exists without one: error saying it was pushed by hand and that
     this number can't be used; release the next version.
  4. `stray-tags.sh` passes (its own error explains).
  5. For `X.Y.0`: `release-X.Y` does not exist (exact match in
     `matching-refs/heads/release-X.Y`); `new-branch=release-X.Y`.
     For a patch: `release-X.Y` exists; `new-branch=` empty.
     Pre-releases: `new-branch=` empty.

- [ ] **Step 1: Write the failing tests**

Build each case with the `CheckTagTests.fixture` pattern (move `fixture` to a
module-level helper `version_fixture(test, cargo, dist) -> dir` so both
classes use it) and `self.env["GITHUB_REF"]` set per case.

```python
class StartCheckTests(FakeGh):
    def start(self, version, ref=None, files=None):
        d = version_fixture(self, files or version, files or version)
        if ref:
            self.env["GITHUB_REF"] = ref
        return self.run_script(START, version, cwd=d)

    def test_prerelease_from_main(self):
        r = self.start("1.3.0-beta.1", "refs/heads/main")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "tag=v1.3.0-beta.1\nprerelease=true\nbranch=main\nnew-branch=\n")

    def test_minor_from_main_makes_branch(self):
        r = self.start("1.3.0", "refs/heads/main")
        self.assertEqual(r.stdout, "tag=v1.3.0\nprerelease=false\nbranch=main\nnew-branch=release-1.3\n")

    def test_patch_from_release_branch(self):
        self.set_state(releases={}, tags=[], branches=["main", "release-1.2"])
        r = self.start("1.2.1", "refs/heads/release-1.2")
        self.assertEqual(r.stdout, "tag=v1.2.1\nprerelease=false\nbranch=release-1.2\nnew-branch=\n")

    def test_wrong_branch(self):
        for version, ref, want in [("1.2.1", "refs/heads/main", "--ref release-1.2"),
                                   ("1.3.0-beta.1", "refs/heads/release-1.2", "--ref main"),
                                   ("1.3.0", "refs/tags/v1.3.0", "--ref main")]:
            r = self.start(version, ref)
            self.assertEqual(r.returncode, 1, version)
            self.assertIn(f"gh workflow run release-gate.yml {want} -f version={version}", r.stderr)

    def test_tag_exists_points_to_resume(self):
        self.set_state(releases={"v1.3.0": REL}, tags=["v1.3.0"], branches=["main"])
        r = self.start("1.3.0", "refs/heads/main")
        self.assertEqual(r.returncode, 1)
        self.assertIn("gh workflow run release-resume.yml --ref v1.3.0 -f tag=v1.3.0", r.stderr)

    def test_tag_pushed_by_hand(self):
        self.set_state(releases={}, tags=["v1.3.0"], branches=["main"])
        r = self.start("1.3.0", "refs/heads/main")
        self.assertEqual(r.returncode, 1)
        self.assertIn("pushed by hand", r.stderr)

    def test_stray_tag_blocks(self):
        self.set_state(releases={}, tags=["v1.2.9"], branches=["main"])
        r = self.start("1.3.0", "refs/heads/main")
        self.assertEqual(r.returncode, 1)
        self.assertIn("v1.2.9", r.stderr)

    def test_minor_when_branch_exists(self):
        self.set_state(releases={}, tags=[], branches=["main", "release-1.3"])
        r = self.start("1.3.0", "refs/heads/main")
        self.assertEqual(r.returncode, 1)
        self.assertIn("release-1.3 already exists", r.stderr)

    def test_patch_without_branch(self):
        self.set_state(releases={}, tags=[], branches=["main"])
        r = self.start("1.2.1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("release-1.2", r.stderr)

    def test_branch_prefix_is_not_a_match(self):
        # release-1.20 must not count as release-1.2
        self.set_state(releases={}, tags=[], branches=["main", "release-1.20"])
        self.assertEqual(self.start("1.2.1").returncode, 1)

    def test_files_disagree(self):
        r = self.start("1.3.0", "refs/heads/main", files="1.2.0")
        self.assertEqual(r.returncode, 1)
        self.assertIn("Cargo.toml has version 1.2.0", r.stderr)

    def test_leading_v(self):
        r = self.start("v1.3.0", "refs/heads/main")
        self.assertEqual(r.returncode, 1)
        self.assertIn("without the v", r.stderr)
```

(`START = os.path.join(SCRIPTS, "release-start-check.sh")`. In
`test_files_disagree` the fixture's files hold `1.2.0`; adjust the `start`
helper so `files` sets the files' version while `version` is what is
released.)

- [ ] **Step 2: Run them to see them fail**

Run: `python3 -m unittest discover -s .github/scripts/tests -k StartCheck`
Expected: FAIL (script missing).

- [ ] **Step 3: Write `release-start-check.sh`**

Same style as `resume-check.sh`. Branch from the version with one bash regex:
`^([0-9]+)\.([0-9]+)\.([0-9]+)$` with Z > 0 means patch.

- [ ] **Step 4: Run the whole script suite**

Run: `python3 -m unittest discover -s .github/scripts/tests`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add .github/scripts/release-start-check.sh .github/scripts/tests/
git commit -m "Add the start check for a release run" -m "The release gate will be started with a version instead of a pushed tag. This script does its checks up front: the version, the branch it runs on, and that neither the tag nor a stray tag is in the way."
```

---

### Task 3: Bump script for release branches

**Files:**
- Modify: `.github/scripts/bump-version.sh`
- Test: `.github/scripts/tests/test_release_scripts.py` (`BumpVersionTests`)

**Interfaces:**
- Produces: `bump-version.sh <version>` now (1) works out the base branch
  with the same rule as Task 2, (2) refuses unless the current branch is that
  base, with the hint `git switch <base> && git pull --ff-only`, (3) commits
  on `bump-v<version>`, (4) prints next steps without any `git tag`:
  ```
  git push -u origin bump-v<version>
  gh pr create --fill --base <base>
  # once "CI result" passes:
  gh pr merge --merge
  gh workflow run release-gate.yml --ref <base> -f version=<version>
  ```

- [ ] **Step 1: Update and add the tests**

- Rename `test_commits_on_a_release_branch` to `test_commits_on_a_bump_branch`;
  expected branch `bump-v1.3.0-beta.1`.
- `test_hint_goes_through_a_pull_request`: expect
  `git push -u origin bump-v1.3.0-beta.1`, `gh pr create --fill --base main`,
  `gh workflow run release-gate.yml --ref main -f version=1.3.0-beta.1`, and
  `assertNotIn("git tag", r.stdout)`.
- New `test_patch_needs_release_branch`: on `main`, `bump("1.2.4")` exits 1
  with `git switch release-1.2` in stderr.
- New `test_patch_on_release_branch`: `git switch -q -c release-1.2`, then
  `bump("1.2.4")` exits 0, branch is `bump-v1.2.4`, stdout has
  `--base release-1.2` and `--ref release-1.2`.
- New `test_prerelease_not_on_release_branch`: on `release-1.2`,
  `bump("1.3.0-beta.1")` exits 1 with `git switch main` in stderr.

- [ ] **Step 2: Run them to see them fail**

Run: `python3 -m unittest discover -s .github/scripts/tests -k BumpVersion`
Expected: the changed and new tests FAIL.

- [ ] **Step 3: Change `bump-version.sh`**

Update the header comment to the new flow.

- [ ] **Step 4: Run the whole script suite**

Run: `python3 -m unittest discover -s .github/scripts/tests`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add .github/scripts/bump-version.sh .github/scripts/tests/
git commit -m "Bump the version on the right branch for release branches" -m "A patch release is bumped on release-X.Y, everything else on main. The PR branch is now bump-vX.Y.Z so it can't be mistaken for a release branch, and the hints start the release gate instead of pushing a tag."
```

---

### Task 4: The release gate runs on a version, and the tag comes last

**Files:**
- Modify: `.github/workflows/release-gate.yml` (whole file)
- Modify: `.github/workflows/ci.yml` (the `push` trigger and the `workflow_call` comment)
- Modify: `.github/workflows/release-resume.yml` (header comment only)
- Test: `.github/scripts/tests/test_workflow_policy.py` (new class `GateTests`; update `EnvironmentTests`)

**Interfaces:**
- Consumes: `release-start-check.sh` outputs `tag`, `prerelease`, `branch`,
  `new-branch` (Task 2).
- Produces: `release-gate.yml` with trigger
  `workflow_dispatch: inputs: version: {type: string, required: true, description: "The version, without the v, for example 1.2.3"}`
  and no other trigger. Concurrency group `release-v${{ inputs.version }}`
  (the same string as `release-resume.yml`'s `release-${{ inputs.tag }}`, so
  the resume run it starts waits for the gate run to end). Jobs:
  - `check`: checkout, `release-start-check.sh "$VERSION" >> "$GITHUB_OUTPUT"`
    with `GH_TOKEN`, `GH_REPO`; outputs the four values.
  - `test`: `uses: ./.github/workflows/ci.yml` with `plan: v${{ inputs.version }}`
    (the value the gate passed before; `ci.yml` uses it for the release
    targets).
  - `release` (`needs: [check, test]`, permissions `actions: write`,
    `contents: write`), steps:
    1. Refuse if any `release.yml` run whose `headBranch` is `pinned/$TAG` is
       `queued` or `in_progress` (an earlier attempt still going): error says
       to cancel it or wait for it first.
    2. Delete `pinned/$TAG` if it exists (a leftover from an earlier attempt;
       the tag doesn't exist, the start check made sure).
    3. Create `refs/heads/pinned/$TAG` at `$GITHUB_SHA`:
       `gh api "repos/$GH_REPO/git/refs" -f ref="refs/heads/pinned/$TAG" -f sha="$GITHUB_SHA"`.
    4. Launch `gh workflow run release.yml --ref "refs/heads/pinned/$TAG" -f tag="$TAG"`
       and find the run with today's logic, matching `headBranch` against
       `pinned/$TAG` (and `refs/heads/pinned/$TAG`). Keep the `headSha` check
       against `$GITHUB_SHA`. Drop the step that follows annotated tags.
    5. `gh run watch "$RUN_ID" --exit-status --interval 30`.
    6. `if: always()`: delete `pinned/$TAG` (ignore "not found").
  - `branch` (`needs: [check, release]`, `if: needs.check.outputs.new-branch != ''`,
    `contents: write`): create `refs/heads/<new-branch>` at `$GITHUB_SHA`.
  - `finish` (`needs: [check, release]`, `actions: write`): launch
    `gh workflow run release-resume.yml --ref "refs/tags/$TAG" -f tag="$TAG"`,
    then print a line with the Actions link saying that for a stable release
    the run waits for the `release` approval there.
  - The `packages`, `homebrew` and `notes` jobs are removed from the gate;
    `release-resume.yml` does them.
- `ci.yml`: `push.branches: [main, 'release-*']`; fix the comment that says
  the gate calls it "on a tag push".

- [ ] **Step 1: Write the failing policy tests**

```python
class GateTests(unittest.TestCase):
    def gate(self):
        return load_workflows()["release-gate.yml"]

    def test_gate_starts_only_by_hand_with_a_version(self):
        on = triggers(self.gate())
        self.assertEqual(set(on), {"workflow_dispatch"})
        self.assertEqual(set(on["workflow_dispatch"]["inputs"]), {"version"})

    def test_no_workflow_runs_on_a_tag_push(self):
        for name, wf in load_workflows().items():
            push = triggers(wf).get("push") or {}
            self.assertNotIn("tags", push, name)

    def test_gate_pins_the_tested_commit(self):
        text = job_text("release-gate.yml", "release")
        self.assertIn('ref="refs/heads/pinned/$TAG" -f sha="$GITHUB_SHA"', text)
        self.assertIn('--ref "refs/heads/pinned/$TAG"', text)
        self.assertIn('"$run_sha" != "$GITHUB_SHA"', text)

    def test_gate_clears_stale_pin(self):
        steps = jobs(self.gate())["release"]["steps"]
        last = steps[-1]
        self.assertEqual(last.get("if"), "always()")
        self.assertIn("pinned/$TAG", last["run"])

    def test_gate_refuses_while_old_release_run_active(self):
        text = job_text("release-gate.yml", "release")
        self.assertIn("in_progress", text)
        self.assertIn("queued", text)

    def test_gate_makes_release_branch_after_release(self):
        job = jobs(self.gate())["branch"]
        self.assertIn("release", job["needs"])
        self.assertIn("new-branch", job["if"])

    def test_gate_finishes_on_the_tag(self):
        self.assertIn('release-resume.yml --ref "refs/tags/$TAG"',
                      job_text("release-gate.yml", "finish"))

    def test_gate_and_resume_share_concurrency(self):
        self.assertEqual(self.gate()["concurrency"]["group"], "release-v${{ inputs.version }}")
        self.assertEqual(load_workflows()["release-resume.yml"]["concurrency"]["group"],
                         "release-${{ inputs.tag }}")

    def test_ci_runs_on_release_branches(self):
        push = triggers(load_workflows()["ci.yml"])["push"]
        self.assertEqual(push["branches"], ["main", "release-*"])
```

Update `EnvironmentTests`: in `test_release_env_callers_inherit_secrets`
loop over `release-resume.yml` only; `homebrew_caller` /
`test_homebrew_job_skips_prereleases` check only the resume caller; add
`test_gate_has_no_release_env_jobs` asserting no gate job `uses`
`packages.yml` or `publish-homebrew.yml`.

(If `triggers()` maps YAML's `on` key oddly for `workflow_dispatch` with
inputs, follow how the file already handles it.)

- [ ] **Step 2: Run them to see them fail**

Run: `python3 -m unittest discover -s .github/scripts/tests -k Gate`
Expected: FAIL.

- [ ] **Step 3: Rewrite `release-gate.yml`, change `ci.yml`, update the resume header**

Rewrite the gate's header comment for the new flow (how to start it, that
the tag is created by `release.yml` at the end, never launch `release.yml`
by hand, and that stable releases are finished by the resume run it
starts). Pin actions exactly as the current file does.
`release-resume.yml`'s header: it is now also started by the gate, as the
normal second half of every release.

- [ ] **Step 4: Run the policy tests, script tests and actionlint**

Run: `python3 -m unittest discover -s .github/scripts/tests && tools/ci.sh workflows`
Expected: all pass, actionlint clean.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/ .github/scripts/tests/
git commit -m "Start releases with a version and create the tag last" -m "A pushed tag used to start the release and could never be removed, so any failure used up the version number. The gate now runs on the source branch, tests that commit, and lets release.yml create the tag together with the GitHub Release. Until then, a failed run can simply be started again."
```

---

### Task 5: GitHub settings for release and pin branches

**Files:**
- Modify: `tools/github_settings.py` (`desired()`, `main()`'s `check`)
- Test: `tools/tests/test_github_settings.py`

**Interfaces:**
- Consumes: `.github/scripts/stray-tags.sh` (Task 1).
- Produces in `desired()[MAIN]["rulesets"]`:
  - `"Release branches": ruleset("branch", ["refs/heads/release-*"], dict(protect, pull_request={"required_approving_review_count": 0}, required_status_checks={"contexts": [REQUIRED_CHECK]}))`
    (the same rules as `main`).
  - `"Pinned release commits": ruleset("branch", ["refs/heads/pinned/*"], {"update": {}, "non_fast_forward": {}})`
    (the gate creates and deletes these, nobody can move one).
  - `"Release tags"` unchanged.
- `check` also runs `stray-tags.sh` with `GH_REPO=irsync-dev/irsync` and
  counts its output as differences, printed as
  `irsync-dev/irsync: tag <tag>: has no GitHub Release (pushed by hand?)`.
- `apply` skips `"Release branches"` with the same message as `main` when no
  `CI result` has passed (generalize the `name == "main"` check to both).

- [ ] **Step 1: Write the failing tests**

```python
    def test_desired_release_branches_like_main(self):
        rs = gs.desired(1)[REPO]["rulesets"]
        self.assertEqual(rs["Release branches"]["include"], ["refs/heads/release-*"])
        self.assertEqual(rs["Release branches"]["rules"], rs["main"]["rules"])
        self.assertEqual(rs["Release branches"]["bypass_actors"], [])

    def test_desired_pins_cannot_move(self):
        pins = gs.desired(1)[REPO]["rulesets"]["Pinned release commits"]
        self.assertEqual(pins["include"], ["refs/heads/pinned/*"])
        self.assertEqual(set(pins["rules"]), {"update", "non_fast_forward"})

    def test_stray_tags_reported(self):
        # stray_tag_lines(output) -> list[str]
        self.assertEqual(gs.stray_tag_lines("v1.0.1\n"),
                         ["irsync-dev/irsync: tag v1.0.1: has no GitHub Release (pushed by hand?)"])
        self.assertEqual(gs.stray_tag_lines(""), [])
```

Keep `test_desired_tags_allow_creation` as it is: Task 7 changes it only if
the hard block works.

- [ ] **Step 2: Run them to see them fail**

Run: `python3 -m unittest discover -s tools/tests`
Expected: FAIL.

- [ ] **Step 3: Implement**

`stray_tag_lines(output: str) -> list[str]` in `github_settings.py`; `check`
calls the script with `subprocess.run` and feeds its stdout through it.

- [ ] **Step 4: Run the tests**

Run: `python3 -m unittest discover -s tools/tests`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add tools/github_settings.py tools/tests/test_github_settings.py
git commit -m "Protect release and pin branches, and report stray tags" -m "release-X.Y branches take changes only through a PR with CI, like main. The gate's pinned/vX.Y.Z branches can be created and deleted but never moved, so release.yml always builds the tested commit."
```

---

### Task 6: Release docs

**Files:**
- Modify: `book/src/contributing/releasing.md`
- Modify: any other page that says a release starts with a pushed tag
  (`grep -rn "push.*tag\|git tag" book/src README.md AGENTS.md SECURITY.md`),
  most likely `book/src/contributing/supply-chain.md`.

**Interfaces:** none (docs only; no tests, clippy or fmt per `AGENTS.md`).

- [ ] **Step 1: Rewrite `releasing.md`**

In the existing plain style, cover:
- **Cutting a release**, three cases: a pre-release (from `main`), `X.Y.0`
  (from `main`; creates `release-X.Y`), a patch (from `release-X.Y`). Each:
  bump script, PR (`--base`), merge, `gh workflow run release-gate.yml --ref <branch> -f version=<version>`.
- **Copying a fix to a release branch**: land it on `main`, then
  `git switch release-1.2 && git pull --ff-only && git switch -c fix-…`,
  `git cherry-pick -x <commit>`, PR with `--base release-1.2`. Only the
  newest `release-X.Y` gets fixes.
- **What the workflow does**: check, test, pin, release (tag and GitHub
  Release together: the point of no return), branch for `X.Y.0`, then the
  resume run on the tag (approve `release` there for stable releases).
- **If a release fails**: before the GitHub Release exists, fix and run the
  gate again with the same version; a leftover `pinned/…` branch is cleared
  by the next run; a still-running `release.yml` must be cancelled first.
  After it exists, use the resume workflow. Remove the "move the tag" and
  "tag the next patch version" advice and the two traps that no longer
  apply; keep "Cancelling the gate does not stop `release.yml`".
- **Protecting the release tags**: tags are created only by `release.yml`;
  never push a `v*` tag. A hand-pushed tag stops every release until
  resolved (it can't be deleted; it uses up its number) and shows up in
  `tools/github_settings.py check`. Mention the two old alpha tags.
- Lightweight tags: release tags are now created by GitHub, not annotated.
- Leave "The first release" as history, with one line saying tags are no
  longer pushed by hand.

- [ ] **Step 2: Build the book**

Run: `mdbook build book`
Expected: builds with no warnings about broken links.

- [ ] **Step 3: Commit**

```bash
git add book/ README.md SECURITY.md
git commit -m "Document release branches and tag-last releases"
```

---

### Task 7: Roll out and rehearse (maintainer, on GitHub)

This needs the maintainer: it changes live settings and makes a real
pre-release. The implementer prepares the commands and stops for each.

- [ ] **Step 1: Delete the old `release-v*` PR branches** (they would fall
  under the new `release-*` ruleset): list with
  `git ls-remote --heads origin 'release-v*'` and delete each after checking
  it's merged. Also the local ones.
- [ ] **Step 2: Merge the PR, then apply the settings:**
  `tools/github_settings.py apply`, then `tools/github_settings.py check`
  prints nothing.
- [ ] **Step 3: Rehearse with the next pre-release** (`0.1.0-beta.4`, or
  whatever is next): bump, PR, merge,
  `gh workflow run release-gate.yml --ref main -f version=0.1.0-beta.4`.
  Check: the tag `v0.1.0-beta.4` appears only when the GitHub Release does,
  on the tested commit (`git rev-parse v0.1.0-beta.4^{commit}` equals the
  gate run's head SHA); `pinned/v0.1.0-beta.4` is gone; the resume run on
  the tag added the notes.
- [ ] **Step 4: Rehearse a failure:** start the gate for the next version
  without bumping the files. It must fail in `check`, and no tag must exist.
  Then bump properly and run the same version again: it must release.
- [ ] **Step 5: Check at the first `X.Y.0` (`0.1.0`) that the gate's
  `branch` job could create `release-0.1`.** It runs after the release, so a
  failure there costs no version number. If the ruleset blocked it, create
  the branch by hand (`git push origin v0.1.0:refs/heads/release-0.1`) and
  fix the ruleset or the job.

---

### Task 8 (optional): Try the hard block on hand-pushed tags

Only after Task 7 passed. Keep it if it works; drop it if GitHub refuses.

**Files:**
- Modify: `tools/github_settings.py`, `tools/tests/test_github_settings.py`,
  `book/src/contributing/releasing.md`

- [ ] **Step 1: Find out whether GitHub accepts the built-in token as a
  bypass.** Read GitHub's current docs on ruleset bypass actors. The
  candidate is the GitHub Actions app as an `Integration` bypass actor
  (`{"actor_id": 15368, "actor_type": "Integration", "bypass_mode": "always"}`);
  verify that ID against the docs or `gh api /apps/github-actions`.
- [ ] **Step 2: If it is accepted,** add a second tag ruleset
  `"Release tag creation": ruleset("tag", ["refs/tags/v*"], {"creation": {}})`
  with that one bypass actor, leaving `"Release tags"` exactly as it is.
  Update `test_desired_tags_allow_creation` to assert that `"Release tags"`
  still has no `creation` rule and no bypass, and add a test that
  `"Release tag creation"` has only `creation` and only that bypass actor.
  Try the two rulesets in a throwaway repository in the org first, with a
  tiny workflow that creates a tag with its built-in token: a hand push of
  `v0.0.1` must be refused and the workflow's tag must be accepted. (Testing
  the hand push on `irsync-dev/irsync` itself would leave a permanent stray
  tag if the block didn't work.) Then apply to `irsync-dev/irsync`; the next
  pre-release through the gate must still get its tag.
- [ ] **Step 3: If it is not accepted,** stop here. The stray-tag check from
  Tasks 1, 2 and 5 is the protection, as the spec allows. Note it in
  `releasing.md`.
- [ ] **Step 4: Commit** what was kept:
  `git commit -m "Block release tags not created by the release workflow"`.
