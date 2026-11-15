"""Tests for the scripts in .github/scripts, with a fake gh (fake_gh.py)."""
import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.dirname(HERE)
ROOT = os.path.dirname(os.path.dirname(SCRIPTS))
CHECK = os.path.join(SCRIPTS, "check-release-tag.sh")
NOTES = os.path.join(SCRIPTS, "release-notes.sh")
RESUME = os.path.join(SCRIPTS, "resume-check.sh")
BUMP = os.path.join(SCRIPTS, "bump-version.sh")
STRAY = os.path.join(SCRIPTS, "stray-tags.sh")
START = os.path.join(SCRIPTS, "release-start-check.sh")
BRANCH = os.path.join(SCRIPTS, "release-branch.sh")
HEADING = "## What's changed"
REL = {"body": "", "isDraft": False, "isPrerelease": False}


class FakeGh(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="gh-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        bindir = os.path.join(self.tmp, "bin")
        os.mkdir(bindir)
        os.symlink(os.path.join(HERE, "fake_gh.py"), os.path.join(bindir, "gh"))
        self.state_file = os.path.join(self.tmp, "state.json")
        self.env = dict(os.environ, PATH=bindir + os.pathsep + os.environ["PATH"],
                        FAKE_GH_STATE=self.state_file, GH_REPO="irsync-dev/irsync")
        self.set_state(releases={}, tags=[], generated="* a change by @someone")

    def set_state(self, **kw):
        with open(self.state_file, "w") as f:
            json.dump(kw, f)

    def state(self):
        with open(self.state_file) as f:
            return json.load(f)

    def run_script(self, *args, cwd=None):
        return subprocess.run(list(args), env=self.env, cwd=cwd,
                              capture_output=True, text=True)


def version_fixture(test, cargo, dist):
    """A directory holding Cargo.toml and packaging/dist/dist.toml with these versions."""
    d = tempfile.mkdtemp(prefix="ver-")
    test.addCleanup(shutil.rmtree, d, ignore_errors=True)
    os.makedirs(os.path.join(d, "packaging", "dist"))
    with open(os.path.join(d, "Cargo.toml"), "w") as f:
        f.write(f'[workspace]\nmembers = []\n\n[workspace.package]\nversion = "{cargo}"\n'
                f'\n[package]\nversion = "9.9.9"\n')
    with open(os.path.join(d, "packaging", "dist", "dist.toml"), "w") as f:
        f.write(f'[package]\nname = "irsync"\nversion = "{dist}"\n')
    return d


class CheckTagTests(unittest.TestCase):
    def check(self, *args, cwd=None):
        return subprocess.run([CHECK, *args], cwd=cwd, capture_output=True, text=True)

    def test_release_tags(self):
        for tag, pre in [("v1.2.3", "false"), ("v0.10.0", "false"),
                         ("v1.2.3-beta.1", "true"), ("v1.2.3-alpha.12", "true")]:
            r = self.check(tag)
            self.assertEqual(r.returncode, 0, tag)
            self.assertEqual(r.stdout, f"prerelease={pre}\n", tag)

    def test_other_tags(self):
        for tag in ["v1.2", "1.2.3", "v1.2.3-rc.1", "v1.2.3-beta", "v1.2.3-beta.1.2",
                    "v1.2.3 ", "v1.2.3;id", "v1.2.3\nv1.2.4", "vx.y.z", ""]:
            r = self.check(tag)
            self.assertEqual(r.returncode, 1, repr(tag))
            self.assertIn("not a release tag", r.stderr, repr(tag))
            self.assertEqual(r.stdout, "", repr(tag))

    def test_versions_match(self):
        d = version_fixture(self, "1.2.3-beta.1", "1.2.3-beta.1")
        r = self.check("--versions", "v1.2.3-beta.1", cwd=d)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(r.stdout, "prerelease=true\n")

    def test_cargo_version_differs(self):
        d = version_fixture(self, "1.2.2", "1.2.3")
        r = self.check("--versions", "v1.2.3", cwd=d)
        self.assertEqual(r.returncode, 1)
        self.assertIn("Cargo.toml has version 1.2.2", r.stderr)
        self.assertNotIn("dist.toml", r.stderr)
        self.assertEqual(r.stdout, "")

    def test_dist_version_differs(self):
        d = version_fixture(self, "1.2.3", "1.2.3-beta.1")
        r = self.check("--versions", "v1.2.3", cwd=d)
        self.assertEqual(r.returncode, 1)
        self.assertIn("dist.toml has version 1.2.3-beta.1", r.stderr)
        self.assertEqual(r.stdout, "")

    def test_bad_tag_with_versions(self):
        d = version_fixture(self, "1.2.3", "1.2.3")
        r = self.check("--versions", "v1.2", cwd=d)
        self.assertEqual(r.returncode, 1)
        self.assertIn("not a release tag", r.stderr)

    def test_this_repo(self):
        with open(os.path.join(ROOT, "Cargo.toml")) as f:
            m = re.search(r'^\[workspace\.package\]\nversion = "([^"]+)"', f.read(), re.M)
        r = self.check("--versions", "v" + m.group(1), cwd=ROOT)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)


@unittest.skipUnless(shutil.which("cargo"), "needs cargo")
class BumpVersionTests(unittest.TestCase):
    """bump-version.sh, in a temporary git repo holding a tiny Cargo workspace."""

    def setUp(self):
        d = self.dir = tempfile.mkdtemp(prefix="bump-")
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        self.env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.com",
                        GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.com",
                        GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        self.write("Cargo.toml",
                   '[workspace]\nmembers = ["a"]\nresolver = "2"\n\n'
                   '[workspace.metadata.other]\nversion = "9.9.9"\n\n'
                   '[workspace.package]\nedition = "2021"\nversion = "1.2.3"\n')
        self.write("a/Cargo.toml",
                   '[package]\nname = "a"\nversion.workspace = true\nedition.workspace = true\n')
        self.write("a/src/lib.rs", "")
        self.write("packaging/dist/dist.toml",
                   '# Keep `version` equal to the workspace version.\n\n'
                   '[package]\nname = "irsync"\nversion = "1.2.3"\nlicense = "MIT"\n')
        self.run_ok("git", "init", "-q", "-b", "main")
        self.run_ok("cargo", "generate-lockfile", "--quiet")
        self.run_ok("git", "add", "-A")
        self.run_ok("git", "commit", "-q", "-m", "start")

    def write(self, path, text):
        path = os.path.join(self.dir, path)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(text)

    def read(self, path):
        with open(os.path.join(self.dir, path)) as f:
            return f.read()

    def run_ok(self, *args):
        r = subprocess.run(args, cwd=self.dir, env=self.env, capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        return r.stdout

    def bump(self, version):
        return subprocess.run([BUMP, version], cwd=os.path.join(self.dir, "a"), env=self.env,
                              capture_output=True, text=True)

    def test_commits_on_a_bump_branch(self):
        main = self.run_ok("git", "rev-parse", "main").strip()
        r = self.bump("1.3.0-beta.1")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.run_ok("git", "branch", "--show-current").strip(),
                         "bump-v1.3.0-beta.1")
        self.assertEqual(self.run_ok("git", "rev-parse", "main").strip(), main)

    def test_hint_goes_through_a_pull_request(self):
        r = self.bump("1.3.0-beta.1")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("git push -u origin bump-v1.3.0-beta.1", r.stdout)
        self.assertIn("gh pr create --fill --base main", r.stdout)
        self.assertIn("gh workflow run release-gate.yml --ref main -f version=1.3.0-beta.1",
                      r.stdout)
        self.assertNotIn("git tag", r.stdout)
        self.assertNotIn("git push origin main", r.stdout)

    def test_bumps_and_commits(self):
        r = self.bump("1.3.0-beta.1")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        cargo = self.read("Cargo.toml")
        self.assertIn('[workspace.package]\nedition = "2021"\nversion = "1.3.0-beta.1"\n', cargo)
        self.assertIn('[workspace.metadata.other]\nversion = "9.9.9"\n', cargo)
        dist = self.read("packaging/dist/dist.toml")
        self.assertTrue(dist.startswith("# Keep `version` equal"), dist)
        self.assertIn('version = "1.3.0-beta.1"\nlicense = "MIT"\n', dist)
        self.assertIn('name = "a"\nversion = "1.3.0-beta.1"\n', self.read("Cargo.lock"))
        self.assertEqual(self.run_ok("git", "log", "-1", "--format=%s"), "Release v1.3.0-beta.1\n")
        self.assertEqual(self.run_ok("git", "status", "--porcelain"), "")
        self.assertEqual(self.run_ok("git", "show", "--name-only", "--format=").split(),
                         ["Cargo.lock", "Cargo.toml", "packaging/dist/dist.toml"])
        self.run_ok(CHECK, "--versions", "v1.3.0-beta.1")

    def assert_refused(self, r, message):
        self.assertNotEqual(r.returncode, 0)
        self.assertIn(message, r.stderr)
        self.assertEqual(self.run_ok("git", "log", "-1", "--format=%s"), "start\n")
        self.assertIn('version = "1.2.3"', self.read("packaging/dist/dist.toml"))

    def test_bad_versions(self):
        for v in ["1.2", "v1.3.0", "1.3.0-rc.1", "1.3.0;id"]:
            self.assert_refused(self.bump(v), "not a release tag")

    def test_same_version(self):
        self.assert_refused(self.bump("1.2.3"), "already 1.2.3")

    def test_uncommitted_changes(self):
        self.write("a/src/lib.rs", "// edit\n")
        self.assert_refused(self.bump("1.3.0"), "uncommitted changes")

    def test_patch_needs_release_branch(self):
        r = self.bump("1.2.4")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("git switch release-1.2", r.stderr)

    def test_patch_on_release_branch(self):
        self.run_ok("git", "switch", "-q", "-c", "release-1.2")
        r = self.bump("1.2.4")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.run_ok("git", "branch", "--show-current").strip(), "bump-v1.2.4")
        self.assertIn("--base release-1.2", r.stdout)
        self.assertIn("--ref release-1.2", r.stdout)

    def test_prerelease_not_on_release_branch(self):
        self.run_ok("git", "switch", "-q", "-c", "release-1.2")
        r = self.bump("1.3.0-beta.1")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("git switch main", r.stderr)


class NotesTests(FakeGh):
    def notes(self, tag, tags, body="Intro."):
        self.set_state(releases={tag: {"body": body, "isDraft": False, "isPrerelease": False}},
                       tags=tags, generated="* a change by @someone")
        r = self.run_script(NOTES, tag)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        return self.state()

    def test_adds_list(self):
        s = self.notes("v0.2.0", ["v0.1.0", "v0.2.0"])
        self.assertEqual(s["releases"]["v0.2.0"]["body"],
                         f"Intro.\n\n{HEADING}\n\n* a change by @someone\n")

    def test_empty_body(self):
        s = self.notes("v0.2.0", ["v0.2.0"], body="")
        self.assertEqual(s["releases"]["v0.2.0"]["body"],
                         f"{HEADING}\n\n* a change by @someone\n")

    def test_stable_starts_at_previous_stable(self):
        tags = ["v0.1.0", "v0.2.0-alpha.1", "v0.2.0-beta.1", "v0.2.0", "v0.3.0-beta.1",
                "v0.3.0-beta.2", "v0.3.0"]
        s = self.notes("v0.3.0", tags)
        self.assertEqual(s["generate"], {"tag_name": "v0.3.0", "previous_tag_name": "v0.2.0"})

    def test_sorts_by_version(self):
        tags = ["v0.9.0", "v0.10.0", "v0.11.0", "v0.10.1-beta.1", "v0.2.0"]
        self.assertEqual(self.notes("v0.11.0", tags)["generate"]["previous_tag_name"], "v0.10.0")
        self.assertEqual(self.notes("v0.10.0", tags)["generate"]["previous_tag_name"], "v0.9.0")

    def test_ignores_later_tags(self):
        # Running the notes for an older release after a newer one exists.
        tags = ["v0.1.0", "v0.2.0", "v0.3.0", "v1.0.0"]
        self.assertEqual(self.notes("v0.2.0", tags)["generate"]["previous_tag_name"], "v0.1.0")

    def test_first_stable(self):
        tags = ["v0.1.0-alpha.1", "v0.1.0-beta.1", "v0.1.0", "v-odd", "vnext"]
        self.assertEqual(self.notes("v0.1.0", tags)["generate"], {"tag_name": "v0.1.0"})

    def test_prerelease_keeps_default(self):
        s = self.notes("v0.2.0-beta.1", ["v0.1.0", "v0.2.0-beta.1"])
        self.assertEqual(s["generate"], {"tag_name": "v0.2.0-beta.1"})
        self.assertFalse([c for c in s["calls"] if "matching-refs" in " ".join(c)])

    def test_rerun_does_nothing(self):
        body = f"Intro.\n\n{HEADING}\n\n* old list\n"
        s = self.notes("v0.2.0", ["v0.2.0"], body=body)
        self.assertEqual(s["releases"]["v0.2.0"]["body"], body)
        self.assertNotIn("generate", s)
        self.assertFalse([c for c in s["calls"] if c[:2] == ["release", "edit"]])

    def test_twice_adds_once(self):
        self.notes("v0.2.0", ["v0.1.0", "v0.2.0"])
        r = self.run_script(NOTES, "v0.2.0")
        self.assertEqual(r.returncode, 0)
        self.assertEqual(self.state()["releases"]["v0.2.0"]["body"].count(HEADING), 1)

    def test_empty_list_fails(self):
        self.set_state(releases={"v0.2.0": {"body": "x", "isDraft": False, "isPrerelease": False}},
                       tags=["v0.2.0"], generated="")
        r = self.run_script(NOTES, "v0.2.0")
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(self.state()["releases"]["v0.2.0"]["body"], "x")

    def test_no_release_fails(self):
        r = self.run_script(NOTES, "v0.2.0")
        self.assertNotEqual(r.returncode, 0)


class ResumeTests(FakeGh):
    def setUp(self):
        super().setUp()
        # On a runner GITHUB_REF is always set; here only where a test sets it.
        self.env.pop("GITHUB_REF", None)

    def resume(self, tag, dry_run, release=None, ref=None):
        releases = {tag: release} if release else {}
        self.set_state(releases=releases, tags=[tag], generated="x")
        if ref is not None:
            self.env["GITHUB_REF"] = ref
        return self.run_script(RESUME, tag, "true" if dry_run else "false")

    def out(self, r):
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        return dict(l.split("=", 1) for l in r.stdout.splitlines())

    PUBLISHED = REL
    PRE = {"body": "", "isDraft": False, "isPrerelease": True}

    def test_stable(self):
        self.assertEqual(self.out(self.resume("v1.2.3", False, self.PUBLISHED)),
                         {"packages": "true", "homebrew": "true", "notes": "true"})

    def test_prerelease_tag_gets_notes_only(self):
        self.assertEqual(self.out(self.resume("v1.2.3-beta.1", False, self.PRE)),
                         {"packages": "false", "homebrew": "false", "notes": "true"})

    def test_stable_tag_marked_prerelease_gets_no_packages(self):
        self.assertEqual(self.out(self.resume("v1.2.3", False, self.PRE)),
                         {"packages": "false", "homebrew": "false", "notes": "true"})

    def test_dry_run_on_alpha(self):
        self.assertEqual(self.out(self.resume("v0.1.0-alpha.1", True, self.PRE)),
                         {"packages": "true", "homebrew": "false", "notes": "false"})

    def test_dry_run_on_stable(self):
        self.assertEqual(self.out(self.resume("v1.2.3", True, self.PUBLISHED)),
                         {"packages": "true", "homebrew": "false", "notes": "false"})

    def test_draft_fails(self):
        r = self.resume("v1.2.3", False, {"body": "", "isDraft": True, "isPrerelease": False})
        self.assertEqual(r.returncode, 1)
        self.assertIn("draft", r.stderr)
        self.assertEqual(r.stdout, "")

    def test_missing_release_fails(self):
        for dry in (False, True):
            r = self.resume("v1.2.3", dry)
            self.assertEqual(r.returncode, 1)
            self.assertIn("no GitHub Release", r.stderr)
            self.assertEqual(r.stdout, "")

    def test_resume_refuses_branch_ref(self):
        r = self.resume("v1.2.3", False, self.PUBLISHED, ref="refs/heads/main")
        self.assertEqual(r.returncode, 1)
        self.assertIn("gh workflow run release-resume.yml --ref v1.2.3 -f tag=v1.2.3", r.stderr)
        self.assertEqual(r.stdout, "")
        self.assertEqual(self.state().get("calls", []), [])

    def test_resume_refuses_other_tag_ref(self):
        r = self.resume("v1.2.3", False, self.PUBLISHED, ref="refs/tags/v1.2.4")
        self.assertEqual(r.returncode, 1)
        self.assertIn("--ref v1.2.3", r.stderr)

    def test_resume_accepts_matching_tag_ref(self):
        self.assertEqual(self.out(self.resume("v1.2.3", False, self.PUBLISHED,
                                              ref="refs/tags/v1.2.3")),
                         {"packages": "true", "homebrew": "true", "notes": "true"})

    def test_bad_tag_fails_before_gh(self):
        r = self.resume("v1.2.3;id", True, self.PUBLISHED)
        self.assertEqual(r.returncode, 1)
        self.assertEqual(r.stdout, "")
        self.assertEqual(self.state().get("calls", []), [])


class StrayTagTests(FakeGh):
    def stray(self):
        return self.run_script(STRAY)

    def test_no_tags(self):
        r = self.stray()
        self.assertEqual((r.returncode, r.stdout), (0, ""))

    def test_every_tag_has_a_release(self):
        self.set_state(releases={"v1.0.0": REL, "v1.1.0-beta.1": REL},
                       tags=["v1.0.0", "v1.1.0-beta.1"])
        self.assertEqual(self.stray().returncode, 0)

    def test_old_alphas_are_accepted(self):
        self.set_state(releases={}, tags=["v0.1.0-alpha.1", "v0.1.0-alpha.2"])
        self.assertEqual(self.stray().returncode, 0)

    def test_hand_pushed_tag_is_stray(self):
        self.set_state(releases={"v1.0.0": REL}, tags=["v1.0.0", "v1.0.1", "v1.0.10"])
        r = self.stray()
        self.assertEqual(r.returncode, 1)
        self.assertEqual(r.stdout, "v1.0.1\nv1.0.10\n")
        self.assertIn("::error::", r.stderr)
        self.assertIn("v1.0.1", r.stderr)

    def test_draft_release_counts(self):
        self.set_state(releases={"v1.0.0": dict(REL, isDraft=True)}, tags=["v1.0.0"])
        self.assertEqual(self.stray().returncode, 0)


class ReleaseBranchTests(unittest.TestCase):
    def branch(self, version):
        return subprocess.run([BRANCH, version], capture_output=True, text=True)

    def test_branches(self):
        for version, want in [("1.3.0-beta.1", "main"), ("1.3.0-alpha.2", "main"),
                              ("1.3.0", "main"), ("1.2.1", "release-1.2"),
                              ("1.20.3", "release-1.20")]:
            r = self.branch(version)
            self.assertEqual((r.returncode, r.stdout), (0, want + "\n"), version)

    def test_bad_names(self):
        for version in ["1.2", "1.2.3-rc.1", "1.2.3;id", ""]:
            r = self.branch(version)
            self.assertNotEqual(r.returncode, 0, repr(version))
            self.assertEqual(r.stdout, "", repr(version))

    def test_leading_v(self):
        r = self.branch("v1.2.1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("without the v", r.stderr)


class StartCheckTests(FakeGh):
    def setUp(self):
        super().setUp()
        # On a runner GITHUB_REF is always set; here only where a test sets it.
        self.env.pop("GITHUB_REF", None)

    def start(self, version, ref=None, files=None):
        files = files or version
        d = version_fixture(self, files, files)
        if ref:
            self.env["GITHUB_REF"] = ref
        return self.run_script(START, version, cwd=d)

    def test_prerelease_from_main(self):
        r = self.start("1.3.0-beta.1", "refs/heads/main")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "tag=v1.3.0-beta.1\nprerelease=true\nbranch=main\nnew-branch=\n")

    def test_minor_from_main_makes_branch(self):
        r = self.start("1.3.0", "refs/heads/main")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "tag=v1.3.0\nprerelease=false\nbranch=main\nnew-branch=release-1.3\n")

    def test_patch_from_release_branch(self):
        self.set_state(releases={}, tags=[], branches=["main", "release-1.2"])
        r = self.start("1.2.1", "refs/heads/release-1.2")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "tag=v1.2.1\nprerelease=false\nbranch=release-1.2\nnew-branch=\n")

    def test_wrong_branch(self):
        self.set_state(releases={}, tags=[], branches=["main", "release-1.2"])
        for version, ref, want in [("1.2.1", "refs/heads/main", "--ref release-1.2"),
                                   ("1.3.0-beta.1", "refs/heads/release-1.2", "--ref main"),
                                   ("1.3.0", "refs/tags/v1.3.0", "--ref main")]:
            r = self.start(version, ref)
            self.assertEqual(r.returncode, 1, version)
            self.assertIn(f"gh workflow run release-gate.yml {want} -f version={version}", r.stderr)

    def test_tag_exists_points_to_resume(self):
        self.set_state(releases={"v1.3.0": REL}, tags=["v1.3.0"], branches=["main"])
        r = self.start("1.3.0", "refs/heads/main")
        self.assertEqual(r.returncode, 1)
        self.assertIn("gh workflow run release-resume.yml --ref v1.3.0 -f tag=v1.3.0", r.stderr)

    def test_tag_pushed_by_hand(self):
        self.set_state(releases={}, tags=["v1.3.0"], branches=["main"])
        r = self.start("1.3.0", "refs/heads/main")
        self.assertEqual(r.returncode, 1)
        self.assertIn("pushed by hand", r.stderr)

    def test_stray_tag_blocks(self):
        self.set_state(releases={}, tags=["v1.2.9"], branches=["main"])
        r = self.start("1.3.0", "refs/heads/main")
        self.assertEqual(r.returncode, 1)
        self.assertIn("v1.2.9", r.stderr)

    def test_minor_when_branch_exists(self):
        self.set_state(releases={}, tags=[], branches=["main", "release-1.3"])
        r = self.start("1.3.0", "refs/heads/main")
        self.assertEqual(r.returncode, 1)
        self.assertIn("release-1.3 already exists", r.stderr)

    def test_patch_without_branch(self):
        self.set_state(releases={}, tags=[], branches=["main"])
        r = self.start("1.2.1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("release-1.2", r.stderr)

    def test_branch_prefix_is_not_a_match(self):
        # release-1.20 must not count as release-1.2
        self.set_state(releases={}, tags=[], branches=["main", "release-1.20"])
        self.assertEqual(self.start("1.2.1").returncode, 1)

    def test_files_disagree(self):
        r = self.start("1.3.0", "refs/heads/main", files="1.2.0")
        self.assertEqual(r.returncode, 1)
        self.assertIn("Cargo.toml has version 1.2.0", r.stderr)

    def test_leading_v(self):
        r = self.start("v1.3.0", "refs/heads/main")
        self.assertEqual(r.returncode, 1)
        self.assertIn("without the v", r.stderr)


CODE_CHANGED = os.path.join(SCRIPTS, "code-changed.sh")
CI_RESULT = os.path.join(SCRIPTS, "ci-result.sh")


class CodeChangedTests(unittest.TestCase):
    """code-changed.sh: does a change touch anything CI has to test?"""

    def setUp(self):
        self.repo = tempfile.mkdtemp(prefix="repo-")
        self.addCleanup(shutil.rmtree, self.repo, ignore_errors=True)
        self.git("init", "-q")
        self.write("crates/x.rs", "fn main() {}\n")
        self.base = self.commit()

    def git(self, *args):
        return subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t",
                               *args], cwd=self.repo, check=True,
                              capture_output=True, text=True).stdout.strip()

    def write(self, path, text):
        full = os.path.join(self.repo, path)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "w") as f:
            f.write(text)

    def commit(self):
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "c")
        return self.git("rev-parse", "HEAD")

    def changed(self, base, head):
        r = subprocess.run([CODE_CHANGED, base, head], cwd=self.repo,
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        return r.stdout.strip()

    def test_docs_only_change_is_not_code(self):
        self.write("README.md", "hi\n")
        self.write("book/src/x.md", "hi\n")
        self.assertEqual(self.changed(self.base, self.commit()), "code=false")

    def test_site_only_change_is_not_code(self):
        self.write("site/index.html", "<p>hi</p>\n")
        self.assertEqual(self.changed(self.base, self.commit()), "code=false")

    def test_mixed_change_is_code(self):
        self.write("book/a.md", "hi\n")
        self.write("crates/x.rs", "fn main() { }\n")
        self.assertEqual(self.changed(self.base, self.commit()), "code=true")

    def test_code_renamed_to_docs_is_code(self):
        self.git("mv", "crates/x.rs", "README.md")
        self.assertEqual(self.changed(self.base, self.commit()), "code=true")

    def test_zero_base_is_code(self):
        self.write("README.md", "hi\n")
        head = self.commit()
        self.assertEqual(self.changed("0" * 40, head), "code=true")

    def test_unknown_base_is_code(self):
        self.write("README.md", "hi\n")
        head = self.commit()
        self.assertEqual(self.changed("1" * 40, head), "code=true")


class CiResultTests(unittest.TestCase):
    """ci-result.sh: one pass/fail for the whole CI run."""

    def run_result(self, needs):
        return subprocess.run([CI_RESULT, json.dumps(needs)],
                              capture_output=True, text=True)

    def test_all_success_passes(self):
        r = self.run_result({"a": {"result": "success"}, "b": {"result": "skipped"}})
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_cancelled_need_fails(self):
        r = self.run_result({"a": {"result": "success"}, "check": {"result": "cancelled"}})
        self.assertEqual(r.returncode, 1)
        self.assertIn("check", r.stderr)

    def test_failure_need_fails(self):
        r = self.run_result({"deny": {"result": "failure"}})
        self.assertEqual(r.returncode, 1)
        self.assertIn("deny", r.stderr)


if __name__ == "__main__":
    unittest.main()
