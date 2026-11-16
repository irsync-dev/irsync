# FreeBSD Manual Test Run and Build Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One command starts a GitHub run that tests a chosen irsync version in a FreeBSD VM and, if it passes, hands back `irsync-x86_64-unknown-freebsd.tar.xz`.

**Architecture:** A new workflow, `freebsd.yml`, starts only by hand. It checks out the chosen ref and boots `vmactions/freebsd-vm`. One script sets the VM up and runs `tools/ci.sh check freebsd-build` as an ordinary user. `freebsd-build` is a new `ci.sh` job: it reuses `packaging/dist/build.sh` for the build and a new `packaging/tarball.sh` for the packing.

**Tech Stack:** GitHub Actions, `vmactions/freebsd-vm` v1.6.0, POSIX sh and bash, Python `unittest` for the script and policy tests.

**Spec:** `docs/superpowers/specs/2026-11-16-freebsd-manual-build-design.md`

## Global Constraints

- Branch: `freebsd-manual-build` (exists; the spec is committed there).
- The workflow's only trigger is `workflow_dispatch`. It is never part of `ci.yml` or `CI result`, and no other workflow calls it.
- Every Action is pinned as `owner/repo@<40 hex> # vX.Y.Z`:
  - `actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1`
  - `vmactions/freebsd-vm@811efaaac0668df8bfb8308c3ec997b5ab50037b # v1.6.0`
  - `actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # v7.0.2`
- Target triple: `x86_64-unknown-freebsd`. x86_64 only.
- Tarball: `irsync-x86_64-unknown-freebsd.tar.xz`, holding one folder `irsync-x86_64-unknown-freebsd/` with `irsync`, `fileset`, `README.md`, `LICENSE`. Next to it `irsync-x86_64-unknown-freebsd.tar.xz.sha256`.
- Default FreeBSD release: `14`.
- Do not touch `crates/irsync/src/ssh/release.rs`, `dist-workspace.toml` or `release.yml`.
- FreeBSD commands live in scripts, not in the workflow file.
- Commit messages: a short summary line, a blank line, then a brief plain-English why if it isn't obvious. No attribution trailers of any kind.
- Docs use plain, short sentences, like the pages around them.
- Before the last commit of each task that touches scripts or workflows: `tools/ci.sh workflows` and `python3 -m unittest discover -s tools/tests` pass.

## Review Focus

1. **`ref` left empty.** The run tests the branch it was started on. Pinned in Task 3 (`test_ref_defaults_to_the_started_branch`).
2. **A version older than this change.** It has no `freebsd-vm.sh`. The run stops before the VM boots, with a message that says so. Pinned in Task 3 (`test_old_versions_stop_with_a_message`).
3. **Tests fail in the VM.** No tarball is uploaded, and a run that passed but made no tarball fails. Pinned in Task 3 (`test_upload_only_after_success`).
4. **A `ref` with shell characters in it.** Inputs never reach a `run:` script as `${{ … }}` text. Pinned in Task 3 (`test_inputs_never_expand_in_scripts`).
5. **A binary missing at packing time.** `tarball.sh` fails and leaves no tarball behind. Pinned in Task 1 (`test_missing_binary_fails_and_leaves_nothing`).

---

### Task 1: `packaging/tarball.sh`

**Files:**
- Create: `packaging/tarball.sh` (executable, `#!/bin/sh`, `set -eu`)
- Test: `packaging/tests/test_tarball.py`

**Interfaces:**
- Produces: `packaging/tarball.sh TARGET BIN_DIR OUT_DIR`. Reads `BIN_DIR/irsync`, `BIN_DIR/fileset`, and `README.md` and `LICENSE` from the repo root. Writes `OUT_DIR/irsync-TARGET.tar.xz` and `OUT_DIR/irsync-TARGET.tar.xz.sha256`. Creates `OUT_DIR` if needed. Exits non-zero with a message on stderr that starts `tarball.sh:` when an argument or input file is missing.

- [ ] **Step 1: Write the failing tests**

In `packaging/tests/test_tarball.py`, following the style of `test_build_script.py` (temp dir, `subprocess.run`, `ROOT`). `setUp` makes a temp `bin` dir with two small executable files named `irsync` and `fileset`, and an empty `out` path.

```python
T = "x86_64-unknown-freebsd"

def test_layout_matches_the_release_tarballs(self):
    self.run_ok(T, self.bin, self.out)
    with tarfile.open(os.path.join(self.out, f"irsync-{T}.tar.xz")) as tar:
        files = sorted(m.name for m in tar.getmembers() if m.isfile())
        self.assertEqual(files, [f"irsync-{T}/{n}" for n in
                                 ["LICENSE", "README.md", "fileset", "irsync"]])
        for n in ["irsync", "fileset"]:
            self.assertTrue(tar.getmember(f"irsync-{T}/{n}").mode & 0o111)

def test_sha256_file_names_the_tarball(self):
    self.run_ok(T, self.bin, self.out)
    name = f"irsync-{T}.tar.xz"
    with open(os.path.join(self.out, name), "rb") as f:
        digest = hashlib.sha256(f.read()).hexdigest()
    with open(os.path.join(self.out, name + ".sha256")) as f:
        self.assertEqual(f.read(), f"{digest}  {name}\n")

def test_missing_binary_fails_and_leaves_nothing(self):
    os.remove(os.path.join(self.bin, "fileset"))
    r = self.run_script(T, self.bin, self.out)
    self.assertNotEqual(r.returncode, 0)
    self.assertIn("tarball.sh:", r.stderr)
    self.assertFalse(os.path.exists(os.path.join(self.out, f"irsync-{T}.tar.xz")))

def test_wrong_argument_count_fails(self):
    self.assertNotEqual(self.run_script(T).returncode, 0)
```

- [ ] **Step 2: Run them and see them fail**

Run: `python3 -m unittest discover -s packaging/tests -p 'test_tarball.py' -v`
Expected: 4 failures or errors (no such file `packaging/tarball.sh`).

- [ ] **Step 3: Write `packaging/tarball.sh`**

Check the inputs first, before writing anything. Stage the four files in a temp folder named `irsync-TARGET`, pack with `tar -cJf` (works with GNU tar and FreeBSD's tar), then write the checksum. For the digest use `sha256sum` when it is on `PATH`, else `sha256 -q` (FreeBSD); write the line yourself so the format is `<digest>  <name>` on both. Remove the temp folder on exit. Start the file with a header comment that says what it is for and that `tools/ci.sh freebsd-build` runs it.

- [ ] **Step 4: Run the tests and see them pass**

Run: `python3 -m unittest discover -s packaging/tests -p 'test_tarball.py' -v`
Expected: 4 tests, OK.

- [ ] **Step 5: Commit**

```bash
git add packaging/tarball.sh packaging/tests/test_tarball.py
git commit -m "Add packaging/tarball.sh, which packs a release-style tarball" \
  -m "dist cannot build on FreeBSD yet, so the FreeBSD build packs its own tarball with the same layout."
```

---

### Task 2: the `freebsd-build` job in `tools/ci.sh`

**Files:**
- Modify: `tools/ci.sh` (usage header, `all_jobs`, the argument `case`, the no-job default, a `needs_freebsd_build`, a `job_freebsd_build`)
- Create: `tools/tests/test_ci_script.py`
- Modify: `book/src/contributing/setup.md` (next to the `release-build` paragraph, about line 73), `AGENTS.md` (the paragraph that lists `deny`, `workflows`, `release-build`)

**Interfaces:**
- Consumes: `packaging/tarball.sh TARGET BIN_DIR OUT_DIR` from Task 1; `packaging/dist/build.sh`, which builds for `$CARGO_DIST_TARGET` and leaves `irsync` and `fileset` in `packaging/dist/`.
- Produces: `tools/ci.sh freebsd-build`. On FreeBSD x86_64 (`uname -s` = `FreeBSD`, `uname -m` = `amd64`) it leaves the tarball and `.sha256` in `target/distrib/`. Anywhere else it exits 1 with `ci.sh: freebsd-build runs on FreeBSD x86_64 only` on stderr, before any job starts.

- [ ] **Step 1: Write the failing tests**

`tools/tests/test_ci_script.py`. `tools/ci.sh check` already runs everything under `tools/tests`.

```python
CI = os.path.join(ROOT, "tools", "ci.sh")

class CiScriptTests(unittest.TestCase):
    def test_help_lists_freebsd_build(self):
        r = subprocess.run([CI, "--help"], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0)
        self.assertRegex(r.stdout, r"(?m)^  freebsd-build ")

    @unittest.skipIf(platform.system() == "FreeBSD", "the job runs there")
    def test_freebsd_build_refuses_elsewhere(self):
        r = subprocess.run([CI, "freebsd-build"], capture_output=True, text=True)
        self.assertEqual(r.returncode, 1)
        self.assertIn("ci.sh: freebsd-build runs on FreeBSD x86_64 only", r.stderr)

    def test_job_builds_then_packs(self):
        with open(CI) as f:
            body = re.search(r"^job_freebsd_build\(\) \{\n(.*?)^\}", f.read(), re.M | re.S)
        self.assertIsNotNone(body)
        lines = [l.strip() for l in body.group(1).splitlines()]
        build = lines.index("run env CARGO_DIST_TARGET=x86_64-unknown-freebsd packaging/dist/build.sh")
        pack = lines.index("run packaging/tarball.sh x86_64-unknown-freebsd packaging/dist target/distrib")
        self.assertLess(build, pack)
```

- [ ] **Step 2: Run them and see them fail**

Run: `python3 -m unittest discover -s tools/tests -p 'test_ci_script.py' -v`
Expected: 3 failures (`unknown job 'freebsd-build'`, no help line, no function).

- [ ] **Step 3: Change `tools/ci.sh`**

- Usage header: add a `freebsd-build` line (`the FreeBSD tarball; FreeBSD x86_64 only`) and say that a run of everything includes it only on FreeBSD.
- Accept `freebsd-build` as a job name.
- No job given: on FreeBSD run `workflows deny check freebsd-build`; elsewhere as today. `release-build` stays Linux only.
- `needs_freebsd_build`: the OS and machine check above, in the style of `needs_release_build`.
- `job_freebsd_build`: the two `run` lines from the test, in that order. After them, list the tarball with `tar -tJf` and fail unless both `irsync-x86_64-unknown-freebsd/irsync` and `…/fileset` are in it, as `job_release_build` does.

- [ ] **Step 4: Run the tests and see them pass**

Run: `python3 -m unittest discover -s tools/tests -v`
Expected: all OK, the three new tests among them.

- [ ] **Step 5: Update the docs**

- `book/src/contributing/setup.md`: after the `release-build` paragraph, two or three sentences. The `freebsd-build` job builds the FreeBSD tarball; it runs on FreeBSD x86_64 only; GitHub runs it in the hand-started FreeBSD workflow (link to the section Task 3 adds: `releasing.md#testing-a-version-on-freebsd`).
- `AGENTS.md`: add `freebsd-build` (FreeBSD only) to the sentence that lists the other jobs.

- [ ] **Step 6: Commit**

```bash
git add tools/ci.sh tools/tests/test_ci_script.py book/src/contributing/setup.md AGENTS.md
git commit -m "Add the freebsd-build job to tools/ci.sh" \
  -m "It runs the release build script for x86_64-unknown-freebsd and packs the tarball, so the FreeBSD build can be run by hand on a FreeBSD machine as well as in the VM."
```

---

### Task 3: the VM script, the workflow and its rules

**Files:**
- Create: `.github/scripts/freebsd-vm.sh` (executable, `#!/bin/sh`, `set -eu`)
- Create: `.github/workflows/freebsd.yml`
- Modify: `.github/scripts/tests/test_workflow_policy.py` (new class `FreeBsdTests`)
- Modify: `book/src/contributing/releasing.md` (new section `## Testing a version on FreeBSD`)

**Interfaces:**
- Consumes: `tools/ci.sh check freebsd-build` from Task 2, which leaves `target/distrib/irsync-x86_64-unknown-freebsd.tar.xz` and `.sha256`.
- Produces: `.github/scripts/freebsd-vm.sh`, run with no arguments from the checkout. After a passing run the two files are in `freebsd-out/` in the directory it was started from.

**`.github/scripts/freebsd-vm.sh`** has two halves, chosen by `id -u`:

*As root* (how the VM action starts it):
1. `pkg install -y bash curl git python3 rsync rustup-init`. If the build later asks for another package, add it here.
2. Create the user `ci` with a home directory, unless it exists (`pw useradd ci -m`).
3. Copy the checkout, `.git` included, to `/home/ci/irsync` (remove an old copy first) and give it to `ci`. The build runs on the VM's own disk and outside the synced folder, so the VM action does not copy gigabytes of `target/` back to the runner.
4. Run this same script as `ci` from `/home/ci/irsync` (`su -l ci -c …`).
5. Copy the two files from `/home/ci/irsync/target/distrib/` to `freebsd-out/` in the starting directory.

*As `ci`:*
1. `rustup-init -y --default-toolchain none`, load `$HOME/.cargo/env`, then `rustup toolchain install` (it reads `rust-toolchain.toml`). If FreeBSD's packages have no `rustup-init`, use `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none` and say so in a comment.
2. `cargo install --locked "cargo-nextest@$NEXTEST_VERSION"`, with `NEXTEST_VERSION=0.9.151` set at the top of the script, with the comment `# Keep equal to cargo-nextest in ci.yml.`
3. `tools/ci.sh check freebsd-build`

**`.github/workflows/freebsd.yml`:**

```yaml
name: FreeBSD
run-name: FreeBSD ${{ inputs.freebsd }}, ${{ inputs.ref || github.ref_name }}

on:
  workflow_dispatch:
    inputs:
      ref:
        description: 'Tag or branch to test. Empty means the branch this run starts on.'
        type: string
        required: false
      freebsd:
        description: FreeBSD release
        type: string
        default: '14'

permissions:
  contents: read

jobs:
  freebsd:
    name: FreeBSD ${{ inputs.freebsd }} x86_64
    runs-on: ubuntu-latest
    timeout-minutes: 120
    steps:
      # checkout: with ref: ${{ inputs.ref || github.ref }}, fetch-depth: 0,
      #   persist-credentials: false
      # "Check that this version has the FreeBSD build": a run: step that
      #   fails with the ::error:: line below when
      #   .github/scripts/freebsd-vm.sh is missing or not executable
      # vmactions/freebsd-vm: with release: ${{ inputs.freebsd }},
      #   arch: x86_64, sync: rsync, run: .github/scripts/freebsd-vm.sh
      # upload-artifact: name: irsync-x86_64-unknown-freebsd,
      #   path: freebsd-out/, if-no-files-found: error; no `if:`
```

The error line for an old version, exactly:
`::error::This version has no FreeBSD build (.github/scripts/freebsd-vm.sh is missing). Test a version from after it was added.`

Open the file with a comment block in the style of the other workflows: what it is for, that it only starts by hand, that it must never become part of `CI result`, and that the commands are in `freebsd-vm.sh` and `tools/ci.sh`.

- [ ] **Step 1: Write the failing policy tests**

Add to `.github/scripts/tests/test_workflow_policy.py`, using its helpers (`load_workflows`, `triggers`, `jobs`, `read`, `job_text`):

```python
class FreeBsdTests(unittest.TestCase):
    def wf(self):
        return load_workflows()["freebsd.yml"]

    def steps(self):
        return jobs(self.wf())["freebsd"]["steps"]

    def step_using(self, action):
        (s,) = [s for s in self.steps() if s.get("uses", "").startswith(action + "@")]
        return s

    def test_starts_only_by_hand(self):
        on = triggers(self.wf())
        self.assertEqual(set(on), {"workflow_dispatch"})
        self.assertEqual(set(on["workflow_dispatch"]["inputs"]), {"ref", "freebsd"})
        self.assertEqual(on["workflow_dispatch"]["inputs"]["freebsd"]["default"], "14")

    def test_no_workflow_calls_it(self):
        for name in workflow_files():
            if name != "freebsd.yml":
                self.assertNotIn("freebsd.yml", read(name), name)

    def test_read_only_and_time_limited(self):
        self.assertEqual(self.wf()["permissions"], {"contents": "read"})
        job = jobs(self.wf())["freebsd"]
        self.assertNotIn("permissions", job)
        self.assertEqual(job["timeout-minutes"], 120)

    def test_ref_defaults_to_the_started_branch(self):
        with_ = self.step_using("actions/checkout")["with"]
        self.assertEqual(with_["ref"], "${{ inputs.ref || github.ref }}")
        self.assertIs(with_["persist-credentials"], False)

    def test_old_versions_stop_with_a_message(self):
        steps = self.steps()
        vm = steps.index(self.step_using("vmactions/freebsd-vm"))
        guards = [i for i, s in enumerate(steps)
                  if "This version has no FreeBSD build" in s.get("run", "")]
        self.assertEqual(len(guards), 1)
        self.assertLess(guards[0], vm)

    def test_vm_runs_the_script(self):
        with_ = self.step_using("vmactions/freebsd-vm")["with"]
        self.assertEqual(with_["run"].strip(), ".github/scripts/freebsd-vm.sh")
        self.assertEqual(with_["release"], "${{ inputs.freebsd }}")
        self.assertEqual(with_["arch"], "x86_64")

    def test_upload_only_after_success(self):
        up = self.step_using("actions/upload-artifact")
        self.assertNotIn("if", up)
        self.assertIs(self.steps()[-1], up)
        self.assertEqual(up["with"]["if-no-files-found"], "error")
        self.assertEqual(up["with"]["path"].rstrip("/"), "freebsd-out")

    def test_inputs_never_expand_in_scripts(self):
        for s in self.steps():
            self.assertNotIn("${{", s.get("run", ""))

    def test_script_runs_the_ci_jobs_with_cis_nextest(self):
        with open(os.path.join(ROOT, ".github", "scripts", "freebsd-vm.sh")) as f:
            script = f.read()
        self.assertRegex(script, r"(?m)^\s*tools/ci\.sh check freebsd-build$")
        ci = re.search(r"cargo-nextest@([\d.]+)", read("ci.yml")).group(1)
        self.assertRegex(script, rf"(?m)^NEXTEST_VERSION={re.escape(ci)}$")
```

- [ ] **Step 2: Run them and see them fail**

Run: `python3 -m unittest discover -s .github/scripts/tests -p 'test_workflow_policy.py' -v`
Expected: the nine `FreeBsdTests` error with `KeyError: 'freebsd.yml'` or a missing file; everything else passes.

- [ ] **Step 3: Write `.github/scripts/freebsd-vm.sh` and `.github/workflows/freebsd.yml`** as specified above.

- [ ] **Step 4: Run the workflow checks**

Run: `tools/ci.sh workflows` and `sh -n .github/scripts/freebsd-vm.sh`
Expected: `ci.sh: passed: workflows`; no output from `sh -n`. The existing `test_third_party_actions_pinned` now also covers `freebsd.yml`.

- [ ] **Step 5: Write the docs section**

In `book/src/contributing/releasing.md`, a new section `## Testing a version on FreeBSD`, placed after the sections about cutting a release. It says, in this order:

- FreeBSD is not a release target yet. This run checks one version by hand and gives you a tarball to try. It never blocks a pull request or a release.
- How to start it and fetch the result:

  ```sh
  gh workflow run freebsd.yml -f ref=v0.1.0-beta.4
  gh run watch
  gh run download <run-id> -n irsync-x86_64-unknown-freebsd
  ```

- `-f freebsd=15` picks another FreeBSD release; the default is 14, because a binary built on 14 also runs on newer releases.
- What it runs: `tools/ci.sh check freebsd-build` as an ordinary user, with the Rust version from `rust-toolchain.toml`. A failing test means no tarball.
- Limits: x86_64 only; a version from before the workflow existed stops with a message; the tarball is not on the release page and has no attestation; irsync does not upload itself to a FreeBSD server over ssh, so install the binaries there by hand.

- [ ] **Step 6: Commit**

```bash
git add .github/scripts/freebsd-vm.sh .github/workflows/freebsd.yml \
  .github/scripts/tests/test_workflow_policy.py book/src/contributing/releasing.md
git commit -m "Add a FreeBSD test run and build, started by hand" \
  -m "Tests a chosen version in a FreeBSD VM and hands back a tarball. It only starts by hand and is not part of CI result, so it cannot block a pull request or a release."
```

- [ ] **Step 7: Full check before pushing**

Run: `tools/ci.sh check workflows` and `PACKAGING_TESTS_REQUIRED=1 python3 -m unittest discover -s packaging/tests`
Expected: `ci.sh: passed: check workflows`; packaging tests OK. If the packaging run fails only because a packaging tool is not installed here, run it without `PACKAGING_TESTS_REQUIRED` and say so in the report.

---

### Task 4: first run and triage

The workflow can only be started once it is on `main`. This task has two stops where Mike decides.

- [ ] **Step 1: Stop. Ask Mike before pushing the branch and opening the pull request.** Nothing has run on FreeBSD yet; say so in the pull request text. No attribution lines.

- [ ] **Step 2: After the merge, start a run on `main`**

```sh
gh workflow run freebsd.yml
gh run watch
```

- [ ] **Step 3: Sort what fails.** Read the log (`gh run view --log-failed`). Put each failure in one of three piles:

  - *The run itself:* a missing package, a wrong path, the user switch, the copy back. Fix in `freebsd-vm.sh` or the workflow.
  - *Small portability fixes:* a test or tool that assumes Linux or macOS where the FreeBSD answer is obvious and irsync's behaviour stays the same.
  - *Mike's call:* anything that changes what irsync does on FreeBSD, or that means leaving a test out there (for example a FreeBSD nextest profile like `ci-macos`, or netstress fault binaries that cannot work).

- [ ] **Step 4: Fix the first two piles** in one or more pull requests, each with the usual checks. Use superpowers:systematic-debugging for any failure whose cause is not plain from the log. Re-run the workflow after each merge.

- [ ] **Step 5: Stop. Bring the third pile to Mike** as a short list: what fails, why, and what you would do.

- [ ] **Step 6: When a run is green,** download the tarball, check it against its `.sha256`, list its contents, and report the run's link and how long it took.
