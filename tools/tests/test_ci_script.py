"""Tests for tools/ci.sh: the freebsd-build, fuzz, workflows and miri jobs."""
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

    def run_with(self, args, cargo, env=None, rustup=RUSTUP):
        """Runs tools/ci.sh on a PATH of the real UTILS, a fake rustup and
        the given fake cargo."""
        tmp = tempfile.mkdtemp(prefix="ci-script-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        for u in UTILS:
            os.symlink(shutil.which(u), os.path.join(tmp, u))
        for name, body in (("cargo", cargo), ("rustup", rustup)):
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

    def test_workflows_needs_zizmor(self):
        r = self.run_with(["workflows"], "exit 0")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("zizmor not found; tools/dev-setup.sh", r.stderr)

    def test_help_lists_miri(self):
        r = subprocess.run([CI, "--help"], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0)
        self.assertRegex(r.stdout, r"(?m)^  miri ")

    def test_miri_needs_the_pinned_nightly(self):
        # A rustup without the nightly: the job stops before cargo runs.
        r = self.run_with(["miri"], 'echo "cargo called" >&2; exit 0',
                          rustup='echo "error: toolchain not installed" >&2; exit 1')
        self.assertNotEqual(r.returncode, 0)
        self.assertRegex(r.stderr, r"Rust nightly-\S+ \(fuzz/rust-toolchain\.toml\) not found")
        self.assertNotIn("cargo called", r.stderr)

    def test_miri_needs_the_miri_component(self):
        r = self.run_with(["miri"], 'case "$2" in miri) exit 1 ;; esac; exit 0')
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("Miri not found; tools/dev-setup.sh", r.stderr)

    def test_miri_runs_each_listed_crate_with_the_nightly(self):
        # The fake cargo logs its arguments one per line, so a word that
        # reached it whole, or two words glued together, shows.
        log = os.path.join(tempfile.mkdtemp(prefix="ci-miri-log-"), "args")
        self.addCleanup(shutil.rmtree, os.path.dirname(log), ignore_errors=True)
        r = self.run_with(["miri"], f'[ "$2" = miri ] && [ "$3" = test ] && '
                                    f'{{ printf "%s\\n" "$@"; echo END; }} >> {log}; exit 0')
        self.assertEqual(r.returncode, 0, r.stderr)
        runs = [l for l in r.stdout.splitlines() if l.startswith("== cargo ")]
        self.assertTrue(runs, r.stdout)
        for line in runs:
            self.assertRegex(line, r"^== cargo \+nightly-\S+ miri test --locked -p \S+")
        self.assertTrue(any(" -p irsync-glob" in l for l in runs), runs)
        self.assertTrue(any(" -p irsync-proto" in l for l in runs), runs)
        # job_miri splits a multi-word entry into words on purpose: each
        # word must reach cargo as an argument of its own.
        with open(log) as f:
            calls = [c.strip("\n").split("\n") for c in f.read().split("END\n") if c.strip()]
        args = [c[c.index("-p") + 1:] for c in calls]
        self.assertIn(["irsync", "--test", "sub_chunk"], args)
        self.assertIn(["fileset", "--lib", "--", "compare::", "escape::", "glob::"], args)

    def test_miri_only_when_named(self):
        # Slow: never part of a run of every job.
        with open(CI) as f:
            all_jobs = re.search(r'^all_jobs="(.*)"$', f.read(), re.M).group(1)
        self.assertNotIn("miri", all_jobs.split())

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
