"""Tests for tools/ci.sh: the freebsd-build job."""
import os
import platform
import re
import subprocess
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CI = os.path.join(ROOT, "tools", "ci.sh")


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
