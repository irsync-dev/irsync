# dev-setup Prebuilt Tools Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `tools/dev-setup.sh` downloads cargo-nextest, cargo-deny, cargo-dist and mdbook as release binaries checked against SHA-256s in the script, and compiles only where no binary exists.

**Architecture:** One shell function, `fetch_tool`, does download, hash check and unpack for all five downloaded tools (actionlint included). Two small tables in the script hold the version and the hashes per tool and platform. `cargo_tool` tries the download first and keeps `cargo install` as the fallback.

**Tech Stack:** bash (`tools/dev-setup.sh`), Python `unittest` with fake tools on a bare `PATH` (`tools/tests/test_dev_setup.py`), GitHub Actions YAML.

**Spec:** `docs/superpowers/specs/2026-11-18-dev-setup-prebuilt-tools-design.md`

## Global Constraints

- Branch `dev-setup-prebuilt-tools`. PR against `main` with `Closes #35` in the description.
- No attribution lines of any kind in commits or the PR.
- A wrong hash never turns into a compile: the tool is `refused` and reported missing.
- A failed download (offline) never turns into a compile either.
- `--check` changes nothing and never calls curl.
- No new tool to install: only curl, tar and a SHA-256 program, as today.
- Pipes in the script never end in a reader that stops early (`head -1`, `grep -q`); see the comment at the top of the script.
- Versions keep coming from where they live today: nextest and deny from `.github/workflows/ci.yml`, dist from `dist-workspace.toml`, mdbook from `MDBOOK_VERSION` in `.github/workflows/site.yml`.
- `site/build.sh` and `site/serve.sh` are not touched.
- Run the tests with `python3 -m unittest discover -s tools/tests`. After the workflow change also `tools/ci.sh workflows`.

## Review Focus

1. **Rerun with everything at the right version:** no curl call, no cargo call. (Task 2, `test_setup_leaves_right_versions_alone`)
2. **A version was bumped but the hash table was not:** the script says so and compiles; it does not download with stale hashes. (Task 2, `test_setup_compiles_when_the_hashes_are_for_another_version`)
3. **A platform with no binary** (FreeBSD for deny, dist and mdbook; any other CPU): compiles as today, while nextest on FreeBSD is downloaded. (Task 2, `test_setup_on_freebsd_downloads_nextest_and_compiles_the_rest`)
4. **`~/.cargo/bin` is not on `PATH`:** the download succeeds but the tool is not found. The message must name the directory to add to `PATH`, not tell the user to run the script again. (Task 2, `test_setup_names_the_directory_missing_from_path`)
5. **An older copy that `cargo install` did not put there** (a downloaded one) meets the compile fallback: `cargo install` must not stop on "binary already exists". (Task 2: `--force` in the command, asserted in the fallback tests)

---

## File Structure

- `tools/dev-setup.sh`: gains `fetch_tool`, `tool_target`, `tool_hashes_for`, `tool_sha256`, `tool_archive`; `cargo_tool` and `install_actionlint` change.
- `tools/tests/test_dev_setup.py`: harness gains downloads that succeed; new tests.
- `.github/workflows/site.yml`: mdbook download is checked.
- `docs/SUPPLY_CHAIN_DECISIONS.md`, `book/src/contributing/setup.md`: words.

### Task 1: One download function, used by actionlint

No change in behavior. The existing tests are the test.

**Files:**
- Modify: `tools/dev-setup.sh` (`sha256_of` and `install_actionlint`, around lines 222–259; move `sha256_of` and the new function above `cargo_tool`)

**Interfaces:**
- Produces:

```sh
# fetch_tool NAME VERSION PLAT URL SHA256 MEMBER
# Downloads URL, checks it against SHA256 and puts MEMBER (a path inside the
# archive, .tar.gz or .tar.xz) into $cargo_bin under its base name.
# Prints "  running  download NAME VERSION (PLAT)", and on a wrong hash
# "  refused  NAME download: SHA-256 is GOT, expected WANT".
# Returns 0 installed, 1 could not download or unpack, 2 refused.
fetch_tool() { ... }
```

- [ ] **Step 1: Run the tests as they are**

Run: `python3 -m unittest discover -s tools/tests`
Expected: OK

- [ ] **Step 2: Write `fetch_tool` and make `install_actionlint` call it**

Unpack into the temporary directory, then move the one binary into `$cargo_bin` (create it if missing), so nothing else from an archive lands there. Remove the temporary directory on every path. Return 1 without calling curl when curl or tar is missing. `install_actionlint` keeps choosing `plat` and the hash, and returns 1 when `fetch_tool` does not return 0.

- [ ] **Step 3: Run the tests**

Run: `python3 -m unittest discover -s tools/tests`
Expected: OK, including the two `refuses_a_tampered_actionlint` tests unchanged.

- [ ] **Step 4: Commit**

```bash
git add tools/dev-setup.sh
git commit -m "dev-setup: one function downloads and checks a tool

actionlint uses it now; the cargo tools follow."
```

### Task 2: Download the four cargo tools, pin mdbook

**Files:**
- Modify: `tools/dev-setup.sh` (header comment lines 6–10, the `step "Rust ..."` title, `cargo_tool`, the four calls)
- Modify: `tools/tests/test_dev_setup.py`
- Modify: `book/src/contributing/setup.md:21-25`
- Modify: `docs/SUPPLY_CHAIN_DECISIONS.md` (new decision 17, before "## Left open")

**Interfaces:**
- Consumes: `fetch_tool` from Task 1.
- Produces, in `tools/dev-setup.sh`, in exactly this shape (the tests read it with regular expressions):

```sh
# The Rust target whose release binaries run here, or nothing.
#   Linux x86_64 -> x86_64-unknown-linux-musl    Linux aarch64|arm64 -> aarch64-unknown-linux-musl
#   Darwin x86_64 -> x86_64-apple-darwin         Darwin arm64 -> aarch64-apple-darwin
#   FreeBSD amd64|x86_64 -> x86_64-unknown-freebsd
tool_target() { ... }

# The version the hashes below are for.
tool_hashes_for() {
    case $1 in
        cargo-nextest) echo 0.9.151 ;;
        cargo-deny) echo 0.20.2 ;;
        cargo-dist) echo 0.34.0 ;;
        mdbook) echo 0.5.2 ;;
    esac
}
# The SHA-256 of a tool's release archive for a target, or nothing where
# there is none. nextest, deny and dist publish a .sha256 file next to each
# archive. mdbook publishes none: download its archives and hash them.
tool_sha256() {
    case $1:$2 in
        cargo-nextest:x86_64-unknown-linux-musl) echo 6b284d6fde9670dfb6ee10ea9d9abbc85f95643d3f592472d420d82e257b3a6c ;;
        cargo-nextest:aarch64-unknown-linux-musl) echo a9ee2d53cf0711d879f068c0ff814b6fe3af3f9be8799f8ab484e47571087160 ;;
        # One universal archive for both Macs.
        cargo-nextest:x86_64-apple-darwin | cargo-nextest:aarch64-apple-darwin) echo d1d7d025d4bfc9fb154cd76aea3949bd78534829a0589abefa6e5dd4edb135dd ;;
        cargo-nextest:x86_64-unknown-freebsd) echo a353c1c6a4488b2be2c3c584852b18caa6830b5fa2ccdc0349e3971418b01d86 ;;
        cargo-deny:x86_64-unknown-linux-musl) echo 9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f ;;
        cargo-deny:aarch64-unknown-linux-musl) echo 995c82be0defc7a025cae49a2aa2644ce8245c9a3318fc4103907c6a285e8c7d ;;
        cargo-deny:x86_64-apple-darwin) echo 248da7f581724e470071990c088ffc55c811981715f4cbdb258621fb79f8b7a6 ;;
        cargo-deny:aarch64-apple-darwin) echo fe67d82a10d8597a3549364cb733a3f9cc1bfff9031b7ae46384a9f2a72090c3 ;;
        cargo-dist:x86_64-unknown-linux-musl) echo f274b9889e93333a52cdcab86b93ab04b875b8ef4eebc7f8589514e0aff827e6 ;;
        cargo-dist:aarch64-unknown-linux-musl) echo aac7d48901f5283b53e136dd0498782c88a3350dfa4a5e88f4c3e0ba7e486eea ;;
        cargo-dist:x86_64-apple-darwin) echo 6aa89bb1e7c18f51ae8b148ce2fc6ab00ac46ed8173d4f2ed9afa12101218773 ;;
        cargo-dist:aarch64-apple-darwin) echo d1eec30e63eed23e5a5bd48fe96eae314799811bffca254ec1d2264638ba1890 ;;
        mdbook:x86_64-unknown-linux-musl) echo 8e9d802bbecea34380aabfc65bec4fdba23c07453ef2c7e06fa34c282082e2c4 ;;
        mdbook:aarch64-unknown-linux-musl) echo fb229bfdc68dd6c93692e64c502a67a8d3d2fc35c37c61fc72768843e6476add ;;
        mdbook:x86_64-apple-darwin) echo 17cc64478ec279a73881420e850bd8f9d460552e56b50159ff465bc97eb90d6c ;;
        mdbook:aarch64-apple-darwin) echo da2f55653e96e3f6e1c53e2e13e91cc0cfbce8ab971c2e0de792c0f1f8d24222 ;;
    esac
}

# tool_archive CRATE VERSION TARGET: prints "URL MEMBER".
tool_archive() { ... }
```

`tool_archive` values (V version, T target):

| Crate | URL | Member |
|---|---|---|
| cargo-nextest | `https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-V/cargo-nextest-V-T.tar.gz`, with `universal-apple-darwin` for T on both Macs | `cargo-nextest` |
| cargo-deny | `https://github.com/EmbarkStudios/cargo-deny/releases/download/V/cargo-deny-V-T.tar.gz` | `cargo-deny-V-T/cargo-deny` |
| cargo-dist | `https://github.com/axodotdev/cargo-dist/releases/download/vV/cargo-dist-T.tar.xz` | `cargo-dist-T/dist` |
| mdbook | `https://github.com/rust-lang/mdBook/releases/download/vV/mdbook-vV-T.tar.gz` | `mdbook` |

`cargo_tool CRATE PINNED-VERSION "VERSION-COMMAND" WHAT-FOR` loses its fifth argument (`any`). For a tool that is missing or at another version, outside `--check`:

| Case | What it does | Reported as |
|---|---|---|
| `tool_hashes_for` differs from the pin | prints `  note     CRATE: the hashes are for X, CI uses PIN; compiling`, then compiles | by version, as today |
| `tool_sha256` prints nothing | compiles | by version, as today |
| `fetch_tool` returns 0 | nothing more | `ok` |
| `fetch_tool` returns 0 but the version command still finds nothing | nothing more | `warn "CRATE PIN, WHAT-FOR; it is in $cargo_bin" "add $cargo_bin to PATH"` |
| `fetch_tool` returns 1 or 2 | no compile | `warn` with the fix `tools/dev-setup.sh` |

The compile command is `cargo install --locked CRATE --version PIN --force`, and it still needs `$rust_ready`. The download does not. The fix line printed for a missing tool is `tools/dev-setup.sh` where a hash exists for this platform and the compile command where none does.

- [ ] **Step 1: Extend the harness in `tools/tests/test_dev_setup.py`**

  - `setup(self, **tools)` runs with `PATH` = `self.bin` + `:` + `<tmp>/.cargo/bin`, and takes `path_has_cargo_bin=True` to leave the second part out.
  - Its fake `cargo` behaves like cargo for subcommands: when `cargo-$1` is on `PATH` it runs that with the same arguments. It still logs every call first.
  - A fake uname for a Mac and for an unknown CPU, next to `LINUX` and `FREEBSD`:
    `MAC = 'case "$1" in -m) echo arm64 ;; *) echo Darwin ;; esac'` and
    `RISCV = 'case "$1" in -m) echo riscv64 ;; *) echo Linux ;; esac'`.
  - `def pins(self) -> dict`: crate name to the version from `tool_hashes_for`, read from the script with a regular expression.
  - `def good_downloads(self) -> dict`: returns `{"curl": ..., "sha256sum": ...}` fakes. The curl builds, at its `-o` path, an archive with the member `tool_archive` names for the URL it was given (told apart by `nextest`, `cargo-deny`, `cargo-dist`, `mdBook`, `actionlint` in the URL; `.tar.xz` for dist), holding a shell script that prints `NAME PIN`. It also writes the hash the script expects for that URL to `$HOME/expected-hash`. The fake `sha256sum` prints that file's content. `setup()` must then not link the real `sha256sum`, `shasum` or `sha256` over it.

- [ ] **Step 2: Write the failing tests**

Replace `test_setup_replaces_another_version_but_keeps_any_mdbook`, and in `test_ci_tools_found` replace the "Any mdbook will do" assertion with
`self.assertRegex(out, r"missing +mdbook \d+\.\d+\.\d+, have 0\.0\.1.*\(optional\)")`.

```python
def test_tool_hash_versions_match_the_pins(self):
    # A version bump must bring its hashes along.
    def first(pattern, *path):
        with open(os.path.join(ROOT, *path)) as f:
            return re.search(pattern, f.read(), re.M).group(1)
    wf = (".github", "workflows")
    self.assertEqual(self.pins(), {
        "cargo-nextest": first(r"cargo-nextest@([0-9.]+)", *wf, "ci.yml"),
        "cargo-deny": first(r"cargo-deny@([0-9.]+)", *wf, "ci.yml"),
        "cargo-dist": first(r'^cargo-dist-version *= *"(.*)"', "dist-workspace.toml"),
        "mdbook": first(r"^ *MDBOOK_VERSION: *([0-9.]+)", *wf, "site.yml"),
    })

def test_setup_downloads_the_tools_and_does_not_compile(self):
    out, calls = self.setup(uname=LINUX, mdbook="echo mdbook v0.0.1", **self.good_downloads())
    for crate, pin in self.pins().items():
        self.assertRegex(out, rf"running +download {crate} {re.escape(pin)} \(x86_64-unknown-linux-musl\)")
        self.assertRegex(out, rf"ok +{crate} {re.escape(pin)}\n")
    self.assertNotIn("install", calls)
    for b in ["cargo-nextest", "cargo-deny", "dist", "mdbook", "actionlint"]:
        self.assertTrue(os.path.exists(os.path.join(self.tmp, ".cargo", "bin", b)), b)
    # Only the binary, not the archive's other files.
    self.assertEqual(len(os.listdir(os.path.join(self.tmp, ".cargo", "bin"))), 5)

def test_setup_on_a_mac_takes_the_universal_nextest(self):
    out, calls = self.setup(uname=MAC, **self.good_downloads())
    self.assertRegex(out, r"download cargo-nextest \S+ \(aarch64-apple-darwin\)")
    self.assertRegex(out, r"ok +cargo-nextest")
    self.assertNotIn("install", calls)

def test_setup_refuses_a_tampered_tool_and_does_not_compile(self):
    out, calls = self.setup(
        uname=LINUX,
        curl='while [ "$1" != -o ]; do shift; done; echo tampered > "$2"')
    for crate in self.pins():
        self.assertRegex(out, rf"refused +{crate} download: SHA-256 is")
        self.assertRegex(out, rf"missing +{crate} .*\n +tools/dev-setup\.sh\n")
    self.assertNotIn("install", calls)

def test_setup_offline_reports_and_does_not_compile(self):
    out, calls = self.setup(uname=LINUX, curl="exit 6")
    for crate in self.pins():
        self.assertRegex(out, rf"missing +{crate} .*\n +tools/dev-setup\.sh\n")
    self.assertNotIn("install", calls)

def test_setup_on_freebsd_downloads_nextest_and_compiles_the_rest(self):
    out, calls = self.setup(uname=FREEBSD, **self.good_downloads())
    self.assertRegex(out, r"download cargo-nextest \S+ \(x86_64-unknown-freebsd\)")
    self.assertNotIn("cargo-nextest", calls.replace("nextest --version", ""))
    for crate in ["cargo-deny", "cargo-dist", "mdbook"]:
        self.assertRegex(calls, rf"install --locked {crate} --version \d+\.\d+\.\d+ --force\n")

def test_setup_compiles_on_a_cpu_without_binaries(self):
    out, calls = self.setup(uname=RISCV, curl="exit 99")
    for crate in self.pins():
        self.assertRegex(calls, rf"install --locked {crate} --version \d+\.\d+\.\d+ --force\n")

def test_setup_leaves_right_versions_alone(self):
    p = self.pins()
    out, calls = self.setup(
        uname=LINUX, curl='echo "curl called" >&2; exit 99',
        cargo=f'case "$1" in nextest) echo "cargo-nextest {p["cargo-nextest"]}" ;; '
              f'deny) echo "cargo-deny {p["cargo-deny"]}" ;; esac',
        dist=f'echo "dist {p["cargo-dist"]}"', mdbook=f'echo "mdbook v{p["mdbook"]}"',
        actionlint=f"echo {self.actionlint_pin()}")
    self.assertNotIn("curl called", out)
    self.assertNotIn("install", calls)
    self.assertNotRegex(out, r"missing +(cargo-|mdbook|actionlint)")

def test_setup_names_the_directory_missing_from_path(self):
    out, calls = self.setup(uname=LINUX, path_has_cargo_bin=False, **self.good_downloads())
    self.assertRegex(out, r"missing +mdbook .*it is in \S+/\.cargo/bin.*\n +add \S+/\.cargo/bin to PATH\n")
    self.assertNotIn("install", calls)

def test_setup_compiles_when_the_hashes_are_for_another_version(self):
    # A copy of the script in a tree whose ci.yml pins another nextest.
    out, calls = self.setup_in_copy(nextest="9.9.9", uname=LINUX, **self.good_downloads())
    self.assertRegex(out, r"note +cargo-nextest: the hashes are for \S+, CI uses 9\.9\.9; compiling")
    self.assertNotIn("download cargo-nextest", out)
    self.assertIn("install --locked cargo-nextest --version 9.9.9 --force\n", calls)

@unittest.skipUnless(os.environ.get("DEV_SETUP_NETWORK") == "1"
                     and os.uname().sysname == "Linux" and os.uname().machine == "x86_64",
                     "set DEV_SETUP_NETWORK=1 on Linux x86_64: downloads the real tools")
def test_the_real_downloads_install_and_run(self):
    os.symlink(shutil.which("curl"), os.path.join(self.bin, "curl.real"))
    out, calls = self.setup(curl='exec curl.real "$@"')
    for crate, pin in self.pins().items():
        self.assertRegex(out, rf"ok +{crate} {re.escape(pin)}\n")
    self.assertRegex(out, r"ok +actionlint")
    self.assertNotIn("install", calls)
```

`setup_in_copy(self, nextest, **tools)`: copies `tools/dev-setup.sh` to `<tmp>/repo/tools/`, copies `rust-toolchain.toml`, `Cargo.toml`, `dist-workspace.toml`, `.github/workflows/site.yml` and `.github/scripts/install-packaging-tools.sh` beside it, writes a `ci.yml` that is the real one with the nextest version replaced, and runs that copy the way `setup()` does. Share the body with `setup()` through a `script=SCRIPT` parameter.

- [ ] **Step 3: Run the tests to see them fail**

Run: `python3 -m unittest discover -s tools/tests`
Expected: the new tests fail (`pins` finds no `tool_hashes_for`); the rest pass.

- [ ] **Step 4: Implement in `tools/dev-setup.sh`**

Add `tool_target`, `tool_hashes_for`, `tool_sha256` and `tool_archive` as given under Interfaces, and change `cargo_tool` to the table above. Call `cargo_tool mdbook ...` without `any`. Above the tables, a comment says how to move a tool to a new version: change the pin where it lives, change `tool_hashes_for`, replace that tool's hash lines, and `test_tool_hash_versions_match_the_pins` holds the two together. Update the header comment (mdbook is held to CI's version too; the tools are downloaded, and compiled only where there is no binary) and the section title to `step "Rust (rustup, and tools from their release pages)"`.

- [ ] **Step 5: Run the tests**

Run: `python3 -m unittest discover -s tools/tests`
Expected: OK (one skipped).

Run: `DEV_SETUP_NETWORK=1 python3 -m unittest tools.tests.test_dev_setup -k real_downloads` (from the repo root; if the module path does not resolve, `cd tools/tests` and use `test_dev_setup`)
Expected: OK on Linux x86_64. This is the proof that the 4 Linux x86_64 URLs, hashes and members are right. The other 10 hashes were copied from the releases' own `.sha256` files (mdbook: hashed from the downloads) on 2026-11-18.

- [ ] **Step 6: Update the words**

`book/src/contributing/setup.md`, the Requirements paragraph: the cargo tools, actionlint and mdbook are downloaded from their release pages and checked against SHA-256s in the script, and compiled only where a project has no binary for the platform; all of them are held to CI's versions. Remove "Any mdbook will do."

`docs/SUPPLY_CHAIN_DECISIONS.md`, new item before "## Left open", in the style of the others:

```markdown
17. **Developer tools are downloaded and checked against hashes in the
    repo.** `tools/dev-setup.sh` fetches cargo-nextest, cargo-deny, dist,
    mdbook and actionlint as release binaries and refuses one whose SHA-256
    is not the one in the script. A refused or failed download is reported;
    it never turns into a build from source. Why: building them took
    minutes on every new machine and version bump. cargo-binstall was
    turned down: it verifies minisign signatures only for a tool whose
    `Cargo.toml` names a key, and none of these does, so it would check
    nothing beyond HTTPS. Cost if wrong: the hashes are replaced by hand on
    each bump, and mdbook publishes none, so its hashes are our own first
    download. Revisit if the tools start signing.
```

- [ ] **Step 7: Commit**

```bash
git add tools/dev-setup.sh tools/tests/test_dev_setup.py book/src/contributing/setup.md docs/SUPPLY_CHAIN_DECISIONS.md
git commit -m "dev-setup: download the cargo tools instead of compiling them

Building nextest, deny, dist and mdbook took minutes on a fresh machine.
They now come from their release pages, checked against SHA-256s in the
script. mdbook is held to CI's version like the others."
```

### Task 3: CI checks its mdbook download

**Files:**
- Modify: `.github/workflows/site.yml:34` (env) and `:52-57` (the "Install mdbook" step)
- Test: `tools/tests/test_dev_setup.py`

**Interfaces:**
- Consumes: the `mdbook:x86_64-unknown-linux-musl) echo HASH ;;` line from Task 2.

- [ ] **Step 1: Write the failing test**

```python
def test_site_workflow_checks_mdbook_against_the_same_hash(self):
    with open(SCRIPT) as f:
        want = re.search(r"mdbook:x86_64-unknown-linux-musl\) echo ([0-9a-f]{64}) ;;", f.read()).group(1)
    with open(os.path.join(ROOT, ".github", "workflows", "site.yml")) as f:
        text = f.read()
    self.assertIn(f"MDBOOK_SHA256: {want}\n", text)
    self.assertIn("x86_64-unknown-linux-musl.tar.gz", text)
    self.assertIn("sha256sum --check", text)
```

- [ ] **Step 2: Run it to see it fail**

Run: `python3 -m unittest discover -s tools/tests -k site_workflow`
Expected: FAIL on `MDBOOK_SHA256`.

- [ ] **Step 3: Change `site.yml`**

Add `MDBOOK_SHA256: 8e9d802bbecea34380aabfc65bec4fdba23c07453ef2c7e06fa34c282082e2c4` under `MDBOOK_VERSION`, with a comment that both are kept in step with `tools/dev-setup.sh`. The step downloads the `x86_64-unknown-linux-musl` archive (not `-gnu`) to a file under `$RUNNER_TEMP`, runs `echo "$MDBOOK_SHA256  <file>" | sha256sum --check -`, and only then unpacks, with `set -euo pipefail` as in the "Install dist" step of `ci.yml`.

- [ ] **Step 4: Run the tests and the workflow checks**

Run: `python3 -m unittest discover -s tools/tests`
Expected: OK

Run: `tools/ci.sh workflows`
Expected: passes (actionlint and the workflow rule tests).

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/site.yml tools/tests/test_dev_setup.py
git commit -m "site: check the mdbook download against a SHA-256

The same hash tools/dev-setup.sh holds, so CI is not looser than a
developer's machine."
```

### Finish

- [ ] Run `tools/dev-setup.sh --check` on this machine. Expected: no curl call, the four tools and actionlint reported `ok` or with a `tools/dev-setup.sh` fix line.
- [ ] Push the branch and open the PR against `main` with `Closes #35`. The change touches a shell script, Python tests, a workflow and Markdown only: no Rust, so `tools/ci.sh check` is not needed locally beyond the two commands above.
