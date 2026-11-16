"""Tests for packaging/tarball.sh, which packs a release-style tarball."""
import hashlib
import os
import shutil
import subprocess
import tarfile
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SCRIPT = os.path.join(ROOT, "packaging", "tarball.sh")

T = "x86_64-unknown-freebsd"


class TarballTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="tarball-sh-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.bin = os.path.join(self.tmp, "bin")
        os.mkdir(self.bin)
        for name in ["irsync", "fileset"]:
            path = os.path.join(self.bin, name)
            with open(path, "w") as f:
                f.write(f"#!/bin/sh\necho {name}\n")
            os.chmod(path, 0o755)
        # Not created here: the script makes it.
        self.out = os.path.join(self.tmp, "out")

    def run_script(self, *args):
        return subprocess.run([SCRIPT, *args], capture_output=True, text=True)

    def run_ok(self, *args):
        r = self.run_script(*args)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        return r

    def test_layout_matches_the_release_tarballs(self):
        self.run_ok(T, self.bin, self.out)
        with tarfile.open(os.path.join(self.out, f"irsync-{T}.tar.xz")) as tar:
            files = sorted(m.name for m in tar.getmembers() if m.isfile())
            self.assertEqual(files, [f"irsync-{T}/{n}" for n in
                                     ["LICENSE", "README.md", "fileset", "irsync"]])
            for n in ["irsync", "fileset"]:
                self.assertTrue(tar.getmember(f"irsync-{T}/{n}").mode & 0o111)

    def test_sha256_file_names_the_tarball(self):
        self.run_ok(T, self.bin, self.out)
        name = f"irsync-{T}.tar.xz"
        with open(os.path.join(self.out, name), "rb") as f:
            digest = hashlib.sha256(f.read()).hexdigest()
        with open(os.path.join(self.out, name + ".sha256")) as f:
            self.assertEqual(f.read(), f"{digest}  {name}\n")

    def test_missing_binary_fails_and_leaves_nothing(self):
        os.remove(os.path.join(self.bin, "fileset"))
        r = self.run_script(T, self.bin, self.out)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("tarball.sh:", r.stderr)
        self.assertFalse(os.path.exists(os.path.join(self.out, f"irsync-{T}.tar.xz")))

    def test_wrong_argument_count_fails(self):
        r = self.run_script(T)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("tarball.sh:", r.stderr)


if __name__ == "__main__":
    unittest.main()
