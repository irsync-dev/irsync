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

    def test_man_page_tools_are_optional_and_named(self):
        r, out = self.check()
        self.assertEqual(r.returncode, 0, out)
        for tool, install in [
            ("pandoc", "brew install pandoc|apt install pandoc|pkg install hs-pandoc"),
            ("mandoc", "brew install mandoc|apt install mandoc|pkg install mandoc"),
        ]:
            self.assertRegex(out, rf"missing +{tool}, for the man pages.*\(optional\)")
            self.assertRegex(out, install)
        r, out = self.check(pandoc="echo pandoc 3.1.3", mandoc="echo mandoc")
        self.assertRegex(out, r"ok +pandoc\n")
        self.assertRegex(out, r"ok +mandoc\n")

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

    def test_optional_tools_do_not_fail_the_check(self):
        # No node, cargo tools or packaging tools here; only required ones
        # decide the exit status.
        r, out = self.check()
        self.assertEqual(r.returncode, 0, out)
        self.assertRegex(out, r"missing +node and npx.*\(optional\)")
        self.assertRegex(out, r"missing +mdbook.*\(optional\)")
        self.assertIn("Ready.", out)

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
        # Any mdbook will do.
        self.assertRegex(out, r"ok +mdbook 0\.0\.1\n")

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

    def setup(self, **tools):
        """Runs the script without --check, with a cargo that records its calls."""
        log = os.path.join(self.tmp, "cargo.log")
        for name, body in {**GOOD, **tools}.items():
            self.fake(name, body)
        self.fake("cargo", f'echo "$*" >> {log}\n' + tools.get("cargo", "") + "\nexit 0")
        for u in ["sha256sum", "shasum", "sha256", "mkdir"]:
            if shutil.which(u):
                os.symlink(shutil.which(u), os.path.join(self.bin, u))
        r = subprocess.run([SCRIPT], env={"PATH": self.bin, "HOME": self.tmp},
                           capture_output=True, text=True)
        calls = ""
        if os.path.exists(log):
            with open(log) as f:
                calls = f.read()
        return r.stdout + r.stderr, calls

    def test_setup_replaces_another_version_but_keeps_any_mdbook(self):
        out, calls = self.setup(cargo='[ "$1" = deny ] && echo "cargo-deny 0.1.0"',
                                mdbook="echo mdbook v0.0.1",
                                curl="exit 22")
        self.assertRegex(calls, r"install --locked cargo-deny --version \d+\.\d+\.\d+\n")
        self.assertRegex(calls, r"install --locked cargo-nextest --version")
        self.assertNotIn("mdbook", calls)

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
        self.assertIn("pkg install rsync", out)
        self.assertIn("pkg install llvm", out)
        self.assertIn("pkg install node npm", out)
        self.assertIn("pkg install py312-pyyaml", out)
        self.assertNotIn("apt install", out)
        self.assertNotIn("brew install", out)
        # Linux-only tools are not asked for.
        self.assertNotIn("musl", out)
        self.assertNotIn("nfpm", out)

    def test_install_commands_get_their_own_line(self):
        r, out = self.check(cc=None)
        lines = out.splitlines()
        i = next(n for n, l in enumerate(lines) if "C compiler" in l)
        self.assertRegex(lines[i], r"^  missing  C compiler \(cc\) \(required\)$")
        self.assertRegex(lines[i + 1], r"^ {15}(apt install build-essential|xcode-select --install|pkg install llvm)$")
        self.assertRegex(out, r"missing  cargo-deny \S+, for tools/ci\.sh deny \(optional\)\n"
                              r" {15}cargo install --locked cargo-deny --version \S+\n")
        # No version for mdbook: any will do.
        self.assertRegex(out, r"missing  mdbook, .*\n {15}cargo install --locked mdbook\n")

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
