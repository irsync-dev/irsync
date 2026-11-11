import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / "install.sh"
BASE = "https://github.com/irsync-dev/irsync/releases/download"


class InstallTest(unittest.TestCase):
    def run_install(self, tags, *args, releases_file=None):
        with tempfile.TemporaryDirectory() as tmp:
            path = releases_file or str(Path(tmp) / "releases.json")
            if releases_file is None:
                Path(path).write_text(json.dumps([{"tag_name": t, "draft": False} for t in tags]))
            env = {
                "PATH": "/usr/bin:/bin",
                "IRSYNC_INSTALL_RELEASES_FILE": path,
                "IRSYNC_INSTALL_DRY_RUN": "1",
            }
            return subprocess.run(
                ["sh", str(SCRIPT), *args], env=env, capture_output=True, text=True
            )

    def picked(self, tags, *args):
        r = self.run_install(tags, *args)
        self.assertEqual(r.returncode, 0, r.stderr)
        words = r.stdout.split()
        self.assertEqual(words[:2], ["would", "install"], r.stdout)
        return words[2]

    def test_default_picks_latest_stable(self):
        self.assertEqual(self.picked(["v0.2.1", "v0.3.0-beta.2", "v0.2.0"]), "v0.2.1")

    def test_beta_newer_than_stable(self):
        tags = ["v0.2.1", "v0.3.0-beta.2", "v0.3.0-alpha.5"]
        self.assertEqual(self.picked(tags, "--beta"), "v0.3.0-beta.2")

    def test_beta_falls_back_when_stable_is_newer(self):
        self.assertEqual(self.picked(["v0.3.0", "v0.3.0-beta.2"], "--beta"), "v0.3.0")

    def test_alpha_takes_newest_of_all(self):
        tags = ["v0.2.1", "v0.3.0-beta.1", "v0.3.0-alpha.7"]
        self.assertEqual(self.picked(tags, "--alpha"), "v0.3.0-beta.1")

    def test_alpha_newer_than_beta(self):
        tags = ["v0.2.1", "v0.3.0-beta.1", "v0.4.0-alpha.1"]
        self.assertEqual(self.picked(tags, "--alpha"), "v0.4.0-alpha.1")

    def test_numeric_ordering(self):
        tags = ["v0.9.0", "v0.10.0", "v0.10.0-beta.9", "v0.10.0-beta.10"]
        self.assertEqual(self.picked(tags), "v0.10.0")
        self.assertEqual(self.picked(tags, "--beta"), "v0.10.0")

    def test_beta_number_ordering(self):
        tags = ["v0.1.0-beta.9", "v0.1.0-beta.10"]
        self.assertEqual(self.picked(tags, "--beta"), "v0.1.0-beta.10")

    def test_no_stable_release_yet(self):
        r = self.run_install(["v0.1.0-alpha.1"])
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("--alpha", r.stderr)

    def test_args_pass_through(self):
        r = self.run_install(["v0.2.1"], "--no-modify-path")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(r.stdout.strip().endswith("--no-modify-path"), r.stdout)

    def test_args_pass_through_after_channel(self):
        r = self.run_install(["v0.2.1", "v0.3.0-beta.1"], "--beta", "--no-modify-path")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("v0.3.0-beta.1", r.stdout)
        self.assertNotIn("--beta", r.stdout)
        self.assertTrue(r.stdout.strip().endswith("--no-modify-path"), r.stdout)

    def test_url(self):
        r = self.run_install(["v0.2.1"])
        self.assertIn(f"{BASE}/v0.2.1/irsync-installer.sh", r.stdout.split())

    def test_github_unreachable(self):
        r = self.run_install([], releases_file="/nonexistent/releases.json")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("could not reach GitHub", r.stderr)
        self.assertIn("rate limit", r.stderr)

    def test_ignores_unknown_tag_shapes(self):
        self.assertEqual(self.picked(["v0.2.1", "nightly", "v0.3.0-rc.1"], "--alpha"), "v0.2.1")


class ShellTest(unittest.TestCase):
    def test_posix_sh(self):
        subprocess.run(["sh", "-n", str(SCRIPT)], check=True)
        dash = shutil.which("dash")
        if dash:
            subprocess.run([dash, "-n", str(SCRIPT)], check=True)


if __name__ == "__main__":
    unittest.main()
