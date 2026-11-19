"""Tests for tools/ci.sh: the freebsd-build and fuzz jobs."""
import os
import platform
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CI = os.path.join(ROOT, "tools", "ci.sh")

# Real programs tools/ci.sh runs before a job starts; cargo and rustup are fakes.
UTILS = ["bash", "env", "dirname", "uname", "awk", "sed", "mkdir"]
# The pinned nightly, whatever its date, on a Linux machine.
RUSTUP = """case "$*" in
  *-vV) printf 'rustc 1.0.0-nightly\\nhost: x86_64-unknown-linux-gnu\\n' ;;
  *) echo "rustc 1.0.0-nightly" ;;
esac"""


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

    def run_with(self, args, cargo, env=None):
        """Runs tools/ci.sh on a PATH of the real UTILS, a fake rustup and
        the given fake cargo."""
        tmp = tempfile.mkdtemp(prefix="ci-script-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        for u in UTILS:
            os.symlink(shutil.which(u), os.path.join(tmp, u))
        for name, body in (("cargo", cargo), ("rustup", RUSTUP)):
            with open(os.path.join(tmp, name), "w") as f:
                f.write(f"#!/bin/sh\n{body}\n")
            os.chmod(os.path.join(tmp, name), 0o755)
        return subprocess.run([CI, *args], capture_output=True, text=True,
                              env={"PATH": tmp, "HOME": tmp, **(env or {})})

    def test_help_lists_fuzz_jobs(self):
        r = subprocess.run([CI, "--help"], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0)
        self.assertRegex(r.stdout, r"(?m)^  fuzz-build ")
        self.assertRegex(r.stdout, r"(?m)^  fuzz ")

    def test_fuzz_build_needs_cargo_fuzz(self):
        # A cargo without the fuzz subcommand, as cargo says it.
        r = self.run_with(["fuzz-build"], 'echo "error: no such command: \\`$1\\`" >&2; exit 101')
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("cargo-fuzz not found; tools/dev-setup.sh", r.stderr)

    def test_fuzz_build_refuses_a_stale_lock_first(self):
        # cargo fuzz has no --locked; cargo metadata --locked fails on a
        # fuzz/Cargo.lock that is out of date, before anything is built.
        r = self.run_with(["fuzz-build"], "exit 0")
        self.assertEqual(r.returncode, 0, r.stderr)
        lines = [l for l in r.stdout.splitlines() if l.startswith("== cargo ")]
        self.assertRegex(lines[0], r"^== cargo \+\S+ metadata --locked --format-version 1 "
                                   r"--manifest-path fuzz/Cargo\.toml$")
        self.assertIn(" fuzz build ", lines[1])

    def test_fuzz_seconds_reaches_libfuzzer(self):
        # A cargo-fuzz that passes and runs nothing.
        r = self.run_with(["fuzz"], "exit 0", env={"FUZZ_SECONDS": "7"})
        self.assertEqual(r.returncode, 0, r.stderr)
        runs = [l for l in r.stdout.splitlines() if " fuzz run " in l]
        self.assertTrue(runs, r.stdout)
        for line in runs:
            self.assertIn(" -max_total_time=7 ", line + " ")
        # Every target, each with its own corpus and its seeds.
        targets = sorted(f[:-3] for f in os.listdir(os.path.join(ROOT, "fuzz", "fuzz_targets")))
        for t in targets:
            self.assertTrue(any(f" {t} fuzz/corpus/{t} fuzz/seeds/{t} " in l for l in runs), t)

    def test_job_builds_then_packs(self):
        with open(CI) as f:
            body = re.search(r"^job_freebsd_build\(\) \{\n(.*?)^\}", f.read(), re.M | re.S)
        self.assertIsNotNone(body)
        lines = [l.strip() for l in body.group(1).splitlines()]
        build = lines.index("run env CARGO_DIST_TARGET=x86_64-unknown-freebsd packaging/dist/build.sh")
        pack = lines.index("run packaging/tarball.sh x86_64-unknown-freebsd packaging/dist target/distrib")
        self.assertLess(build, pack)


if __name__ == "__main__":
    unittest.main()
