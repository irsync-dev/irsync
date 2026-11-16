"""Tests for packaging/dist/build.sh, with fake cargo and rustup on PATH."""
import os
import re
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

    def test_builds_man_pages_into_package_dir(self):
        # Run a copy of build.sh in a fake checkout, with a fake cargo that
        # succeeds and a fake tools/build-man.sh that records its argument.
        repo = os.path.realpath(os.path.join(self.tmp, "repo"))
        pkg = os.path.join(repo, "packaging", "dist")
        os.makedirs(pkg)
        os.makedirs(os.path.join(repo, "tools"))
        shutil.copy(BUILD, pkg)
        shutil.copy(os.path.join(os.path.dirname(BUILD), "dist.toml"), pkg)
        # The fake irsync reports the version in dist.toml, whatever it is:
        # build.sh compares the two.
        with open(os.path.join(pkg, "dist.toml")) as f:
            version = re.search(r'^version = "(.*)"$', f.read(), re.M).group(1)
        target = "aarch64-apple-darwin"
        out = os.path.join(repo, "target", target, "dist")
        man_log = os.path.join(self.tmp, "man.log")
        write = lambda path, body: (open(path, "w").write(body), os.chmod(path, 0o755))
        write(os.path.join(self.tmp, "bin", "cargo"), f"""#!/bin/sh
mkdir -p "{out}"
printf '#!/bin/sh\necho "irsync {version} (fake)"\n' > "{out}/irsync"
printf '#!/bin/sh\n' > "{out}/fileset"
chmod +x "{out}/irsync" "{out}/fileset"
""")
        write(os.path.join(repo, "tools", "build-man.sh"), f"""#!/bin/sh
echo "$1" > "{man_log}"
ls "$1" > "{man_log}.before" 2>&1
mkdir -p "$1/man1"
touch "$1/man1/irsync.1"
""")
        # A leftover from an earlier build must not survive.
        os.makedirs(os.path.join(pkg, "man"))
        open(os.path.join(pkg, "man", "stale"), "w").close()
        r = subprocess.run([os.path.join(pkg, "build.sh")], capture_output=True, text=True,
                           env=dict(self.env, CARGO_DIST_TARGET=target))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        with open(man_log) as f:
            self.assertEqual(f.read().strip(), os.path.join(pkg, "man"))
        self.assertFalse(os.path.exists(os.path.join(pkg, "man", "stale")))
        self.assertTrue(os.path.exists(os.path.join(pkg, "man", "man1", "irsync.1")))


if __name__ == "__main__":
    unittest.main()
