import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
SITE_YML = ROOT / ".github" / "workflows" / "site.yml"


class BuildMdbookVersionTests(unittest.TestCase):
    """site/build.sh holds mdbook against the version CI builds the site with.

    The script runs in a copy of the tree that has only what it reads before
    the build starts. A fake python3 stops it there with status 7.
    """

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="site-build-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        for f in ["site/build.sh", ".github/workflows/site.yml"]:
            os.makedirs(os.path.dirname(os.path.join(self.tmp, f)), exist_ok=True)
            shutil.copy(ROOT / f, os.path.join(self.tmp, f))
        self.bin = os.path.join(self.tmp, "bin")
        os.mkdir(self.bin)
        for u in ["sh", "sed", "dirname", "basename", "mkdir"]:
            os.symlink(shutil.which(u), os.path.join(self.bin, u))
        self.fake("python3", "exit 7")

    def fake(self, name, body):
        path = os.path.join(self.bin, name)
        with open(path, "w") as f:
            f.write(f"#!/bin/sh\n{body}\n")
        os.chmod(path, 0o755)

    def build(self):
        return subprocess.run([os.path.join(self.tmp, "site", "build.sh")],
                              env={"PATH": self.bin}, capture_output=True, text=True)

    def pin(self):
        return re.search(r"^ *MDBOOK_VERSION: *([0-9.]+)$", SITE_YML.read_text(), re.M).group(1)

    def test_ci_version_builds_without_a_warning(self):
        self.fake("mdbook", f"echo mdbook v{self.pin()}")
        r = self.build()
        self.assertEqual(r.returncode, 7, r.stderr)
        self.assertEqual(r.stderr, "")

    def test_another_version_warns_and_builds(self):
        self.fake("mdbook", "echo mdbook v0.0.1")
        r = self.build()
        self.assertEqual(r.returncode, 7, r.stderr)
        self.assertIn(f"warning: CI builds the site with mdbook {self.pin()}, found 0.0.1", r.stderr)
        self.assertIn("tools/dev-setup.sh", r.stderr)

    def test_missing_mdbook_stops_the_build(self):
        r = self.build()
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("mdbook not found; tools/dev-setup.sh installs it", r.stderr)


if __name__ == "__main__":
    unittest.main()
