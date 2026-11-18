"""Tests for tools/dev-setup.sh --check, against fake tools on a bare PATH."""
import os
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SCRIPT = os.path.join(ROOT, "tools", "dev-setup.sh")

# Real programs the script itself runs; everything else on PATH is a fake.
UTILS = ["bash", "env", "sh", "dirname", "uname", "sed", "grep", "awk", "head",
         "tr", "cat", "cut", "mktemp", "rm", "tar"]
# Real programs that are linked in only where they exist: GNU tar runs xz as
# a program, macOS's tar has it built in and ships no xz.
OPTIONAL_UTILS = ["xz"]

GOOD = {
    "cc": "echo cc",
    "git": 'if [ "$1" = -C ]; then echo .githooks; else echo "git version 2.50.0"; fi',
    "rsync": "echo 'rsync  version 3.4.1  protocol version 32'",
    "python3": "echo 'Python 3.12.3'",
    "pandoc": "echo pandoc 3.1.3",
    "mandoc": "echo mandoc",
    # The pinned toolchain, whatever its version, with rustfmt, clippy and both musl targets.
    "rustup": """case "$1 $2" in
  "run "*) echo "rustc 1.97.0 (abc 2026-10-30)" ;;
  "component list") printf 'rustfmt-x\\nclippy-x\\n' ;;
  "target list") printf 'x86_64-unknown-linux-musl\\naarch64-unknown-linux-musl\\n' ;;
  *) echo "rustup 1.29.3" ;;
esac""",
}

# Fake `uname`s, so a test of one OS's branch runs on every OS.
LINUX = 'case "$1" in -m) echo x86_64 ;; *) echo Linux ;; esac'
FREEBSD = 'case "$1" in -m) echo amd64 ;; *) echo FreeBSD ;; esac'
MAC = 'case "$1" in -m) echo arm64 ;; *) echo Darwin ;; esac'
# A CPU no tool has a release binary for.
RISCV = 'case "$1" in -m) echo riscv64 ;; *) echo Linux ;; esac'

# The part of its download URL that tells a tool apart.
URL_KEY = {"cargo-nextest": "nextest-rs", "cargo-deny": "cargo-deny",
           "cargo-dist": "cargo-dist", "mdbook": "mdBook"}


class DevSetupCheckTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="dev-setup-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.bin = os.path.join(self.tmp, "bin")
        os.mkdir(self.bin)
        for u in UTILS:
            os.symlink(shutil.which(u), os.path.join(self.bin, u))
        for u in OPTIONAL_UTILS:
            if shutil.which(u):
                os.symlink(shutil.which(u), os.path.join(self.bin, u))
        # Must never run in --check mode: it would install rustup.
        self.fake("curl", 'echo "curl called" >&2; exit 99')

    def fake(self, name, body):
        path = os.path.join(self.bin, name)
        # Never write through a symlink to the real program.
        if os.path.lexists(path):
            os.remove(path)
        with open(path, "w") as f:
            f.write(f"#!/bin/sh\n{body}\n")
        os.chmod(path, 0o755)

    def check(self, **tools):
        for name, body in {**GOOD, **tools}.items():
            if body is None:
                p = os.path.join(self.bin, name)
                if os.path.exists(p):
                    os.remove(p)
            else:
                self.fake(name, body)
        env = {"PATH": self.bin, "HOME": self.tmp}
        r = subprocess.run([SCRIPT, "--check"], env=env, capture_output=True, text=True)
        self.assertNotIn("curl called", r.stderr)
        return r, r.stdout + r.stderr

    def test_good_system_tools_pass(self):
        r, out = self.check()
        for tool in ["C compiler", "git", "GNU rsync", "Python", "curl", "tar with xz"]:
            self.assertRegex(out, rf"ok +{tool}", out)

    def test_tar_without_xz_names_the_package(self):
        r, out = self.check(tar="exit 2")
        self.assertNotEqual(r.returncode, 0)
        self.assertRegex(out, r"missing +xz, so tar can write and read \.tar\.xz.*\(required\)")
        self.assertRegex(out, r"xz-utils|brew install xz|pkg install xz")

    def test_missing_tar_and_curl_are_required(self):
        os.remove(os.path.join(self.bin, "curl"))
        r, out = self.check(tar=None)
        self.assertNotEqual(r.returncode, 0)
        self.assertRegex(out, r"missing +tar.*\(required\)")
        self.assertRegex(out, r"missing +curl.*\(required\)")

    def test_old_and_bsd_rsync_are_refused(self):
        for line in ["rsync  version 2.6.9  protocol version 29",
                     "openrsync: protocol version 29"]:
            r, out = self.check(rsync=f"echo '{line}'")
            self.assertNotEqual(r.returncode, 0, line)
            self.assertIn("rsync 3.1 or newer", out, line)

    def test_long_version_output_is_read_to_the_end(self):
        # Real rsync --version prints a page. A reader that stops after the
        # first line kills rsync with SIGPIPE, which pipefail made fatal.
        r, out = self.check(rsync="echo 'rsync  version 3.4.1  protocol version 32'\n"
                                  "i=0; while [ $i -lt 5000 ]; do echo filler line $i; i=$((i+1)); done")
        self.assertRegex(out, r"ok +GNU rsync 3\.4\.1", out)
        self.assertIn("Ready.", out)

    def test_man_page_tools_are_required_and_named(self):
        # tools/ci.sh check stops without either, so neither is optional.
        r, out = self.check()
        self.assertEqual(r.returncode, 0, out)
        self.assertRegex(out, r"ok +pandoc\n")
        self.assertRegex(out, r"ok +mandoc\n")
        for tool, install in [
            ("pandoc", "brew install pandoc|apt install pandoc|pkg install hs-pandoc"),
            ("mandoc", "brew install mandoc|apt install mandoc|pkg install mandoc"),
        ]:
            r, out = self.check(**{tool: None})
            self.assertNotEqual(r.returncode, 0, tool)
            self.assertRegex(out, rf"missing +{tool}, for the man pages \(required\)")
            self.assertRegex(out, install)
            self.assertNotIn("Ready.", out)

    def test_old_python_is_refused(self):
        r, out = self.check(python3="echo 'Python 3.10.12'")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("Python 3.11 or newer", out)

    def test_missing_compiler_names_the_package(self):
        r, out = self.check(cc=None)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("C compiler", out)
        self.assertRegex(out, r"build-essential|xcode-select|pkg install llvm")

    def test_missing_rustup_is_reported_not_installed(self):
        r, out = self.check(rustup=None)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("rustup", out)

    def test_rustup_that_path_does_not_lead_to_is_reported(self):
        # rustup is in ~/.cargo/bin and PATH does not lead there. The script
        # finds it for its own run; the shell that started it will not.
        cargo_bin = os.path.join(self.tmp, ".cargo", "bin")
        os.makedirs(cargo_bin)
        with open(os.path.join(cargo_bin, "rustup"), "w") as f:
            f.write(f"#!/bin/sh\n{GOOD['rustup']}\n")
        os.chmod(os.path.join(cargo_bin, "rustup"), 0o755)
        r, out = self.check(rustup=None)
        self.assertNotEqual(r.returncode, 0)
        self.assertNotRegex(out, r"ok +rustup")
        self.assertRegex(out, r"missing +rustup on PATH; it is in \S+/\.cargo/bin.*\(required\)\n"
                              r" +add \S+/\.cargo/bin to PATH\n")
        # The rest is still checked, with the rustup that was found.
        self.assertRegex(out, r"ok +Rust ")

    def test_optional_tools_do_not_fail_the_check(self):
        # No node, cargo tools or packaging tools here; only required ones
        # decide the exit status.
        r, out = self.check()
        self.assertEqual(r.returncode, 0, out)
        self.assertRegex(out, r"missing +node and npx.*\(optional\)")
        self.assertRegex(out, r"missing +mdbook.*\(optional\)")
        self.assertIn("Ready.", out)

    def test_minisign_is_optional_and_named(self):
        r, out = self.check()
        self.assertEqual(r.returncode, 0, out)
        self.assertRegex(out, r"missing  minisign 0\.10 or newer, to sign releases \(optional\)\n"
                              r" {15}(sudo apt install minisign|brew install minisign|sudo pkg install minisign)\n")

    def test_old_minisign_is_reported(self):
        r, out = self.check(minisign="echo minisign 0.9")
        self.assertEqual(r.returncode, 0, out)
        self.assertRegex(out, r"missing  minisign 0\.10 or newer, have 0\.9, to sign releases \(optional\)")

    def test_minisign_found(self):
        for line, version in [("minisign 0.11", "0.11"), ("minisign 0.12.1", "0.12.1")]:
            r, out = self.check(minisign=f'[ "$1" = -v ] && echo {line}')
            self.assertRegex(out, rf"ok +minisign {re.escape(version)}\n", line)

    def test_ci_tools_are_optional_and_named(self):
        # What tools/ci.sh needs beyond the build: none of it is here.
        r, out = self.check(python3='case "$1" in -c) exit 1 ;; *) echo "Python 3.12.3" ;; esac')
        self.assertEqual(r.returncode, 0, out)
        for tool in ["cargo-nextest", "cargo-deny", "cargo-dist", "PyYAML", "actionlint"]:
            self.assertRegex(out, rf"missing +{tool}.*tools/ci\.sh.*\(optional\)")

    def test_ci_tools_found(self):
        r, out = self.check(actionlint=f"echo {self.actionlint_pin()}; echo 'installed by building from source'",
                            cargo='[ "$1" = deny ] && echo "cargo-deny 0.1.0"',
                            mdbook="echo mdbook v0.0.1")
        self.assertRegex(out, r"ok +PyYAML")
        self.assertRegex(out, rf"ok +actionlint {re.escape(self.actionlint_pin())}")
        # Another version than CI's is reported, with the one CI uses.
        self.assertRegex(out, r"missing +cargo-deny \d+\.\d+\.\d+, have 0\.1\.0.*\(optional\)")
        # mdbook is held to CI's version too.
        self.assertRegex(out, r"missing +mdbook \d+\.\d+\.\d+, have 0\.0\.1.*\(optional\)")

    def test_another_actionlint_version_is_reported(self):
        r, out = self.check(actionlint="echo 0.0.1")
        self.assertEqual(r.returncode, 0, out)
        self.assertRegex(out, r"missing +actionlint \S+, have 0\.0\.1.*\(optional\)")

    def actionlint_pin(self):
        with open(SCRIPT) as f:
            return re.search(r"^actionlint_pin=(\S+)$", f.read(), re.M).group(1)

    def test_actionlint_pin_matches_the_workflows(self):
        with open(SCRIPT) as f:
            script = f.read()
        amd64 = re.search(r"linux_amd64\) echo ([0-9a-f]{64}) ;;", script).group(1)
        for wf in ["ci.yml", "release-tooling.yml"]:
            with open(os.path.join(ROOT, ".github", "workflows", wf)) as f:
                text = f.read()
            self.assertIn(f"ACTIONLINT_VERSION: {self.actionlint_pin()}\n", text, wf)
            self.assertIn(f"ACTIONLINT_SHA256: {amd64}\n", text, wf)

    def setup(self, script=SCRIPT, path_has_cargo_bin=True, **tools):
        """Runs the script without --check, with a cargo that records its calls.

        Like cargo, the fake runs cargo-NAME for the subcommand NAME when
        that is on PATH. ~/.cargo/bin is on PATH, ahead of the fakes.
        """
        log = os.path.join(self.tmp, "cargo.log")
        for name, body in {**GOOD, **tools}.items():
            self.fake(name, body)
        self.fake("cargo", f'echo "$*" >> {log}\n' + tools.get("cargo", "") + """
if command -v "cargo-$1" >/dev/null 2>&1; then exec "cargo-$1" "$@"; fi
exit 0""")
        for u in ["sha256sum", "shasum", "sha256", "mkdir", "mv", "chmod", "gzip"]:
            # A test's own fake stays.
            if shutil.which(u) and not os.path.lexists(os.path.join(self.bin, u)):
                os.symlink(shutil.which(u), os.path.join(self.bin, u))
        path = self.bin
        if path_has_cargo_bin:
            path = os.path.join(self.tmp, ".cargo", "bin") + ":" + path
        self.scratch = os.path.join(self.tmp, "scratch")
        os.makedirs(self.scratch, exist_ok=True)
        r = subprocess.run([script], env={"PATH": path, "HOME": self.tmp, "TMPDIR": self.scratch},
                           capture_output=True, text=True)
        calls = ""
        if os.path.exists(log):
            with open(log) as f:
                calls = f.read()
        return r.stdout + r.stderr, calls

    def setup_in_copy(self, nextest, **tools):
        """Runs a copy of the script in a tree whose ci.yml pins another nextest."""
        repo = os.path.join(self.tmp, "repo")
        files = ["tools/dev-setup.sh", "rust-toolchain.toml", "Cargo.toml", "dist-workspace.toml",
                 ".github/workflows/site.yml", ".github/workflows/ci.yml",
                 ".github/scripts/install-packaging-tools.sh"]
        for f in files:
            os.makedirs(os.path.dirname(os.path.join(repo, f)), exist_ok=True)
            shutil.copy(os.path.join(ROOT, f), os.path.join(repo, f))
        ci = os.path.join(repo, ".github", "workflows", "ci.yml")
        with open(ci) as f:
            text = f.read()
        with open(ci, "w") as f:
            f.write(re.sub(r"cargo-nextest@[0-9.]+", f"cargo-nextest@{nextest}", text))
        return self.setup(script=os.path.join(repo, "tools", "dev-setup.sh"), **tools)

    def pins(self):
        """Crate name to the version the script's hashes are for."""
        with open(SCRIPT) as f:
            body = re.search(r"^tool_hashes_for\(\) \{\n(.*?)^\}", f.read(), re.M | re.S)
        self.assertIsNotNone(body, "tool_hashes_for not found")
        return dict(re.findall(r"^ +(\S+)\) echo (\S+) ;;$", body.group(1), re.M))

    def good_downloads(self):
        """A curl that "downloads" each tool as a script printing the pinned
        version, packed the way the real archive is, and a sha256sum that
        prints the hash the script expects for it."""
        with open(SCRIPT) as f:
            script = f.read()
        arms = []
        for keys, sha in re.findall(r"^ +(\S+:\S+(?: \| \S+:\S+)*)\) echo ([0-9a-f]{64}) ;;$", script, re.M):
            for key in keys.split(" | "):
                crate, target = key.split(":")
                if crate == "cargo-nextest" and target.endswith("apple-darwin"):
                    target = "universal-apple-darwin"
                arms.append(f'*{URL_KEY[crate]}*{target}*) name={crate}; pin={self.pins()[crate]}; sha={sha} ;;')
        for plat, sha in re.findall(r"^ +(\w+_\w+)\) echo ([0-9a-f]{64}) ;;$", script, re.M):
            arms.append(f'*actionlint*{plat}*) name=actionlint; pin={self.actionlint_pin()}; sha={sha} ;;')
        curl = """out=; url=
while [ $# -gt 0 ]; do
  case $1 in -o) out=$2; shift ;; https://*) url=$1 ;; esac
  shift
done
case $url in
%s
*) exit 22 ;;
esac
file=${url##*/}
case $name in
  cargo-deny) member=${file%%.tar.gz}/cargo-deny ;;
  cargo-dist) member=${file%%.tar.xz}/dist ;;
  *) member=$name ;;
esac
d=$(mktemp -d)
mkdir -p "$(dirname "$d/$member")"
printf '#!/bin/sh\\necho "%%s %%s"\\n' "$name" "$pin" > "$d/$member"
chmod +x "$d/$member"
echo other > "$d/README.md"
case $url in
  *.tar.xz) tar -cJf "$out" -C "$d" "$member" README.md ;;
  *) tar -czf "$out" -C "$d" "$member" README.md ;;
esac
rm -rf "$d"
echo "$sha" > "$HOME/expected-hash"
""" % "\n".join(arms)
        return {"curl": curl, "sha256sum": 'echo "$(cat "$HOME/expected-hash")  $1"'}

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

    def test_setup_does_not_install_a_real_archive_with_another_hash(self):
        # A tampered release is a working archive; only the hash tells.
        out, calls = self.setup(uname=LINUX, curl=self.good_downloads()["curl"])
        for tool in [*self.pins(), "actionlint"]:
            self.assertRegex(out, rf"refused +{tool} download: SHA-256 is [0-9a-f]{{64}}, expected")
        cargo_bin = os.path.join(self.tmp, ".cargo", "bin")
        self.assertEqual(os.listdir(cargo_bin) if os.path.exists(cargo_bin) else [], [])
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
        for tool in [*self.pins(), "actionlint"]:
            self.assertRegex(out, rf"missing +{tool} .*it is in \S+/\.cargo/bin.*\n +add \S+/\.cargo/bin to PATH\n")
        self.assertNotIn("install", calls)

    def test_setup_does_not_download_again_what_path_hides(self):
        # Another mdbook comes first on PATH, and ~/.cargo/bin is not on it.
        hidden = dict(uname=LINUX, path_has_cargo_bin=False, mdbook="echo mdbook v0.0.1")
        self.setup(**hidden, **self.good_downloads())
        out, calls = self.setup(**hidden, curl='echo "curl called" >&2; exit 99')
        self.assertNotIn("curl called", out)
        self.assertNotIn("download", out)
        self.assertRegex(out, r"missing +mdbook \S+, have 0\.0\.1, .*\n +put \S+/\.cargo/bin first in PATH\n")
        self.assertRegex(out, r"missing +actionlint .*\n +add \S+/\.cargo/bin to PATH\n")
        self.assertNotIn("install", calls)

    def test_setup_says_why_a_download_failed(self):
        out, calls = self.setup(uname=LINUX, curl="exit 6")
        self.assertRegex(out, r"failed +mdbook download: could not fetch https://github\.com/rust-lang/mdBook/\S+\n")
        self.assertRegex(out, r"failed +actionlint download: could not fetch https://")

    def test_setup_says_when_it_cannot_unpack(self):
        # The right hash, but no archive behind it.
        g = self.good_downloads()
        out, calls = self.setup(uname=LINUX, sha256sum=g["sha256sum"],
                                curl=g["curl"] + '\necho "not an archive" > "$out"')
        for tool in [*self.pins(), "actionlint"]:
            self.assertRegex(out, rf"failed +{tool} download: could not unpack")
        self.assertNotIn("install", calls)

    def test_setup_says_when_nothing_can_check_the_hash(self):
        # setup() links the real hash programs in; keep them out.
        real_which = shutil.which
        self.addCleanup(setattr, shutil, "which", real_which)
        shutil.which = lambda u, *a, **k: None if u in ("sha256sum", "shasum", "sha256") else real_which(u, *a, **k)
        out, calls = self.setup(uname=LINUX, curl=self.good_downloads()["curl"])
        self.assertRegex(out, r"failed +mdbook download: there is no sha256sum, shasum or sha256")
        self.assertNotIn("refused", out)
        self.assertNotIn("install", calls)

    def test_setup_leaves_no_temporary_directory_when_stopped(self):
        out, calls = self.setup(uname=LINUX, curl="kill -TERM $PPID; exit 1")
        self.assertNotIn("download cargo-deny", out)  # it stopped at the first download
        self.assertEqual(os.listdir(self.scratch), [])

    def test_setup_goes_on_when_cargo_cannot_build_a_tool(self):
        out, calls = self.setup(uname=RISCV, curl="exit 99", cargo='[ "$1" = install ] && exit 1')
        for crate in self.pins():
            self.assertRegex(out, rf"missing +{crate} \S+, .*\n +cargo install --locked {crate} ")
        self.assertIn("This checkout", out)

    def test_setup_does_not_say_compiling_when_rust_is_not_ready(self):
        old_rust = """case "$1 $2" in
  "run "*) echo "rustc 1.80.0 (abc 2024-07-21)" ;;
  "component list") printf 'rustfmt-x\\nclippy-x\\n' ;;
  *) echo "rustup 1.29.3" ;;
esac"""
        out, calls = self.setup_in_copy(nextest="9.9.9", uname=LINUX, rustup=old_rust, **self.good_downloads())
        self.assertRegex(out, r"note +cargo-nextest: the hashes are for \S+, CI uses 9\.9\.9; "
                              r"cargo has to build it, and Rust is not ready\n")
        self.assertNotIn("; compiling", out)
        self.assertNotIn("install", calls)

    def test_setup_says_when_another_copy_comes_first_on_path(self):
        out, calls = self.setup(uname=LINUX, path_has_cargo_bin=False,
                                mdbook="echo mdbook v0.0.1", **self.good_downloads())
        self.assertRegex(out, r"missing +mdbook \S+, have 0\.0\.1, .*\n +put \S+/\.cargo/bin first in PATH\n")

    def test_site_workflow_checks_mdbook_against_the_same_hash(self):
        with open(SCRIPT) as f:
            want = re.search(r"mdbook:x86_64-unknown-linux-musl\) echo ([0-9a-f]{64}) ;;", f.read()).group(1)
        with open(os.path.join(ROOT, ".github", "workflows", "site.yml")) as f:
            text = f.read()
        self.assertIn(f"MDBOOK_SHA256: {want}\n", text)
        self.assertIn("x86_64-unknown-linux-musl.tar.gz", text)
        self.assertIn("sha256sum --check", text)

    def test_setup_compiles_when_the_hashes_are_for_another_version(self):
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

    def refuses_a_tampered_actionlint(self, uname, plat):
        out, calls = self.setup(
            uname=uname,
            curl='while [ "$1" != -o ]; do shift; done; echo tampered > "$2"')
        self.assertRegex(out, rf"running +download actionlint \S+ \({plat}\)")
        self.assertRegex(out, r"refused +actionlint download: SHA-256 is")
        self.assertRegex(out, r"missing +actionlint")
        self.assertFalse(os.path.exists(os.path.join(self.tmp, ".cargo", "bin", "actionlint")))

    def test_setup_refuses_an_actionlint_download_with_the_wrong_hash(self):
        self.refuses_a_tampered_actionlint(LINUX, "linux_amd64")

    def test_setup_refuses_a_tampered_actionlint_on_freebsd(self):
        self.refuses_a_tampered_actionlint(FREEBSD, "freebsd_amd64")

    def test_freebsd_names_pkg_packages(self):
        r, out = self.check(
            uname=FREEBSD, rsync=None, cc=None,
            python3='case "$1" in -c) exit 1 ;; *) echo "Python 3.12.3" ;; esac')
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("sudo pkg install rsync", out)
        self.assertIn("sudo pkg install llvm", out)
        self.assertIn("sudo pkg install node npm", out)
        self.assertIn("sudo pkg install py312-pyyaml", out)
        self.assertNotIn("apt install", out)
        self.assertNotIn("brew install", out)
        # Linux-only tools are not asked for.
        self.assertNotIn("musl", out)
        self.assertNotIn("nfpm", out)

    def test_root_only_hints_use_sudo(self):
        # apt and pkg need root; brew refuses it, and cargo, go and pip
        # install for the user.
        missing = dict(rsync=None, cc=None, pandoc=None,
                       python3='case "$1" in -c) exit 1 ;; *) echo "Python 3.12.3" ;; esac')
        for uname, manager in [(LINUX, "apt"), (FREEBSD, "pkg")]:
            r, out = self.check(uname=uname, **missing)
            hints = [l.strip() for l in out.splitlines() if f"{manager} install" in l]
            self.assertTrue(hints, out)
            for h in hints:
                self.assertTrue(h.startswith(f"sudo {manager} install "), h)
            self.assertNotRegex(out, r"sudo (cargo|go|brew|rustup|python3)")
        r, out = self.check(uname=MAC, **missing)
        self.assertIn("brew install rsync", out)
        self.assertNotIn("sudo", out)

    def test_install_commands_get_their_own_line(self):
        r, out = self.check(cc=None)
        lines = out.splitlines()
        i = next(n for n, l in enumerate(lines) if "C compiler" in l)
        self.assertRegex(lines[i], r"^  missing  C compiler \(cc\) \(required\)$")
        self.assertRegex(lines[i + 1], r"^ {15}(sudo apt install build-essential|xcode-select --install|sudo pkg install llvm)$")
        # Where a tool has a release binary, the script is what installs it.
        r, out = self.check(uname=LINUX)
        self.assertRegex(out, r"missing  cargo-deny \S+, for tools/ci\.sh deny \(optional\)\n"
                              r" {15}tools/dev-setup\.sh\n")
        self.assertRegex(out, r"missing  mdbook \S+, .*\n {15}tools/dev-setup\.sh\n")
        # Where it has none, cargo builds it.
        r, out = self.check(uname=RISCV)
        self.assertRegex(out, r"missing  cargo-deny \S+, for tools/ci\.sh deny \(optional\)\n"
                              r" {15}cargo install --locked cargo-deny --version \S+ --force\n")
        self.assertRegex(out, r"missing  mdbook \S+, .*\n {15}cargo install --locked mdbook --version \S+ --force\n")

    @unittest.skipUnless(os.uname().sysname == "Linux", "the packaging tools are checked on Linux only")
    def test_nfpm_is_held_to_the_ci_version(self):
        with open(os.path.join(ROOT, ".github", "scripts", "install-packaging-tools.sh")) as f:
            pin = re.search(r"^NFPM_VERSION=(\S+)$", f.read(), re.M).group(1)
        install = rf" {{15}}go install github\.com/goreleaser/nfpm/v2/cmd/nfpm@v{re.escape(pin)}\n"
        r, out = self.check()
        self.assertRegex(out, rf"missing  nfpm {re.escape(pin)}, for the packaging tests \(optional\)\n" + install)
        r, out = self.check(nfpm="echo 0.0.1")
        self.assertRegex(out, rf"missing  nfpm {re.escape(pin)}, have 0\.0\.1, .*\n" + install)
        r, out = self.check(nfpm=f"echo {pin}")
        self.assertRegex(out, rf"ok +nfpm {re.escape(pin)}\n")

    def test_old_rust_is_refused(self):
        r, out = self.check(rustup="""case "$1 $2" in
  "run "*) echo "rustc 1.80.0 (abc 2024-07-21)" ;;
  "component list") printf 'rustfmt-x\\nclippy-x\\n' ;;
  *) echo "rustup 1.29.3" ;;
esac""")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("have 1.80.0", out)


if __name__ == "__main__":
    unittest.main()
