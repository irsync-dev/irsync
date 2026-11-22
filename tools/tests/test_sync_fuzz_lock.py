"""Tests for tools/sync-fuzz-lock.sh, on made-up lock files with a fake cargo."""
import os
import shutil
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SCRIPT = os.path.join(ROOT, "tools", "sync-fuzz-lock.sh")

REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
GIT = "git+https://github.com/n0-computer/iroh?rev=970c#970c"

# A fake cargo: it writes its arguments to cargo.log, and `cargo metadata`
# without --locked puts a marker at the end of the lock file, as the real
# one rewrites it. CARGO_LOCKED_FAILS makes --locked fail.
CARGO = """echo "$*" >>"$LOG"
case "$*" in
  *--locked*) [ -z "${CARGO_LOCKED_FAILS:-}" ] || { echo "error: cannot update the lock file" >&2; exit 101; } ;;
  *) echo "# rewritten by cargo" >>fuzz/Cargo.lock ;;
esac"""


def lock(*packages):
    """A lock file of (name, version, source) packages; no source means a
    crate of this repository."""
    out = ["version = 4", ""]
    for name, version, source in packages:
        out += ["[[package]]", f'name = "{name}"', f'version = "{version}"']
        if source:
            out += [f'source = "{source}"', 'checksum = "00"']
        # Names another package at another version, which is not a package.
        out += ["dependencies = [", ' "url 9.9.9",', "]", ""]
    return "\n".join(out)


MAIN = lock(("irsync", "0.1.0", None), ("iroh", "1.5.0", GIT),
            ("url", "2.5.9", REGISTRY),
            ("hashbrown", "0.15.5", REGISTRY), ("hashbrown", "0.17.2", REGISTRY))
FUZZ = lock(("irsync", "0.1.0", None), ("irsync-fuzz", "0.0.0", None),
            ("iroh", "1.5.0", GIT), ("url", "2.5.9", REGISTRY),
            ("hashbrown", "0.17.2", REGISTRY),
            ("libfuzzer-sys", "0.4.13", REGISTRY))


class SyncFuzzLockTests(unittest.TestCase):
    def run_with(self, args, main=MAIN, fuzz=FUZZ, env=None):
        """Runs a copy of the script in a made-up repository."""
        self.root = tempfile.mkdtemp(prefix="sync-fuzz-lock-")
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        for d in ("tools", "fuzz", "bin"):
            os.mkdir(os.path.join(self.root, d))
        script = os.path.join(self.root, "tools", "sync-fuzz-lock.sh")
        shutil.copy(SCRIPT, script)
        for path, text in (("Cargo.lock", main), ("fuzz/Cargo.lock", fuzz)):
            with open(os.path.join(self.root, path), "w") as f:
                f.write(text)
        cargo = os.path.join(self.root, "bin", "cargo")
        with open(cargo, "w") as f:
            f.write(f"#!/bin/sh\n{CARGO}\n")
        os.chmod(cargo, 0o755)
        path = os.path.join(self.root, "bin") + os.pathsep + os.environ["PATH"]
        # Started somewhere else: the script finds the repository itself.
        return subprocess.run(
            [script, *args], capture_output=True, text=True, cwd=tempfile.gettempdir(),
            env={"PATH": path, "LOG": os.path.join(self.root, "cargo.log"), **(env or {})})

    def read(self, path):
        with open(os.path.join(self.root, path)) as f:
            return f.read()

    def test_check_passes_when_the_shared_crates_match(self):
        r = self.run_with(["--check"])
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stderr, "")
        self.assertEqual(self.read("fuzz/Cargo.lock"), FUZZ)
        self.assertEqual(
            self.read("cargo.log"),
            "metadata --locked --format-version 1 --manifest-path fuzz/Cargo.toml\n")

    def test_check_names_a_crate_at_another_version(self):
        fuzz = FUZZ.replace('version = "2.5.9"', 'version = "2.5.8"')
        r = self.run_with(["--check"], fuzz=fuzz)
        self.assertEqual(r.returncode, 1)
        self.assertIn("\n  url 2.5.8\n", r.stderr)
        self.assertIn("run tools/sync-fuzz-lock.sh and commit the result", r.stderr)
        self.assertNotIn("hashbrown", r.stderr)
        self.assertNotIn("libfuzzer-sys", r.stderr)
        self.assertEqual(self.read("fuzz/Cargo.lock"), fuzz)

    def test_check_names_a_crate_from_another_source(self):
        r = self.run_with(["--check"], fuzz=FUZZ.replace("rev=970c#970c", "rev=1234#1234"))
        self.assertEqual(r.returncode, 1)
        self.assertIn("\n  iroh 1.5.0\n", r.stderr)

    def test_check_fails_when_cargo_would_rewrite_the_file(self):
        r = self.run_with(["--check"], env={"CARGO_LOCKED_FAILS": "1"})
        self.assertEqual(r.returncode, 1)
        self.assertIn("fuzz/Cargo.lock is out of date; run tools/sync-fuzz-lock.sh", r.stderr)
        self.assertEqual(self.read("fuzz/Cargo.lock"), FUZZ)

    def test_sync_starts_from_the_main_lock_file(self):
        r = self.run_with([], fuzz=FUZZ.replace('version = "2.5.9"', 'version = "2.5.8"'))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.read("fuzz/Cargo.lock"), MAIN + "# rewritten by cargo\n")
        self.assertEqual(
            self.read("cargo.log"),
            "metadata --format-version 1 --manifest-path fuzz/Cargo.toml\n"
            "metadata --locked --format-version 1 --manifest-path fuzz/Cargo.toml\n")

    def test_an_unknown_argument_is_refused(self):
        r = self.run_with(["--chekc"])
        self.assertEqual(r.returncode, 2)
        self.assertIn("usage:", r.stderr)
        self.assertFalse(os.path.exists(os.path.join(self.root, "cargo.log")))


if __name__ == "__main__":
    unittest.main()
