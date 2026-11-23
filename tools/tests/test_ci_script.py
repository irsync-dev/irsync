"""Tests for tools/ci.sh: the freebsd-build, fuzz, workflows, miri and check jobs."""
import os
import platform
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CI = os.path.join(ROOT, "tools", "ci.sh")

# Real programs tools/ci.sh runs, before a job and in check; cargo and rustup are fakes.
UTILS = ["bash", "env", "dirname", "uname", "awk", "sed", "mkdir", "mktemp", "rm", "tr"]
# What uname prints in Git Bash, which ci.sh takes for Windows.
WINDOWS = "echo MINGW64_NT-10.0-26100"
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

    def run_with(self, args, cargo, env=None, rustup=RUSTUP, uname=None, fakes=None):
        """Runs tools/ci.sh on a PATH of the real UTILS, a fake rustup and
        the given fake cargo. uname puts a fake uname, a shell command that
        prints, in place of the real one; fakes maps the names of more fake
        programs to their shell bodies."""
        tmp = tempfile.mkdtemp(prefix="ci-script-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        for u in UTILS:
            if u == "uname" and uname is not None:
                continue
            os.symlink(shutil.which(u), os.path.join(tmp, u))
        more = [("uname", uname)] if uname is not None else []
        for name, body in (("cargo", cargo), ("rustup", rustup), *more, *(fakes or {}).items()):
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
        self.assertIn(["fileset", "--lib", "wtf8::"], args)

    def test_miri_only_when_named(self):
        # Slow: never part of a run of every job.
        with open(CI) as f:
            all_jobs = re.search(r'^all_jobs="(.*)"$', f.read(), re.M).group(1)
        self.assertNotIn("miri", all_jobs.split())

    def run_check(self, args, uname, fakes=None, fail=None):
        """Runs ci.sh with a fake cargo that succeeds and logs each call as
        one line. Returns the result and the logged calls, cargo's and the
        other fakes' (`python3 ...`), in order. With fail, a
        substring of a cargo call's arguments: that call exits 1."""
        log = os.path.join(tempfile.mkdtemp(prefix="ci-check-log-"), "calls")
        self.addCleanup(shutil.rmtree, os.path.dirname(log), ignore_errors=True)
        logging = {n: f'echo "{n} $*" >> {log}; exit 0' for n in (fakes or ())}
        stop = f'case "$*" in *"{fail}"*) exit 1 ;; esac; ' if fail else ""
        r = self.run_with(args, f'echo "cargo $*" >> {log}; {stop}exit 0', uname=uname, fakes=logging)
        calls = []
        if os.path.exists(log):
            with open(log) as f:
                calls = f.read().splitlines()
        return r, calls

    def test_windows_check_runs_the_three_crates(self):
        # No python3, pandoc, mandoc or rsync on the path: check needs none.
        r, calls = self.run_check(["check"], WINDOWS)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(calls[0], "cargo nextest --version")
        pkgs = "-p filegen -p fileset -p irsync-glob"
        self.assertEqual(calls[1:6], [
            "cargo fmt --all --check",
            # tools/sync-fuzz-lock.sh --check: its one cargo call; the fake
            # exits 0 with no output, which the script takes as a good lock.
            "cargo metadata --locked --format-version 1 --manifest-path fuzz/Cargo.toml",
            f"cargo clippy --locked {pkgs} --all-targets --all-features -- -D warnings",
            f"cargo nextest run --locked {pkgs} --all-features --profile ci --run-ignored all",
            f"cargo test --locked {pkgs} --all-features --doc",
        ])
        self.assertFalse([c for c in calls if "--workspace" in c], calls)
        # One clippy only: these crates have no OpenTelemetry feature.
        self.assertEqual(len([c for c in calls if c.startswith("cargo clippy")]), 1)

    def test_windows_check_ends_with_the_smoke_run(self):
        r, calls = self.run_check(["check"], WINDOWS)
        self.assertEqual(r.returncode, 0, r.stderr)
        smoke = calls[6:]
        self.assertEqual(len(smoke), 6, smoke)
        for c in smoke:
            self.assertRegex(c, r"^cargo run --locked -q -p (filegen|fileset) -- ")
        # filegen starts fresh from a seed, then resumes from the state file
        # it left in the output directory.
        self.assertRegex(smoke[0], r"^cargo run --locked -q -p filegen -- run --seed 1 --steps \d+ ")
        self.assertRegex(smoke[3], r"^cargo run --locked -q -p filegen -- run --resume \S+ --steps \d+ ")
        # fileset: init, check, then update and check again.
        verbs = [c.split(" -- ")[1].split()[0] for c in (smoke[1], smoke[2], smoke[4], smoke[5])]
        self.assertEqual(verbs, ["init", "check", "update", "check"])
        # The output directory is beside the tree, not inside it.
        first = smoke[0].split()
        out = first[first.index("--out-dir") + 1]
        tree = first[-1]
        self.assertEqual(os.path.dirname(out), os.path.dirname(tree))
        self.assertNotEqual(out, tree)
        self.assertIn(f"--resume {out}/filegen-1.state.yaml ", smoke[3])
        for c in (smoke[1], smoke[2], smoke[4], smoke[5], smoke[3]):
            self.assertTrue(c.endswith(" " + tree), c)
        # The temporary directory is gone.
        self.assertFalse(os.path.exists(os.path.dirname(tree)))

    def test_bare_run_on_windows_is_check_only(self):
        r, calls = self.run_check([], WINDOWS)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("ci.sh: on Windows only check runs, for filegen, fileset and irsync-glob",
                      r.stdout)
        self.assertIn("==== check", r.stdout)
        self.assertEqual([l for l in r.stdout.splitlines() if l.startswith("==== ")], ["==== check"])
        self.assertFalse([c for c in calls if c.startswith("cargo deny")], calls)

    def test_check_elsewhere_still_takes_the_workspace(self):
        # A Linux uname, so the run is the same on every host. The tools the
        # check needs there are fakes that succeed and log their calls. The
        # list is what tools/ci.sh on main ran for Linux, word for word.
        fakes = {n: "" for n in ("python3", "rsync", "pandoc", "mandoc", "touch", "rustfmt")}
        r, calls = self.run_check(["check"], "echo Linux", fakes)
        self.assertEqual(r.returncode, 0, r.stderr)
        targets = sorted(f for f in os.listdir(os.path.join(ROOT, "fuzz", "fuzz_targets")) if f.endswith(".rs"))
        targets = " ".join(f"fuzz/fuzz_targets/{t}" for t in targets)
        self.assertEqual(calls, [
            # the checks for tools before the job
            "cargo nextest --version",
            "rsync --version",
            "python3 --version",
            "pandoc --version",
            "mandoc -T ascii /dev/null",
            # job_check
            "touch crates/irsync/build.rs",
            "cargo fmt --all --check",
            f"rustfmt --edition 2021 --check {targets}",
            # tools/sync-fuzz-lock.sh --check
            "cargo metadata --locked --format-version 1 --manifest-path fuzz/Cargo.toml",
            "python3 -m unittest discover -s tools/tests",
            "cargo clippy --locked --workspace --all-targets --all-features -- -D warnings",
            "cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings",
            "cargo nextest run --locked --workspace --all-features --profile ci --run-ignored all",
            "cargo test --locked --workspace --all-features --doc",
        ])

    def test_a_failing_smoke_step_fails_the_check(self):
        r, calls = self.run_check(["check"], WINDOWS, fail="-p fileset -- update ")
        self.assertNotEqual(r.returncode, 0)
        # The calls stop at the failing update: no check after it.
        self.assertIn("-p fileset -- update ", calls[-1])
        self.assertEqual(len([c for c in calls if "-p fileset -- check " in c]), 1, calls)
        self.assertNotIn("passed", r.stdout)
        # The temporary directory is gone all the same.
        tree = calls[-1].split()[-1]
        self.assertTrue(tree.endswith("/tree"), tree)
        self.assertFalse(os.path.exists(os.path.dirname(tree)))

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
