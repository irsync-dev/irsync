"""Tests for tools/dev-setup.sh --check, against fake tools on a bare PATH."""
import os
import shutil
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SCRIPT = os.path.join(ROOT, "tools", "dev-setup.sh")

# Real programs the script itself runs; everything else on PATH is a fake.
UTILS = ["bash", "env", "sh", "dirname", "uname", "sed", "grep", "awk", "head",
         "tr", "cat", "cut"]

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


class DevSetupCheckTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="dev-setup-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.bin = os.path.join(self.tmp, "bin")
        os.mkdir(self.bin)
        for u in UTILS:
            os.symlink(shutil.which(u), os.path.join(self.bin, u))
        # Must never run in --check mode: it would install rustup.
        self.fake("curl", 'echo "curl called" >&2; exit 99')

    def fake(self, name, body):
        path = os.path.join(self.bin, name)
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
        for tool in ["C compiler", "git", "GNU rsync", "Python"]:
            self.assertRegex(out, rf"ok +{tool}", out)

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

    def test_old_python_is_refused(self):
        r, out = self.check(python3="echo 'Python 3.10.12'")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("Python 3.11 or newer", out)

    def test_missing_compiler_names_the_package(self):
        r, out = self.check(cc=None)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("C compiler", out)
        self.assertRegex(out, r"build-essential|xcode-select")

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
