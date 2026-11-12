"""Tests for packaging/dist/build.sh, with fake cargo and rustup on PATH."""
import os
import shutil
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BUILD = os.path.join(ROOT, "packaging", "dist", "build.sh")

# Records what cargo was run with, then fails so build.sh stops there.
FAKE_CARGO = """#!/bin/sh
{ echo "args=$*"; echo "CC=${CC-<unset>}"; echo "CXX=${CXX-<unset>}"; } > "$FAKE_LOG"
exit 3
"""


class BuildScriptTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="build-sh-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        bindir = os.path.join(self.tmp, "bin")
        os.mkdir(bindir)
        for name, body in [("cargo", FAKE_CARGO), ("rustup", "#!/bin/sh\nexit 0\n")]:
            path = os.path.join(bindir, name)
            with open(path, "w") as f:
                f.write(body)
            os.chmod(path, 0o755)
        self.log = os.path.join(self.tmp, "cargo.log")
        self.env = dict(os.environ, PATH=bindir + os.pathsep + os.environ["PATH"],
                        FAKE_LOG=self.log)

    def cargo_run(self, **env):
        r = subprocess.run([BUILD], env=dict(self.env, **env), capture_output=True, text=True)
        self.assertEqual(r.returncode, 3, r.stdout + r.stderr)
        with open(self.log) as f:
            return dict(line.rstrip("\n").split("=", 1) for line in f)

    def test_drops_dist_compiler_settings(self):
        # dist sets CC=gcc and CXX=g++ for a generic build. cc-rs would then
        # compile the bundled C code (SQLite) with glibc headers even for a
        # musl target, and the link fails. cargo must pick the compiler itself.
        for target in ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl",
                       "aarch64-apple-darwin"]:
            got = self.cargo_run(CARGO_DIST_TARGET=target, CC="gcc", CXX="g++")
            self.assertEqual(got["CC"], "<unset>", target)
            self.assertEqual(got["CXX"], "<unset>", target)
            self.assertIn(f"--target {target}", got["args"], target)
            self.assertIn("--locked", got["args"], target)


if __name__ == "__main__":
    unittest.main()
