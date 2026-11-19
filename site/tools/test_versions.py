import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import versions
from versions import Shown

ROOT = Path(__file__).resolve().parent.parent.parent
TOOL = ROOT / "site" / "tools" / "versions.py"
D = "2026-11-19"


class RepoCase(unittest.TestCase):
    """A throwaway git repository, kept clear of the developer's git settings."""

    def setUp(self):
        self.repo = Path(tempfile.mkdtemp(prefix="site-versions-"))
        self.addCleanup(shutil.rmtree, self.repo, ignore_errors=True)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "Test")
        self.git("config", "user.email", "test@example.invalid")

    def git(self, *args, env=None):
        full = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null")
        full.update(env or {})
        r = subprocess.run(["git", "-C", str(self.repo), *args], env=full,
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        return r.stdout.strip()

    def commit(self, cargo_version, when):
        """Commit a Cargo.toml with this workspace version, dated `when`."""
        (self.repo / "Cargo.toml").write_text(
            f'[workspace]\n\n[workspace.package]\nversion = "{cargo_version}"\nedition = "2024"\n')
        self.git("add", "Cargo.toml")
        self.git("commit", "-q", "--allow-empty", "-m", f"version {cargo_version}",
                 env={"GIT_AUTHOR_DATE": when, "GIT_COMMITTER_DATE": when})


class ParseTests(unittest.TestCase):
    def test_shapes(self):
        self.assertEqual(versions.parse("1.2.3"), (1, 2, 3, 2, 0))
        self.assertEqual(versions.parse("0.1.0-beta.5"), (0, 1, 0, 1, 5))
        self.assertEqual(versions.parse("0.1.0-alpha.4"), (0, 1, 0, 0, 4))

    def test_not_a_release(self):
        for t in ["1.2", "v1.2.3", "0.1.0-rc.1", "0.1.0-beta", "0.1.0-beta.x",
                  "next", "", "1.2.3 ", "1.2.3-beta.1.2"]:
            self.assertIsNone(versions.parse(t), t)

    def test_numbers_compare_as_numbers(self):
        self.assertGreater(versions.parse("0.1.0-beta.10"), versions.parse("0.1.0-beta.9"))
        self.assertGreater(versions.parse("0.10.0"), versions.parse("0.9.0"))
        self.assertGreater(versions.parse("0.1.0"), versions.parse("0.1.0-beta.99"))
        self.assertGreater(versions.parse("0.1.0-beta.1"), versions.parse("0.1.0-alpha.99"))

    def test_channel(self):
        self.assertEqual(versions.channel("1.2.3"), "stable")
        self.assertEqual(versions.channel("0.1.0-beta.5"), "beta")
        self.assertEqual(versions.channel("0.1.0-alpha.4"), "alpha")


class PickTests(unittest.TestCase):
    def shown(self, *vs):
        return [(s.channel, s.version) for s in versions.pick({v: D for v in vs})]

    def test_spec_table(self):
        self.assertEqual(self.shown("0.1.0-beta.5", "0.1.0-alpha.4"), [("beta", "0.1.0-beta.5")])
        self.assertEqual(self.shown("0.1.0", "0.1.0-beta.7", "0.1.0-alpha.4"), [("stable", "0.1.0")])
        self.assertEqual(self.shown("0.1.0", "0.2.0-beta.1", "0.2.0-alpha.3"),
                         [("stable", "0.1.0"), ("beta", "0.2.0-beta.1")])
        self.assertEqual(self.shown("0.1.2", "0.2.0-beta.1", "0.3.0-alpha.1"),
                         [("stable", "0.1.2"), ("beta", "0.2.0-beta.1"), ("alpha", "0.3.0-alpha.1")])
        self.assertEqual(self.shown("0.1.0", "0.1.0-beta.7", "0.2.0-alpha.1"),
                         [("stable", "0.1.0"), ("alpha", "0.2.0-alpha.1")])
        self.assertEqual(self.shown("0.1.0-alpha.4"), [("alpha", "0.1.0-alpha.4")])
        self.assertEqual(self.shown(), [])

    def test_alpha_is_held_against_a_hidden_beta(self):
        # The beta is hidden by stable 0.2.0, and still hides the alpha.
        self.assertEqual(self.shown("0.2.0", "0.2.0-beta.1", "0.2.0-alpha.9"), [("stable", "0.2.0")])
        # An alpha between a hidden beta and the stable: newer than the stable
        # is not enough, it must beat the beta too.
        self.assertEqual(self.shown("0.1.0", "0.3.0-beta.1", "0.2.0-alpha.1"),
                         [("stable", "0.1.0"), ("beta", "0.3.0-beta.1")])

    def test_newest_of_each_channel_and_junk_ignored(self):
        self.assertEqual(self.shown("0.1.0-beta.9", "0.1.0-beta.10", "0.1.0-rc.1", "next"),
                         [("beta", "0.1.0-beta.10")])

    def test_keeps_the_date(self):
        self.assertEqual(versions.pick({"1.0.0": "2027-03-03"}), [Shown("stable", "1.0.0", "2027-03-03")])


class FromGitTests(RepoCase):
    def test_tags_and_dates(self):
        self.commit("0.1.0-alpha.4", "2026-11-13T12:00:00+00:00")
        self.git("tag", "v0.1.0-alpha.4")
        # Just after midnight in +01:00: the commit's own day, not the UTC day before.
        self.commit("0.1.0-beta.5", "2026-11-19T00:30:00+01:00")
        self.git("tag", "v0.1.0-beta.5")
        self.assertEqual(versions.from_git(self.repo),
                         {"0.1.0-alpha.4": "2026-11-13", "0.1.0-beta.5": "2026-11-19"})

    def test_annotated_tag_uses_the_commit_date(self):
        self.commit("0.1.0-beta.1", "2026-11-14T12:00:00+00:00")
        self.git("tag", "-a", "-m", "irsync 0.1.0-beta.1", "v0.1.0-beta.1",
                 env={"GIT_COMMITTER_DATE": "2026-12-01T12:00:00+00:00"})
        self.assertEqual(versions.from_git(self.repo), {"0.1.0-beta.1": "2026-11-14"})

    def test_junk_tags_are_ignored(self):
        self.commit("0.1.0-beta.1", "2026-11-14T12:00:00+00:00")
        for t in ["v0.1.0-beta.1", "v1.2", "v0.1.0-rc.1", "vnext", "other"]:
            self.git("tag", t)
        self.assertEqual(list(versions.from_git(self.repo)), ["0.1.0-beta.1"])

    def test_no_tags_fails_and_says_what_to_do(self):
        self.commit("0.1.0-beta.1", "2026-11-14T12:00:00+00:00")
        with self.assertRaisesRegex(versions.VersionsError, "git fetch --tags"):
            versions.from_git(self.repo)

    def test_not_a_repository_fails(self):
        with tempfile.TemporaryDirectory() as d:
            with self.assertRaises(versions.VersionsError):
                versions.from_git(Path(d))

    def test_release_being_built_on_head(self):
        self.commit("0.1.0-beta.5", "2026-11-19T12:00:00+00:00")
        self.git("tag", "v0.1.0-beta.5")
        self.commit("0.1.0-beta.6", "2026-11-25T12:00:00+00:00")   # the bump
        self.commit("0.1.0-beta.6", "2026-11-27T12:00:00+00:00")   # a later commit, same version
        self.assertEqual(versions.from_git(self.repo)["0.1.0-beta.6"], "2026-11-25")

    def test_bump_merged_through_a_pull_request(self):
        # The date is the merge commit's, as the tag will sit on it.
        self.commit("0.1.0-beta.5", "2026-11-19T12:00:00+00:00")
        self.git("tag", "v0.1.0-beta.5")
        self.git("switch", "-q", "-c", "bump")
        self.commit("0.1.0-beta.6", "2026-11-24T12:00:00+00:00")
        self.git("switch", "-q", "main")
        self.git("merge", "-q", "--no-ff", "-m", "Merge bump", "bump",
                 env={"GIT_COMMITTER_DATE": "2026-11-25T12:00:00+00:00",
                      "GIT_AUTHOR_DATE": "2026-11-25T12:00:00+00:00"})
        self.assertEqual(versions.from_git(self.repo)["0.1.0-beta.6"], "2026-11-25")

    def test_head_version_that_is_not_a_release_is_skipped(self):
        self.commit("0.1.0-beta.5", "2026-11-19T12:00:00+00:00")
        self.git("tag", "v0.1.0-beta.5")
        self.commit("0.2.0-dev", "2026-11-25T12:00:00+00:00")
        self.assertEqual(versions.from_git(self.repo), {"0.1.0-beta.5": "2026-11-19"})

    def test_release_branches(self):
        self.commit("1.2.0", "2027-03-03T12:00:00+00:00")
        self.git("tag", "v1.2.0")
        base = self.git("rev-parse", "HEAD")
        self.commit("1.2.1", "2027-03-10T12:00:00+00:00")
        self.git("update-ref", "refs/remotes/origin/release-1.2", "HEAD")
        # Not a release branch: its name is not release-X.Y.
        self.commit("9.9.9", "2027-03-11T12:00:00+00:00")
        self.git("update-ref", "refs/remotes/origin/release-v0.1.0-beta.3", "HEAD")
        self.git("reset", "-q", "--hard", base)
        # A release branch with nothing to read.
        self.git("rm", "-q", "Cargo.toml")
        self.git("commit", "-q", "-m", "no cargo")
        self.git("update-ref", "refs/remotes/origin/release-1.1", "HEAD")
        self.git("reset", "-q", "--hard", base)
        self.assertEqual(versions.from_git(self.repo), {"1.2.0": "2027-03-03", "1.2.1": "2027-03-10"})

    def test_real_checkout(self):
        r = subprocess.run(["git", "-C", str(ROOT), "tag", "-l", "v0.1.0-beta.5"],
                           capture_output=True, text=True)
        if not r.stdout.strip():
            self.skipTest("this checkout has no release tags")
        self.assertEqual(versions.from_git(ROOT)["0.1.0-beta.5"], "2026-11-19")


if __name__ == "__main__":
    unittest.main()
