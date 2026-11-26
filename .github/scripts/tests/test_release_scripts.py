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
RELEASE_YML = os.path.join(SCRIPTS, "release-yml.sh")
RESUME = os.path.join(SCRIPTS, "resume-check.sh")
BUMP = os.path.join(SCRIPTS, "bump-version.sh")
STRAY = os.path.join(SCRIPTS, "stray-tags.sh")
START = os.path.join(SCRIPTS, "release-start-check.sh")
CHANGELOG = os.path.join(SCRIPTS, "changelog-section.sh")
PIN = os.path.join(SCRIPTS, "pin-release.sh")
BRANCH = os.path.join(SCRIPTS, "release-branch.sh")
LINE = os.path.join(SCRIPTS, "release-line.sh")
BUMP_BRANCH = os.path.join(SCRIPTS, "bump-branch.sh")
PREP_CHECK = os.path.join(SCRIPTS, "prepare-check.sh")
PREP_BRANCH = os.path.join(SCRIPTS, "prepare-branch.sh")
PREP_BUSY = os.path.join(SCRIPTS, "prepare-busy.sh")
FORMULA_MAN = os.path.join(SCRIPTS, "formula-man-pages.sh")
FORMULA = os.path.join(HERE, "fixtures", "irsync.rb")
HEADING = "## What's changed"
REL = {"body": "", "isDraft": False, "isPrerelease": False}
# A pipe into grep -q, also when the pipe ends a line and grep starts the
# next, but not ||. test_workflow_policy.py has the same for workflow steps.
PIPE_INTO_GREP_Q = re.compile(r"(?<!\|)\|(?!\|)\s*(\\\n\s*)?grep\s+(-\w*\s+)*-\w*q")


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


def version_fixture(test, cargo, dist, changelog=None):
    """A directory holding Cargo.toml and packaging/dist/dist.toml with these
    versions, and a CHANGELOG.md: `changelog`, or notes for the Cargo.toml
    version."""
    d = tempfile.mkdtemp(prefix="ver-")
    test.addCleanup(shutil.rmtree, d, ignore_errors=True)
    os.makedirs(os.path.join(d, "packaging", "dist"))
    with open(os.path.join(d, "Cargo.toml"), "w") as f:
        f.write(f'[workspace]\nmembers = []\n\n[workspace.package]\nversion = "{cargo}"\n'
                f'\n[package]\nversion = "9.9.9"\n')
    with open(os.path.join(d, "packaging", "dist", "dist.toml"), "w") as f:
        f.write(f'[package]\nname = "irsync"\nversion = "{dist}"\n')
    if changelog is None:
        changelog = f"# Changelog\n\n## v{cargo}\n\nWhat is new.\n"
    with open(os.path.join(d, "CHANGELOG.md"), "w") as f:
        f.write(changelog)
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
                   '[workspace]\nmembers = ["a"]\nexclude = ["fuzz"]\nresolver = "2"\n\n'
                   '[workspace.metadata.other]\nversion = "9.9.9"\n\n'
                   '[workspace.package]\nedition = "2021"\nversion = "1.2.3"\n')
        self.write("a/Cargo.toml",
                   '[package]\nname = "a"\nversion.workspace = true\nedition.workspace = true\n')
        self.write("a/src/lib.rs", "")
        # Like the real fuzz/: outside the workspace, with a lock file of its
        # own that records a's version.
        self.write("fuzz/Cargo.toml",
                   '[package]\nname = "a-fuzz"\nversion = "0.0.0"\nedition = "2021"\n'
                   'publish = false\n\n[dependencies]\na = { path = "../a" }\n')
        self.write("fuzz/src/lib.rs", "")
        self.write("packaging/dist/dist.toml",
                   '# Keep `version` equal to the workspace version.\n\n'
                   '[package]\nname = "irsync"\nversion = "1.2.3"\nlicense = "MIT"\n')
        self.run_ok("git", "init", "-q", "-b", "main")
        self.run_ok("cargo", "generate-lockfile", "--quiet")
        self.run_ok("cargo", "generate-lockfile", "--quiet", "--manifest-path", "fuzz/Cargo.toml")
        self.run_ok("git", "add", "-A")
        self.run_ok("git", "commit", "-q", "-m", "start")
        # bump-version.sh reads main's version from origin/main.
        self.run_ok("git", "update-ref", "refs/remotes/origin/main", "HEAD")

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
        self.assertIn("gh workflow run release-gate.yml --ref release-1.3 -f version=1.3.0-beta.1",
                      r.stdout)
        # The gate ends at a draft; signing it is the last step.
        self.assertEqual(r.stdout.rstrip().splitlines()[-1].strip(),
                         ".github/scripts/sign-release.sh --publish 1.3.0-beta.1")
        self.assertNotIn("git tag", r.stdout)
        self.assertNotIn("git push origin main", r.stdout)

    def test_hint_asks_for_the_release_notes(self):
        r = self.bump("1.3.0-beta.1")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("'## v1.3.0-beta.1'", r.stdout)
        self.assertIn("git commit -m 'Release notes for v1.3.0-beta.1' -- CHANGELOG.md", r.stdout)
        self.assertLess(r.stdout.index("CHANGELOG.md"), r.stdout.index("git push -u origin"))

    def test_hint_is_quiet_when_the_notes_are_written(self):
        self.write("CHANGELOG.md", "# Changelog\n\n## v1.3.0-beta.1\n\nNew.\n")
        self.run_ok("git", "add", "CHANGELOG.md")
        self.run_ok("git", "commit", "-q", "-m", "notes")
        r = self.bump("1.3.0-beta.1")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertNotIn("CHANGELOG.md", r.stdout)
        self.assertIn("Next:\n  git push -u origin bump-v1.3.0-beta.1", r.stdout)

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
        # The fuzz targets' own lock file too: CI builds them with --locked.
        self.assertIn('name = "a"\nversion = "1.3.0-beta.1"\n', self.read("fuzz/Cargo.lock"))
        self.run_ok("cargo", "metadata", "--locked", "--format-version", "1",
                    "--manifest-path", "fuzz/Cargo.toml")
        self.assertEqual(self.run_ok("git", "log", "-1", "--format=%s"), "Release v1.3.0-beta.1\n")
        self.assertEqual(self.run_ok("git", "status", "--porcelain"), "")
        self.assertEqual(self.run_ok("git", "show", "--name-only", "--format=").split(),
                         ["Cargo.lock", "Cargo.toml", "fuzz/Cargo.lock", "packaging/dist/dist.toml"])
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

    def test_prints_prepare_then_gate(self):
        self.set_main_version("1.3.0-alpha.1")
        r = self.bump("1.3.0-beta.1")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        prepare = "gh workflow run release-prepare.yml --ref main -f version=1.3.0-beta.1"
        gate = "gh workflow run release-gate.yml --ref release-1.3 -f version=1.3.0-beta.1"
        self.assertIn(prepare, r.stdout)
        self.assertIn(gate, r.stdout)
        self.assertLess(r.stdout.index(prepare), r.stdout.index(gate))
        self.assertIn("gh pr create --fill --base main", r.stdout)

    def test_line_main_has_left(self):
        self.set_main_version("1.4.0-alpha.1")
        r = self.bump("1.3.0")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("release-1.3", r.stderr)

    def test_patch_prints_no_prepare(self):
        self.run_ok("git", "switch", "-q", "-c", "release-1.2")
        r = self.bump("1.2.4")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertNotIn("release-prepare.yml", r.stdout)

    def test_no_origin_main(self):
        self.run_ok("git", "update-ref", "-d", "refs/remotes/origin/main")
        r = self.bump("1.3.0-beta.1")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("git fetch origin", r.stderr)

    def set_main_version(self, version):
        """A commit on main (and origin/main) with this workspace version."""
        cargo = self.read("Cargo.toml").replace('version = "1.2.3"', f'version = "{version}"')
        self.write("Cargo.toml", cargo)
        self.run_ok("git", "commit", "-q", "-am", "main moves on")
        self.run_ok("git", "update-ref", "refs/remotes/origin/main", "HEAD")
        self.run_ok("git", "reset", "-q", "--hard", "HEAD~1")


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

    def resume(self, tag, dry_run, release=None, ref=None, branches=("main",)):
        releases = {tag: release} if release else {}
        self.set_state(releases=releases, tags=[tag], branches=list(branches), generated="x")
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

    def test_no_branch_output(self):
        for tag, rel in [("v1.3.0", self.PUBLISHED), ("v1.3.0-beta.1", self.PRE)]:
            self.assertNotIn("new-branch", self.resume(tag, False, rel).stdout)

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
        for version, want in [("1.3.0-beta.1", "release-1.3"), ("1.3.0-alpha.2", "release-1.3"),
                              ("1.3.0", "release-1.3"), ("1.2.1", "release-1.2"),
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


class BumpBranchTests(unittest.TestCase):
    def bump_branch(self, version, main_version):
        return subprocess.run([BUMP_BRANCH, version, main_version], capture_output=True, text=True)

    def test_bump_branches(self):
        for version, main_version, want in [("1.3.0-beta.1", "1.3.0-alpha.2", "main"),
                                            ("1.3.0", "1.3.0-beta.2", "main"),
                                            ("1.3.0-alpha.1", "1.2.4", "main"),
                                            ("1.3.0-beta.3", "1.4.0-alpha.1", "release-1.3"),
                                            ("1.3.0", "1.4.0-alpha.1", "release-1.3"),
                                            ("1.3.0", "2.0.0-alpha.1", "release-1.3"),
                                            ("1.3.1", "1.3.0", "release-1.3"),
                                            ("1.3.1", "1.3.1", "release-1.3")]:
            r = self.bump_branch(version, main_version)
            self.assertEqual((r.returncode, r.stdout), (0, want + "\n"), (version, main_version))

    def test_bad_input(self):
        for version, main_version in [("1.3", "1.3.0"), ("v1.3.0", "1.3.0"),
                                      ("1.3.0", "1.3"), ("1.3.0", "x;y")]:
            r = self.bump_branch(version, main_version)
            self.assertNotEqual(r.returncode, 0, (version, main_version))
            self.assertEqual(r.stdout, "", (version, main_version))


class ReleaseLineTests(FakeGh):
    def line(self, version, tags=()):
        self.set_state(releases={}, tags=list(tags))
        return self.run_script(LINE, version)

    def test_first_alpha(self):
        r = self.line("1.3.0-alpha.1")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "line=1.3\nfirst=true\n")
        self.assertNotIn("::warning::", r.stderr)

    def test_first_beta_warns(self):
        r = self.line("1.3.0-beta.1")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "line=1.3\nfirst=true\n")
        self.assertIn("::warning::", r.stderr)
        self.assertIn("not the usual order", r.stderr)

    def test_first_stable_warns(self):
        r = self.line("1.3.0")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("::warning::", r.stderr)
        self.assertIn("not the usual order", r.stderr)

    def test_allowed_steps(self):
        for tags, version in [(["v1.3.0-alpha.1"], "1.3.0-alpha.2"),
                              (["v1.3.0-alpha.2"], "1.3.0-beta.1"),
                              (["v1.3.0-beta.1"], "1.3.0-beta.2"),
                              (["v1.3.0-alpha.2"], "1.3.0"),
                              (["v1.3.0-beta.2"], "1.3.0"),
                              (["v1.3.0"], "1.3.1"),
                              (["v1.3.0", "v1.3.1"], "1.3.2")]:
            with self.subTest(tags=tags, version=version):
                r = self.line(version, tags)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(r.stdout, "line=1.3\nfirst=false\n")
                self.assertNotIn("::warning::", r.stderr)

    def test_refused_steps(self):
        for tags, version, newest in [(["v1.3.0-beta.1"], "1.3.0-alpha.3", "1.3.0-beta.1"),
                                      (["v1.3.0"], "1.3.0-beta.3", "1.3.0"),
                                      (["v1.3.0-beta.2"], "1.3.0-beta.2", "1.3.0-beta.2"),
                                      (["v1.3.0-beta.2"], "1.3.0-beta.1", "1.3.0-beta.2"),
                                      (["v1.3.1"], "1.3.1", "1.3.1")]:
            with self.subTest(tags=tags, version=version):
                r = self.line(version, tags)
                self.assertEqual(r.returncode, 1)
                self.assertIn("::error::", r.stderr)
                self.assertIn(newest, r.stderr)
                self.assertEqual(r.stdout, "")

    def test_prerelease_of_patch_refused(self):
        r = self.line("1.3.1-beta.1", ["v1.3.0"])
        self.assertEqual(r.returncode, 1)
        self.assertIn("Pre-releases are only of X.Y.0", r.stderr)
        self.assertEqual(r.stdout, "")

    def test_line_prefix_is_not_a_match(self):
        r = self.line("1.2.0-alpha.1", ["v1.20.0-beta.1"])
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "line=1.2\nfirst=true\n")

    def test_old_tags_count(self):
        r = self.line("0.1.0-alpha.3", ["v0.1.0-alpha.1", "v0.1.0-alpha.2", "v0.1.0-beta.6"])
        self.assertEqual(r.returncode, 1)
        self.assertIn("0.1.0-beta.6", r.stderr)

    def test_failed_lookup_stops(self):
        self.set_state(releases={}, tags=[], api_fails=["git/matching-refs/tags/"])
        r = self.run_script(LINE, "1.3.0-alpha.1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("Could not read", r.stderr)
        self.assertEqual(r.stdout, "")

    def test_bad_version(self):
        for version in ["v1.3.0", "1.3", "1.3.0-rc.1"]:
            r = self.line(version)
            self.assertNotEqual(r.returncode, 0, version)
            self.assertEqual(r.stdout, "", version)


class PrepareCheckTests(FakeGh):
    def setUp(self):
        super().setUp()
        self.env["GITHUB_REF"] = "refs/heads/main"
        self.set_state(releases={"v1.3.0-alpha.1": REL}, tags=["v1.3.0-alpha.1"],
                       branches=["main", "release-1.3"])

    def check(self, version, fixed=None, files=None):
        files = files or version
        d = version_fixture(self, files, files)
        return self.run_script(PREP_CHECK, version, *([fixed] if fixed is not None else []), cwd=d)

    def update_state(self, **kw):
        state = self.state()
        state.update(kw)
        self.set_state(**state)

    def assert_refused(self, r, *messages):
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("::error::", r.stderr)
        for m in messages:
            self.assertIn(m, r.stderr)
        self.assertEqual(r.stdout, "")

    def test_ok(self):
        r = self.check("1.3.0-beta.1")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "line=1.3\nbranch=release-1.3\nfixed=\n")

    def test_new_line_ok(self):
        # The first release of a line: no branch, no tags yet.
        self.set_state(releases={}, tags=[], branches=["main"])
        r = self.check("1.4.0-alpha.1")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "line=1.4\nbranch=release-1.4\nfixed=\n")

    def test_not_on_main(self):
        self.env["GITHUB_REF"] = "refs/heads/release-1.3"
        self.assert_refused(self.check("1.3.0-beta.1"), "--ref main")

    def test_patch_refused(self):
        self.assert_refused(self.check("1.3.1"),
                            "gh workflow run release-gate.yml --ref release-1.3 -f version=1.3.1")

    def test_main_not_bumped(self):
        self.assert_refused(self.check("1.3.0-beta.1", files="1.3.0-alpha.1"),
                            "bump-version.sh 1.3.0-beta.1")

    def test_main_left_the_line(self):
        self.assert_refused(self.check("1.3.0", files="1.4.0-alpha.1"), "--base release-1.3",
                            "--ref release-1.3")

    def test_stable_out_refused(self):
        self.update_state(releases={"v1.3.0": REL}, tags=["v1.3.0"])
        self.assert_refused(self.check("1.3.0-beta.3"), "v1.3.0")

    def test_tag_exists_refused(self):
        self.update_state(releases={"v1.3.0-beta.1": REL}, tags=["v1.3.0-alpha.1", "v1.3.0-beta.1"])
        self.assert_refused(self.check("1.3.0-beta.1"), "v1.3.0-beta.1")

    def test_order_refused(self):
        self.update_state(tags=["v1.3.0-beta.2"])
        self.assert_refused(self.check("1.3.0-alpha.2"), "v1.3.0-beta.2")

    def test_draft_refused(self):
        self.update_state(releases={"v1.3.0-alpha.1": REL,
                                    "v1.3.0-beta.1": dict(REL, isDraft=True)})
        self.assert_refused(self.check("1.3.0-beta.2"), "v1.3.0-beta.1")

    def test_draft_of_other_line_is_fine(self):
        self.update_state(releases={"v1.3.0-alpha.1": REL,
                                    "v1.30.0-beta.1": dict(REL, isDraft=True)})
        self.assertEqual(self.check("1.3.0-beta.1").returncode, 0)

    def test_pin_refused(self):
        self.update_state(branches=["main", "release-1.3", "pinned/v1.3.0-beta.1"])
        self.assert_refused(self.check("1.3.0-beta.2"), "pinned/v1.3.0-beta.1")

    def test_gate_running_refused(self):
        for status in ["queued", "in_progress", "waiting"]:
            with self.subTest(status=status):
                self.update_state(runs=[{"workflow": "release-gate.yml", "status": "completed"},
                                        {"workflow": "release-gate.yml", "status": status}])
                self.assert_refused(self.check("1.3.0-beta.1"), "release gate")

    def test_finished_gate_is_fine(self):
        self.update_state(runs=[{"workflow": "release-gate.yml", "status": "completed"},
                                {"workflow": "site.yml", "status": "in_progress"}])
        self.assertEqual(self.check("1.3.0-beta.1").returncode, 0)

    def test_failed_lookup_stops(self):
        for path in ["git/matching-refs/tags/", "git/matching-refs/heads/", "releases"]:
            with self.subTest(path=path):
                self.update_state(api_fails=[path])
                self.assert_refused(self.check("1.3.0-beta.1"), "Could not read")

    def test_fixed_branch_name_refused(self):
        for fixed in ["release-1.3-fix", "release-1.3", "pinned/x", "main", "-x", "a b", "a;b"]:
            with self.subTest(fixed=fixed):
                self.update_state(calls=[])
                self.assert_refused(self.check("1.3.0-beta.1", fixed))
                self.assertEqual(self.state()["calls"], [])

    def test_fixed_branch_missing(self):
        self.assert_refused(self.check("1.3.0-beta.1", "fix-1.3"), "fix-1.3")

    def test_fixed_branch_ok(self):
        self.update_state(branches=["main", "release-1.3", "fix-1.3"])
        r = self.check("1.3.0-beta.1", "fix-1.3")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "line=1.3\nbranch=release-1.3\nfixed=fix-1.3\n")

    def test_empty_fixed_branch_is_none(self):
        r = self.check("1.3.0-beta.1", "")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("fixed=\n", r.stdout)


class PrepareBranchTests(unittest.TestCase):
    """prepare-branch.sh in a clone of a local bare repository."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="prep-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.com",
                        GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.com",
                        GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        self.origin = os.path.join(self.tmp, "origin.git")
        self.git(self.tmp, "init", "-q", "--bare", "-b", "main", self.origin)
        self.dev = os.path.join(self.tmp, "dev")
        self.git(self.tmp, "clone", "-q", self.origin, self.dev)
        self.git(self.dev, "switch", "-q", "-c", "main")
        self.commit("f.txt", "one\ntwo\nthree\n", "start")
        self.git(self.dev, "push", "-q", "origin", "main")

    def git(self, cwd, *args):
        r = subprocess.run(["git", *args], cwd=cwd, env=self.env, capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        return r.stdout.strip()

    def commit(self, path, text, subject):
        with open(os.path.join(self.dev, path), "w") as f:
            f.write(text)
        self.git(self.dev, "add", path)
        self.git(self.dev, "commit", "-q", "-m", subject)
        return self.git(self.dev, "rev-parse", "HEAD")

    def on(self, branch, start=None):
        if start:
            self.git(self.dev, "switch", "-q", "-C", branch, start)
        else:
            self.git(self.dev, "switch", "-q", branch)

    def push(self, *refs):
        self.git(self.dev, "push", "-q", "-f", "origin", *refs)

    def remote(self, branch):
        r = subprocess.run(["git", "rev-parse", "--verify", "-q", f"refs/heads/{branch}"],
                           cwd=self.origin, env=self.env, capture_output=True, text=True)
        return r.stdout.strip() or None

    def prepare(self, *args, hook=None, main=None):
        """Runs the script; main is the commit the checks saw (default: main now)."""
        work = os.path.join(self.tmp, "work")
        shutil.rmtree(work, ignore_errors=True)
        self.git(self.tmp, "clone", "-q", self.origin, work)
        if hook:
            path = os.path.join(work, ".git", "hooks", "pre-push")
            with open(path, "w") as f:
                f.write(hook)
            os.chmod(path, 0o755)
        if main is None:
            main = self.remote("main")
        line, *rest = args
        return subprocess.run([PREP_BRANCH, line, main, *rest], cwd=work, env=self.env,
                              capture_output=True, text=True)

    def own_commit(self):
        """release-1.3 at the first main commit plus one commit of its own."""
        self.on("release-1.3", "main")
        sha = self.commit("fix.txt", "fix\n", "A fix only for 1.3")
        self.push("release-1.3")
        self.on("main")
        return sha

    def test_creates_branch(self):
        r = self.prepare("1.3")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.remote("release-1.3"), self.remote("main"))
        self.assertIn("created release-1.3", r.stdout)

    def test_fast_forward(self):
        self.push("main:release-1.3")
        self.commit("g.txt", "g\n", "main moves on")
        self.push("main")
        r = self.prepare("1.3")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.remote("release-1.3"), self.remote("main"))

    def test_rebase_keeps_own_commit(self):
        self.own_commit()
        self.commit("g.txt", "g\n", "main moves on")
        self.push("main")
        r = self.prepare("1.3")
        self.assertEqual(r.returncode, 0, r.stderr)
        tip = self.remote("release-1.3")
        self.assertEqual(self.git(self.origin, "rev-parse", tip + "^"), self.remote("main"))
        self.assertEqual(self.git(self.origin, "log", "-1", "--format=%s", tip), "A fix only for 1.3")
        self.assertEqual(self.git(self.origin, "show", tip + ":fix.txt"), "fix")
        self.assertEqual(self.git(self.origin, "log", "-1", "--format=%cn", tip), "irsync release bot")
        self.assertIn("rebased release-1.3", r.stdout)

    def test_rebase_drops_commit_main_has(self):
        sha = self.own_commit()
        self.git(self.dev, "cherry-pick", sha)
        self.push("main")
        r = self.prepare("1.3")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.remote("release-1.3"), self.remote("main"))

    def conflict(self):
        self.on("release-1.3", "main")
        self.commit("f.txt", "one\nTWO on 1.3\nthree\n", "Change two for 1.3")
        self.push("release-1.3")
        self.on("main")
        self.commit("f.txt", "one\nTWO on main\nthree\n", "Change two on main")
        self.push("main")

    def test_conflict_changes_nothing(self):
        self.conflict()
        before = self.remote("release-1.3")
        r = self.prepare("1.3")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertEqual(self.remote("release-1.3"), before)
        self.assertIn("f.txt", r.stderr)
        self.assertIn("fixed-branch=", r.stderr)

    def test_lease_protects(self):
        self.own_commit()
        self.commit("g.txt", "g\n", "main moves on")
        self.push("main")
        # Someone pushes to release-1.3 while the script is about to push.
        other = os.path.join(self.tmp, "other")
        hook = (f"#!/bin/sh\nset -e\nrm -f \"$0\"\n"
                f"git clone -q {self.origin} {other}\ncd {other}\n"
                "git switch -q release-1.3\n"
                "git -c user.name=o -c user.email=o@example.com commit -q --allow-empty -m late\n"
                "git push -q origin release-1.3\n")
        r = self.prepare("1.3", hook=hook)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertEqual(self.git(self.origin, "log", "-1", "--format=%s", "release-1.3"), "late")

    def fixed(self, base, subjects_and_texts, name="fix-1.3"):
        self.on(name, base)
        for subject, text in subjects_and_texts:
            self.commit("f.txt", text, subject)
        self.push(name)
        self.on("main")

    def test_fixed_branch_ok(self):
        self.conflict()
        self.fixed("main", [("Change two for 1.3", "one\nTWO on both\nthree\n")])
        tip = self.remote("fix-1.3")
        r = self.prepare("1.3", "fix-1.3")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.remote("release-1.3"), tip)
        self.assertIsNone(self.remote("fix-1.3"))

    def test_fixed_branch_adds_commit_refused(self):
        self.conflict()
        before = self.remote("release-1.3")
        self.fixed("main", [("Change two for 1.3", "one\nTWO on both\nthree\n"),
                            ("Something new", "one\nTWO\nthree\n")])
        r = self.prepare("1.3", "fix-1.3")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertEqual(self.remote("release-1.3"), before)
        self.assertIsNotNone(self.remote("fix-1.3"))

    def test_fixed_branch_not_on_main_refused(self):
        self.conflict()
        before = self.remote("release-1.3")
        self.fixed("main~1", [("Change two for 1.3", "one\nTWO on both\nthree\n")])
        r = self.prepare("1.3", "fix-1.3")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertEqual(self.remote("release-1.3"), before)

    def test_fixed_branch_with_merge_refused(self):
        self.conflict()
        before = self.remote("release-1.3")
        self.on("side", "main")
        self.commit("s.txt", "s\n", "Change two for 1.3")
        self.on("fix-1.3", "main")
        self.git(self.dev, "merge", "-q", "--no-ff", "-m", "merge", "side")
        self.push("fix-1.3")
        r = self.prepare("1.3", "fix-1.3")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertEqual(self.remote("release-1.3"), before)

    def test_main_moved_since_the_check(self):
        self.own_commit()
        checked = self.remote("main")
        before = self.remote("release-1.3")
        self.commit("g.txt", "g\n", "main moves on")
        self.push("main")
        r = self.prepare("1.3", main=checked)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("main moved", r.stderr)
        self.assertEqual(self.remote("release-1.3"), before)

    def test_bad_main_sha(self):
        r = self.prepare("1.3", main="nonsense")
        self.assertNotEqual(r.returncode, 0)
        self.assertIsNone(self.remote("release-1.3"))

    def test_bad_arguments(self):
        for args in [("1.3.0",), ("x",), ("1.3", "pinned/x"), ("1.3", "main")]:
            with self.subTest(args=args):
                self.assertNotEqual(self.prepare(*args).returncode, 0)


class PrepareBusyTests(FakeGh):
    """prepare-busy.sh: is a release of the line under way?"""

    def setUp(self):
        super().setUp()
        self.set_state(releases={"v1.3.0-alpha.1": REL}, tags=["v1.3.0-alpha.1"],
                       branches=["main", "release-1.3"])

    def busy(self, **kw):
        state = self.state()
        state.update(kw)
        self.set_state(**state)
        return self.run_script(PREP_BUSY, "1.3")

    def test_idle(self):
        r = self.busy()
        self.assertEqual((r.returncode, r.stdout), (0, ""), r.stderr)

    def test_draft(self):
        r = self.busy(releases={"v1.3.0-beta.1": dict(REL, isDraft=True)})
        self.assertEqual(r.returncode, 1)
        self.assertIn("v1.3.0-beta.1", r.stderr)

    def test_draft_of_other_line(self):
        self.assertEqual(self.busy(releases={"v1.30.0-beta.1": dict(REL, isDraft=True)}).returncode, 0)

    def test_pin(self):
        r = self.busy(branches=["main", "pinned/v1.3.0-beta.1"])
        self.assertEqual(r.returncode, 1)
        self.assertIn("pinned/v1.3.0-beta.1", r.stderr)

    def test_gate_running(self):
        r = self.busy(runs=[{"workflow": "release-gate.yml", "status": "in_progress"}])
        self.assertEqual(r.returncode, 1)
        self.assertIn("release gate", r.stderr)

    def test_failed_lookup_stops(self):
        for path in ["releases", "git/matching-refs/heads/"]:
            with self.subTest(path=path):
                r = self.busy(api_fails=[path])
                self.assertEqual(r.returncode, 1)
                self.assertIn("Could not read", r.stderr)

    def test_bad_line(self):
        self.assertNotEqual(self.run_script(PREP_BUSY, "1.3.0").returncode, 0)


class StartCheckTests(FakeGh):
    def setUp(self):
        super().setUp()
        # On a runner GITHUB_REF is always set; here only where a test sets it.
        self.env.pop("GITHUB_REF", None)
        self.set_state(releases={}, tags=[], branches=["main", "release-1.3"])

    def start(self, version, ref=None, files=None, changelog=None):
        files = files or version
        d = version_fixture(self, files, files, changelog)
        if ref:
            self.env["GITHUB_REF"] = ref
        return self.run_script(START, version, cwd=d)

    def test_prerelease_from_release_branch(self):
        r = self.start("1.3.0-beta.1", "refs/heads/release-1.3")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "tag=v1.3.0-beta.1\nprerelease=true\nbranch=release-1.3\n")

    def test_minor_from_release_branch(self):
        self.set_state(releases={"v1.3.0-beta.2": REL}, tags=["v1.3.0-beta.2"],
                       branches=["main", "release-1.3"])
        r = self.start("1.3.0", "refs/heads/release-1.3")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "tag=v1.3.0\nprerelease=false\nbranch=release-1.3\n")
        self.assertNotIn("::warning::", r.stderr)

    def test_patch_from_release_branch(self):
        self.set_state(releases={"v1.2.0": REL}, tags=["v1.2.0"], branches=["main", "release-1.2"])
        r = self.start("1.2.1", "refs/heads/release-1.2")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "tag=v1.2.1\nprerelease=false\nbranch=release-1.2\n")

    def test_wrong_branch(self):
        self.set_state(releases={}, tags=[], branches=["main", "release-1.2", "release-1.3"])
        for version, ref, want in [("1.2.1", "refs/heads/main", "--ref release-1.2"),
                                   ("1.3.0-beta.1", "refs/heads/main", "--ref release-1.3"),
                                   ("1.3.0-beta.1", "refs/heads/release-1.2", "--ref release-1.3"),
                                   ("1.3.0", "refs/tags/v1.3.0", "--ref release-1.3")]:
            r = self.start(version, ref)
            self.assertEqual(r.returncode, 1, version)
            self.assertIn(f"gh workflow run release-gate.yml {want} -f version={version}", r.stderr)

    def test_branch_missing_points_to_prepare(self):
        self.set_state(releases={}, tags=[], branches=["main"])
        r = self.start("1.3.0-beta.1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("gh workflow run release-prepare.yml --ref main -f version=1.3.0-beta.1",
                      r.stderr)

    def test_order_refused(self):
        self.set_state(releases={"v1.3.0": REL}, tags=["v1.3.0"], branches=["main", "release-1.3"])
        r = self.start("1.3.0-beta.3", "refs/heads/release-1.3")
        self.assertEqual(r.returncode, 1)
        self.assertIn("v1.3.0", r.stderr)
        self.assertNotIn("tag=", r.stdout)

    def test_first_beta_warns(self):
        r = self.start("1.3.0-beta.1", "refs/heads/release-1.3")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("::warning::", r.stderr)

    def test_tag_exists_points_to_resume(self):
        self.set_state(releases={"v1.3.0": REL}, tags=["v1.3.0"], branches=["main", "release-1.3"])
        r = self.start("1.3.0", "refs/heads/release-1.3")
        self.assertEqual(r.returncode, 1)
        self.assertIn("gh workflow run release-resume.yml --ref v1.3.0 -f tag=v1.3.0", r.stderr)

    def test_failed_lookup_stops(self):
        # A failed gh call must not read as "the tag (or branch) is not there".
        for path in ["git/matching-refs/tags/", "git/matching-refs/heads/"]:
            with self.subTest(path=path):
                self.set_state(releases={}, tags=[], branches=["main", "release-1.3"],
                               api_fails=[path])
                r = self.start("1.3.0", "refs/heads/release-1.3")
                self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
                self.assertIn(f"Could not read repos/irsync-dev/irsync/{path}", r.stderr)
                self.assertNotIn("tag=", r.stdout)

    def test_tag_pushed_by_hand(self):
        self.set_state(releases={}, tags=["v1.3.0"], branches=["main", "release-1.3"])
        r = self.start("1.3.0", "refs/heads/release-1.3")
        self.assertEqual(r.returncode, 1)
        self.assertIn("pushed by hand", r.stderr)

    def test_stray_tag_blocks(self):
        self.set_state(releases={}, tags=["v1.2.9"], branches=["main", "release-1.3"])
        r = self.start("1.3.0", "refs/heads/release-1.3")
        self.assertEqual(r.returncode, 1)
        self.assertIn("v1.2.9", r.stderr)

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
        r = self.start("1.3.0", "refs/heads/release-1.3", files="1.2.0")
        self.assertEqual(r.returncode, 1)
        self.assertIn("Cargo.toml has version 1.2.0", r.stderr)

    def test_leading_v(self):
        r = self.start("v1.3.0", "refs/heads/release-1.3")
        self.assertEqual(r.returncode, 1)
        self.assertIn("without the v", r.stderr)

    def test_no_release_notes(self):
        # Notes for another version do not count.
        r = self.start("1.3.0", "refs/heads/release-1.3",
                       changelog="# Changelog\n\n## v1.3.0-beta.1\n\nOlder.\n")
        self.assertEqual(r.returncode, 1)
        self.assertIn("CHANGELOG.md has no section '## v1.3.0'", r.stderr)
        self.assertNotIn("tag=", r.stdout)


class ChangelogTests(unittest.TestCase):

    """changelog-section.sh, in a directory holding only a CHANGELOG.md."""

    def section(self, version, text):
        d = tempfile.mkdtemp(prefix="notes-")
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        if text is not None:
            with open(os.path.join(d, "CHANGELOG.md"), "w") as f:
                f.write(text)
        return subprocess.run([CHANGELOG, version], cwd=d, capture_output=True, text=True)

    def refused(self, version, text, message):
        r = self.section(version, text)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn(message, r.stderr)
        self.assertEqual(r.stdout, "")

    def test_prints_the_section(self):
        text = ("# Changelog\n\nIntro.\n\n## v1.3.0\n\nNew things.\n\n### Upgrading\n\n- one\n"
                "\n## v1.3.0-beta.1\n\nOlder.\n")
        r = self.section("1.3.0", text)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "New things.\n\n### Upgrading\n\n- one")
        r = self.section("1.3.0-beta.1", text)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "Older.")

    def test_no_file(self):
        self.refused("1.3.0", None, "There is no CHANGELOG.md")

    def test_no_section(self):
        self.refused("1.3.0", "# Changelog\n\n## v1.2.0\n\nOld.\n", "has no section '## v1.3.0'")

    def test_longer_version_is_not_a_match(self):
        # The heading must be the whole line: v1.3.0 is not v1.3.0-beta.1.
        self.refused("1.3.0", "## v1.3.0-beta.1\n\nText.\n", "has no section '## v1.3.0'")
        self.refused("1.3.0", "## 1.3.0\n\nText.\n", "has no section '## v1.3.0'")

    def test_empty_section(self):
        self.refused("1.3.0", "## v1.3.0\n\n   \n## v1.2.0\n\nOld.\n", "is empty")
        self.refused("1.3.0", "## v1.3.0\n", "is empty")

    def test_heading_twice(self):
        self.refused("1.3.0", "## v1.3.0\n\nA.\n\n## v1.3.0\n\nB.\n", "2 times")

    def test_this_repo(self):
        # The same rule as crates/irsync/tests/release_version.rs.
        with open(os.path.join(ROOT, "Cargo.toml"), "rb") as f:
            import tomllib
            version = tomllib.load(f)["workspace"]["package"]["version"]
        r = subprocess.run([CHANGELOG, version], cwd=ROOT, capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_current_reads_the_version_from_cargo_toml(self):
        d = version_fixture(self, "1.3.0", "1.3.0")
        r = subprocess.run([CHANGELOG, "--current"], cwd=d, capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "What is new.")

    def test_current_without_notes(self):
        d = version_fixture(self, "1.3.0", "1.3.0", changelog="## v1.2.0\n\nOld.\n")
        r = subprocess.run([CHANGELOG, "--current"], cwd=d, capture_output=True, text=True)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("has no section '## v1.3.0'", r.stderr)


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

    def test_man_page_sources_are_code(self):
        # tools/build-man.sh builds the man pages from these, and CI checks it.
        for path in ["book/src/reference/command-line.md",
                     "book/src/reference/management.md",
                     "book/src/reference/remote-addresses.md",
                     "book/src/reference/files-and-state.md",
                     "book/src/reference/limitations.md",
                     "book/src/reference/configuration.md",
                     "book/src/fileset/reference.md",
                     "packaging/man/irsync.md"]:
            # Each path on its own: a change from the previous commit.
            base = self.git("rev-parse", "HEAD")
            self.write(path, "changed\n")
            self.assertEqual(self.changed(base, self.commit()), "code=true", path)

    def test_other_book_chapter_is_not_code(self):
        self.write("book/src/reference/metrics.md", "changed\n")
        self.assertEqual(self.changed(self.base, self.commit()), "code=false")

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

    def test_skipped_fuzz_build_passes(self):
        # A docs-only pull request skips fuzz-build like the other builds.
        r = self.run_result({"changes": {"result": "success"}, "fuzz-build": {"result": "skipped"}})
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_cancelled_need_fails(self):
        r = self.run_result({"a": {"result": "success"}, "check": {"result": "cancelled"}})
        self.assertEqual(r.returncode, 1)
        self.assertIn("check", r.stderr)

    def test_failure_need_fails(self):
        r = self.run_result({"deny": {"result": "failure"}})
        self.assertEqual(r.returncode, 1)
        self.assertIn("deny", r.stderr)


class FormulaManPagesTests(unittest.TestCase):
    """formula-man-pages.sh: adds the man page lines to dist's formula."""

    ANCHOR = 'leftover_contents = Dir["*"] - doc_files'
    MAN1 = '    man1.install Dir["man/man1/*.1"]'
    MAN5 = '    man5.install Dir["man/man5/*.5"]'

    def setUp(self):
        d = tempfile.mkdtemp(prefix="formula-")
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        self.path = os.path.join(d, "irsync.rb")
        shutil.copy(FORMULA, self.path)

    def text(self):
        with open(self.path) as f:
            return f.read()

    def write(self, text):
        with open(self.path, "w") as f:
            f.write(text)

    def run_script(self):
        return subprocess.run([FORMULA_MAN, self.path], capture_output=True, text=True)

    def test_inserts_before_leftovers(self):
        r = self.run_script()
        self.assertEqual(r.returncode, 0, r.stderr)
        lines = self.text().splitlines()
        for want in (self.MAN1, self.MAN5):
            self.assertEqual(lines.count(want), 1)
        anchor = next(i for i, l in enumerate(lines) if self.ANCHOR in l)
        self.assertEqual(lines[anchor - 2:anchor], [self.MAN1, self.MAN5])
        last_bin = max(i for i, l in enumerate(lines) if "bin.install " in l)
        self.assertLess(last_bin, lines.index(self.MAN1))

    def test_only_two_lines_added(self):
        self.run_script()
        with open(FORMULA) as f:
            old = f.read().splitlines()
        new = self.text().splitlines()
        self.assertEqual(len(new), len(old) + 2)
        self.assertEqual([l for l in new if l not in (self.MAN1, self.MAN5)], old)

    def test_idempotent(self):
        self.run_script()
        once = self.text()
        r = self.run_script()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.text(), once)

    def test_already_present_with_other_indent_is_unchanged(self):
        text = self.text().replace(
            "    " + self.ANCHOR,
            '  man1.install Dir["man/man1/*.1"]\n\tman5.install Dir["man/man5/*.5"]\n    ' + self.ANCHOR)
        self.write(text)
        r = self.run_script()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.text(), text)

    def test_only_one_line_present_fails(self):
        text = self.text().replace("    " + self.ANCHOR, self.MAN1 + "\n    " + self.ANCHOR)
        self.write(text)
        r = self.run_script()
        self.assertEqual(r.returncode, 1)
        self.assertIn("one of the two", r.stderr)
        self.assertEqual(self.text(), text)

    def test_missing_anchor_fails(self):
        text = self.text().replace(self.ANCHOR, "leftovers = []")
        self.write(text)
        r = self.run_script()
        self.assertEqual(r.returncode, 1)
        self.assertIn("leftover_contents", r.stderr)
        self.assertEqual(self.text(), text)

    def test_duplicate_anchor_fails(self):
        text = self.text()
        text = text.replace("  end\nend\n", "    " + self.ANCHOR + "\n  end\nend\n")
        self.assertEqual(text.count(self.ANCHOR), 2)
        self.write(text)
        r = self.run_script()
        self.assertEqual(r.returncode, 1)
        self.assertIn("leftover_contents", r.stderr)
        self.assertEqual(self.text(), text)

    @unittest.skipUnless(shutil.which("ruby"), "ruby is not installed")
    def test_result_is_valid_ruby(self):
        self.run_script()
        r = subprocess.run(["ruby", "-c", self.path], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)


class ReleaseYmlTests(unittest.TestCase):
    """release-yml.sh, in a temp git repo, with a fake dist that writes a canned workflow."""
    CONFIG = '[dist]\nci = "github"\nallow-dirty = ["ci"]\n'
    OLD = "old release.yml\n"
    CREATE = ('          gh release create "$TAG" --title "$T" --notes-file n.txt'
              ' artifacts/*\n')
    DRAFT = CREATE.replace(" artifacts/*", " --draft artifacts/*")
    CLEANUP = "          rm -f artifacts/*-dist-manifest.json\n"
    COPY = ("          # Our own copy, not the artifact: the artifact list can miss it.\n"
            "          cp dist-manifest.json artifacts/dist-manifest.json\n")

    def setUp(self):
        self.repo = tempfile.mkdtemp(prefix="relyml-")
        self.addCleanup(shutil.rmtree, self.repo, ignore_errors=True)
        subprocess.run(["git", "init", "-q", self.repo], check=True)
        os.makedirs(os.path.join(self.repo, ".github", "workflows"))
        self.write_repo("dist-workspace.toml", self.CONFIG)
        self.write_repo(".github/workflows/release.yml", self.OLD)
        bindir = os.path.join(self.repo, "fakebin")
        os.mkdir(bindir)
        # The fake dist insists that allow-dirty is gone, as dist needs it to be.
        with open(os.path.join(bindir, "dist"), "w") as f:
            f.write('#!/bin/sh\n'
                    '[ "$1 $2" = "generate --mode=ci" ] || exit 2\n'
                    '! grep -q allow-dirty dist-workspace.toml || exit 3\n'
                    'printf "%s" "$FAKE_WORKFLOW" > .github/workflows/release.yml\n')
        os.chmod(os.path.join(bindir, "dist"), 0o755)
        self.bindir = bindir

    def write_repo(self, name, text):
        with open(os.path.join(self.repo, name), "w") as f:
            f.write(text)

    def read_repo(self, name):
        with open(os.path.join(self.repo, name)) as f:
            return f.read()

    def run_script(self, mode, create_lines=1, cleanup_lines=1):
        workflow = "jobs:\n" + self.CLEANUP * cleanup_lines + self.CREATE * create_lines + "done\n"
        env = dict(os.environ, PATH=self.bindir + os.pathsep + os.environ["PATH"],
                   FAKE_WORKFLOW=workflow)
        return subprocess.run([RELEASE_YML, mode], cwd=self.repo, env=env,
                              capture_output=True, text=True)

    def assert_untouched(self):
        self.assertEqual(self.read_repo("dist-workspace.toml"), self.CONFIG)
        self.assertEqual(self.read_repo(".github/workflows/release.yml"), self.OLD)

    WRITTEN = "jobs:\n" + CLEANUP + COPY + DRAFT + "done\n"

    def test_write_adds_draft_before_artifacts(self):
        r = self.run_script("write")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.read_repo(".github/workflows/release.yml"),
                         self.WRITTEN)
        self.assertEqual(self.read_repo("dist-workspace.toml"), self.CONFIG)

    def test_no_create_line_fails(self):
        r = self.run_script("write", create_lines=0)
        self.assertEqual(r.returncode, 1)
        self.assertIn("found 0", r.stderr)
        self.assert_untouched()

    def test_two_create_lines_fail(self):
        r = self.run_script("write", create_lines=2)
        self.assertEqual(r.returncode, 1)
        self.assertIn("found 2", r.stderr)
        self.assert_untouched()

    def test_write_copies_the_manifest_into_artifacts(self):
        # Right after dist's cleanup, so the attest step and the release see it.
        r = self.run_script("write")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(self.CLEANUP + self.COPY, self.read_repo(".github/workflows/release.yml"))

    def test_no_cleanup_line_fails(self):
        for n in [0, 2]:
            with self.subTest(cleanup_lines=n):
                r = self.run_script("write", cleanup_lines=n)
                self.assertEqual(r.returncode, 1)
                self.assertIn(f"found {n}", r.stderr)
                self.assertIn("dist-manifest.json", r.stderr)
                self.assert_untouched()

    def test_check_fails_with_a_diff(self):
        r = self.run_script("check")
        self.assertEqual(r.returncode, 1)
        self.assertIn("-old release.yml", r.stdout)
        self.assertIn("+" + self.DRAFT.rstrip("\n"), r.stdout)
        self.assert_untouched()

    def test_check_passes_after_write(self):
        self.assertEqual(self.run_script("write").returncode, 0)
        r = self.run_script("check")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.read_repo(".github/workflows/release.yml"),
                         self.WRITTEN)
        self.assertEqual(self.read_repo("dist-workspace.toml"), self.CONFIG)

    def test_dist_failure_restores_files(self):
        os.remove(os.path.join(self.bindir, "dist"))
        with open(os.path.join(self.bindir, "dist"), "w") as f:
            f.write("#!/bin/sh\necho junk > .github/workflows/release.yml\nexit 1\n")
        os.chmod(os.path.join(self.bindir, "dist"), 0o755)
        r = self.run_script("write")
        self.assertNotEqual(r.returncode, 0)
        self.assert_untouched()

    def test_usage(self):
        r = self.run_script("nonsense")
        self.assertEqual(r.returncode, 2)


SIGN = os.path.join(SCRIPTS, "sign-release.sh")
MINISIGN_FIXTURES = os.path.join(HERE, "fixtures", "minisign")
TEST_KEY = os.path.join(MINISIGN_FIXTURES, "test.key")
TEST_PUB = os.path.join(MINISIGN_FIXTURES, "test.pub")
# A public key nobody has the secret key for.
OTHER_PUB = ("untrusted comment: some other key\n"
             "RWQBAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4fICEiIyQlJico\n")


COMMIT = "0123abcd" * 5


def release_targets():
    import tomllib
    with open(os.path.join(ROOT, "dist-workspace.toml"), "rb") as f:
        return tomllib.load(f)["dist"]["targets"]


def sha256(text):
    import hashlib
    return hashlib.sha256(text.encode()).hexdigest()


class SignTests(FakeGh):
    """sign-release.sh, with fake gh and minisign (fake_minisign.py)."""

    REAL_MINISIGN = None

    def setUp(self):
        super().setUp()
        bindir = os.path.join(self.tmp, "bin")
        os.symlink(os.path.join(HERE, "fake_minisign.py"), os.path.join(bindir, "minisign"))
        self.minisign_log = os.path.join(self.tmp, "minisign.log")
        open(self.minisign_log, "w").close()
        self.env.update(FAKE_MINISIGN_LOG=self.minisign_log, IRSYNC_SIGN_KEYS_FILE=TEST_PUB)
        if self.REAL_MINISIGN:
            self.env["FAKE_MINISIGN_REAL"] = self.REAL_MINISIGN
        self.files = {}
        for t in release_targets():
            self.files[f"irsync-{t}.tar.xz"] = f"tarball for {t}"
        self.files["source.tar.gz"] = "source"
        for name in list(self.files):
            self.files[name + ".sha256"] = f"{sha256(self.files[name])} *{name}\n"
        self.files["irsync-installer.sh"] = "#!/bin/sh\n"
        self.files["irsync.rb"] = "class Irsync\nend\n"
        self.files["dist-manifest.json"] = "{}"
        self.files["sha256.sum"] = "".join(
            f"{sha256(c)} *{n}\n" for n, c in self.files.items()
            if not n.endswith(".sha256")) + "\n"
        self.release = {"body": "", "isDraft": True, "isPrerelease": False, "databaseId": 42,
                        "targetCommitish": COMMIT, "immutable": False,
                        "assets": dict(self.files)}
        self.tags = []
        with open(TEST_PUB) as f:
            self.extra_state = {"keys_at_commit": f.read(), "attested_commit": COMMIT,
                                "attested_ref": "refs/heads/pinned/v1.2.3"}

    def sign(self, *opts, answer="y\n", version="1.2.3", keys=None, publish=True):
        self.set_state(releases={f"v{version}": self.release}, tags=self.tags,
                       generated="", **self.extra_state)
        open(self.minisign_log, "w").close()
        if keys is not None:
            path = os.path.join(self.tmp, "keys.pub")
            with open(path, "w") as f:
                f.write(keys)
            self.env["IRSYNC_SIGN_KEYS_FILE"] = path
            self.set_state(**dict(self.state(), keys_at_commit=keys))
        if publish:
            opts = ("--publish", *opts)
        return subprocess.run([SIGN, "--key", TEST_KEY, *opts, version], env=self.env,
                              input=answer, capture_output=True, text=True)

    def calls(self, *prefix):
        return [c for c in self.state().get("calls", []) if c[:len(prefix)] == list(prefix)]

    def minisign_calls(self):
        with open(self.minisign_log) as f:
            return [json.loads(l) for l in f]

    def signing_calls(self):
        return [c for c in self.minisign_calls() if c[0] == "-S"]

    def uploaded(self):
        return [os.path.basename(a) for c in self.calls("release", "upload")
                for a in c[3:] if not a.startswith("-")]

    def assert_refused(self, r, *words):
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        for w in words:
            self.assertIn(w, r.stderr)

    def assert_nothing_signed(self):
        self.assertEqual(self.signing_calls(), [])
        self.assertEqual(self.calls("release", "upload"), [])
        self.assertEqual(self.calls("api", "-X", "PATCH"), [])
        self.assertEqual(self.calls("workflow"), [])

    def test_signs_uploads_publishes_and_resumes(self):
        r = self.sign()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn(COMMIT, r.stdout)
        for name in self.files:
            self.assertIn(name, r.stdout)
        signing = self.signing_calls()
        self.assertEqual(len(signing), 1)
        self.assertEqual(signing[0][signing[0].index("-t") + 1], "irsync v1.2.3")
        self.assertEqual(sorted(os.path.basename(f) for f in signing[0][signing[0].index("-m") + 1:]),
                         sorted(self.files))
        upload = self.calls("release", "upload")
        self.assertEqual(len(upload), 1)
        self.assertEqual(upload[0][2], "v1.2.3")
        self.assertIn("--clobber", upload[0])
        self.assertEqual(sorted(self.uploaded()), sorted(n + ".minisig" for n in self.files))
        calls = self.state()["calls"]
        patch = calls.index(["api", "-X", "PATCH", "repos/irsync-dev/irsync/releases/42",
                             "-F", "draft=false", "-f", "make_latest=legacy"])
        self.assertLess(calls.index(upload[0]), patch)
        resume = calls.index(["workflow", "run", "release-resume.yml", "--ref", "v1.2.3",
                              "-f", "tag=v1.2.3"])
        self.assertLess(patch, resume)
        # Then it rebuilds the staging site: the tag exists now, so the site
        # build reads the release from it.
        site = calls.index(["run", "list", "--workflow", "site.yml", "--branch", "main",
                            "--event", "push", "--limit", "1", "--json", "databaseId",
                            "--jq", ".[0].databaseId // empty"])
        rerun = calls.index(["run", "rerun", "7001"])
        self.assertLess(resume, site)
        self.assertLess(site, rerun)
        self.assertEqual(calls[-1], calls[rerun])
        self.assertIn("Rebuilding the staging site (run 7001), so it shows v1.2.3 as published.\n",
                      r.stdout)
        # It ends by saying how to promote the site; it does not start it.
        self.assertTrue(r.stdout.endswith(
            "To put v1.2.3 on irsync.org: wait for that run, check that https://dev.irsync.org/ shows it, then run:\n"
            "  gh workflow run site.yml --ref main\n"), r.stdout)

    def test_publish_survives_a_failed_site_rerun(self):
        # The release is published by then: a site that cannot be rebuilt is
        # a message, not a failure.
        self.extra_state["site_rerun_fails"] = True
        r = self.sign()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertFalse(self.state()["releases"]["v1.2.3"]["isDraft"])
        self.assertIn("Could not rebuild the staging site.", r.stdout)
        self.assertIn("gh run rerun", r.stdout)
        self.assertNotIn("Rebuilding the staging site", r.stdout)
        self.assertTrue(r.stdout.endswith(
            "To put v1.2.3 on irsync.org: wait for that run, check that https://dev.irsync.org/ shows it, then run:\n"
            "  gh workflow run site.yml --ref main\n"), r.stdout)

    def test_publish_survives_no_site_run(self):
        self.extra_state["no_site_run"] = True
        r = self.sign()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.calls("run", "rerun"), [])
        self.assertIn("Could not rebuild the staging site.", r.stdout)
        self.assertTrue(r.stdout.endswith(
            "To put v1.2.3 on irsync.org: wait for that run, check that https://dev.irsync.org/ shows it, then run:\n"
            "  gh workflow run site.yml --ref main\n"), r.stdout)
        rel = self.state()["releases"]["v1.2.3"]
        self.assertFalse(rel["isDraft"])
        for name in self.files:
            self.assertIn("trusted comment: irsync v1.2.3\n", rel["assets"][name + ".minisig"])

    def test_publishes_with_make_latest_legacy(self):
        r = self.sign()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.state()["patched"], {"draft": "false", "make_latest": "legacy"})
        self.assertEqual(self.calls("release", "edit"), [])

    def test_refuses_published_release(self):
        self.release["isDraft"] = False
        r = self.sign()
        self.assert_refused(r, "not a draft",
                            "gh workflow run release-resume.yml --ref v1.2.3 -f tag=v1.2.3")
        self.assertEqual(self.calls("release", "download"), [])
        self.assert_nothing_signed()

    def test_refuses_missing_release(self):
        self.set_state(releases={}, tags=[])
        r = subprocess.run([SIGN, "1.2.3"], env=self.env, input="y\n",
                           capture_output=True, text=True)
        self.assert_refused(r, "v1.2.3")
        self.assert_nothing_signed()

    def test_refuses_missing_file(self):
        del self.release["assets"]["irsync.rb"]
        r = self.sign()
        self.assert_refused(r, "missing", "irsync.rb")
        self.assert_nothing_signed()

    def test_refuses_extra_file(self):
        self.release["assets"]["notes.txt"] = "hello"
        r = self.sign()
        self.assert_refused(r, "notes.txt")
        self.assert_nothing_signed()

    def test_refuses_signature_of_unknown_file(self):
        self.release["assets"]["notes.txt.minisig"] = "old"
        r = self.sign()
        self.assert_refused(r, "notes.txt.minisig")
        self.assert_nothing_signed()

    def test_refuses_checksum_mismatch(self):
        target = release_targets()[0]
        cases = [
            # The tarball no longer matches its .sha256 nor sha256.sum.
            (f"irsync-{target}.tar.xz", "changed", f"irsync-{target}.tar.xz"),
            # Only sha256.sum is wrong.
            ("sha256.sum", self.files["sha256.sum"].replace(
                sha256(self.files["irsync.rb"]), sha256("other")), "irsync.rb"),
            # Only the .sha256 file is wrong.
            ("source.tar.gz.sha256", f"{sha256('other')} *source.tar.gz\n", "source.tar.gz"),
        ]
        for name, content, named in cases:
            with self.subTest(name=name):
                self.release["assets"] = dict(self.files, **{name: content})
                r = self.sign()
                self.assert_refused(r, "checksum", named)
                self.assert_nothing_signed()

    def test_refuses_failed_attestation(self):
        self.extra_state["bad_attestations"] = ["irsync.rb"]
        r = self.sign()
        self.assert_refused(r, "attestation", "irsync.rb")
        self.assert_nothing_signed()
        attest = self.calls("attestation", "verify")
        self.assertIn(["--repo", "irsync-dev/irsync", "--signer-workflow",
                       "irsync-dev/irsync/.github/workflows/release.yml",
                       "--source-digest", COMMIT, "--source-ref", "refs/heads/pinned/v1.2.3",
                       "--deny-self-hosted-runners"],
                      [c[3:] for c in attest])

    def test_attestation_must_name_the_commit_and_pinned_branch(self):
        for key, value in [("attested_commit", "f" * 40),
                           ("attested_ref", "refs/heads/some-branch")]:
            with self.subTest(key=key):
                self.extra_state[key] = value
                r = self.sign()
                self.assert_refused(r, "attestation")
                self.assert_nothing_signed()
                del self.extra_state[key]

    def test_commit_must_be_a_full_sha(self):
        for commit in ["0123abcd", "main", "0123ABCD" * 5]:
            with self.subTest(commit=commit):
                self.release["targetCommitish"] = commit
                r = self.sign()
                self.assert_refused(r, "full commit id")
                self.assertEqual(self.calls("release", "download"), [])
                self.assert_nothing_signed()

    def test_commit_must_be_on_the_release_branch(self):
        for version, branch in [("1.2.3", "release-1.2"), ("1.3.0", "release-1.3")]:
            for status in ["identical", "ahead"]:
                with self.subTest(version=version, status=status):
                    self.extra_state.update(compare=status,
                                            attested_ref=f"refs/heads/pinned/v{version}")
                    r = self.sign(version=version, publish=False)
                    self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
                    self.assertEqual(self.calls("api")[0][1],
                                     f"repos/irsync-dev/irsync/compare/{COMMIT}...{branch}")
            for status in ["behind", "diverged"]:
                with self.subTest(version=version, status=status):
                    self.extra_state["compare"] = status
                    r = self.sign(version=version)
                    self.assert_refused(r, COMMIT, branch, status)
                    self.assertEqual(self.calls("release", "download"), [])
                    self.assert_nothing_signed()

    def test_keys_must_match_the_commit(self):
        r = self.sign()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn(["api", f"repos/irsync-dev/irsync/contents/packaging/signing/"
                       f"release-keys.pub?ref={COMMIT}", "--jq", ".content | @base64d"],
                      self.calls("api"))
        self.extra_state["keys_at_commit"] = OTHER_PUB
        r = self.sign()
        self.assert_refused(r, "not the same", COMMIT)
        self.assertEqual(self.calls("release", "download"), [])
        self.assert_nothing_signed()

    def test_keys_file_missing_at_the_commit(self):
        del self.extra_state["keys_at_commit"]
        r = self.sign()
        self.assert_refused(r, "release-keys.pub", COMMIT)
        self.assert_nothing_signed()

    def test_answer_no_signs_nothing(self):
        for answer in ["n\n", "\n", ""]:
            with self.subTest(answer=answer):
                r = self.sign(answer=answer)
                self.assert_refused(r)
                self.assertIn("Sign these files? [y/N]", r.stdout + r.stderr)
                self.assert_nothing_signed()

    def test_without_publish_stops_after_upload(self):
        r = self.sign(publish=False)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn(f"--key {TEST_KEY} --publish 1.2.3", r.stdout)
        self.assertEqual(sorted(self.uploaded()), sorted(n + ".minisig" for n in self.files))
        self.assertEqual(self.calls("api", "-X", "PATCH"), [])
        self.assertEqual(self.calls("workflow"), [])
        self.assertTrue(self.state()["releases"]["v1.2.3"]["isDraft"])
        self.assertNotIn("irsync.org", r.stdout)
        self.assertEqual(self.calls("run"), [])

    def test_replaces_stale_signatures(self):
        self.release["assets"]["irsync.rb.minisig"] = "stale"
        r = self.sign()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("--clobber", self.calls("release", "upload")[0])
        self.assertIn("irsync.rb.minisig", self.uploaded())
        new = self.state()["releases"]["v1.2.3"]["assets"]["irsync.rb.minisig"]
        self.assertIn("trusted comment: irsync v1.2.3\n", new)

    def test_reads_is_immutable_before_signing(self):
        # An old gh does not know isImmutable; it must fail in step 2.
        r = self.sign()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        first = self.calls("release", "view")[0]
        self.assertIn("isImmutable", first[first.index("--json") + 1].split(","))

    def test_checksum_on_a_last_line_without_newline(self):
        lines = [l for l in self.files["sha256.sum"].splitlines() if l]
        good = "\n".join(lines)
        bad = "\n".join(lines[:-1] + [f"{sha256('other')} {lines[-1].split()[1]}"])
        named = lines[-1].split()[1].lstrip("*")
        with self.subTest("good"):
            self.release["assets"] = dict(self.files, **{"sha256.sum": good})
            r = self.sign(publish=False)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        with self.subTest("bad"):
            self.release["assets"] = dict(self.files, **{"sha256.sum": bad})
            r = self.sign()
            self.assert_refused(r, "checksum", named)
            self.assert_nothing_signed()

    def test_verifies_prehashed_only(self):
        r = self.sign()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        verify = [c for c in self.minisign_calls() if c[0] == "-V"]
        self.assertTrue(verify)
        for c in verify:
            self.assertIn("-H", c)

    def test_refuses_publish_when_an_asset_is_swapped(self):
        target = release_targets()[0]
        for name in [f"irsync-{target}.tar.xz", "sha256.sum.minisig"]:
            with self.subTest(name=name):
                self.extra_state["swap_on_upload"] = {name: "swapped"}
                r = self.sign()
                self.assert_refused(r, name, "not the ones that were signed", "still a draft")
                self.assertEqual(self.calls("api", "-X", "PATCH"), [])
                self.assertEqual(self.calls("workflow"), [])

    def test_refuses_publish_when_an_asset_is_added(self):
        self.extra_state["swap_on_upload"] = {"notes.txt": "hello"}
        r = self.sign()
        self.assert_refused(r, "not the ones that were signed")
        self.assertEqual(self.calls("api", "-X", "PATCH"), [])

    def test_refuses_publish_when_the_release_changed(self):
        self.extra_state["new_id_on_upload"] = 43
        r = self.sign()
        self.assert_refused(r, "release 43, not 42")
        self.assertEqual(self.calls("api", "-X", "PATCH"), [])
        self.assertEqual(self.calls("workflow"), [])

    def test_no_resume_when_an_asset_changed_on_publish(self):
        self.extra_state["swap_on_publish"] = {"irsync.rb": "swapped"}
        r = self.sign()
        self.assert_refused(r, "irsync.rb", "release-resume.yml has not been started")
        self.assertEqual(len(self.calls("api", "-X", "PATCH")), 1)
        self.assertEqual(self.calls("workflow"), [])

    def test_refuses_publish_when_a_signature_is_missing(self):
        self.extra_state["drop_upload"] = ["irsync.rb.minisig"]
        r = self.sign()
        self.assert_refused(r, "irsync.rb.minisig")
        self.assertEqual(self.calls("api", "-X", "PATCH"), [])
        self.assertEqual(self.calls("workflow"), [])

    def test_wrong_key_refused(self):
        r = self.sign(keys=OTHER_PUB)
        self.assert_refused(r, "does not verify")
        self.assertEqual(self.calls("release", "upload"), [])
        self.assertEqual(self.calls("api", "-X", "PATCH"), [])

    def test_any_key_in_the_keys_file_will_do(self):
        with open(TEST_PUB) as f:
            r = self.sign(keys=OTHER_PUB + f.read())
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_refuses_publish_when_tag_exists(self):
        self.tags = ["v1.2.3"]
        r = self.sign()
        self.assert_refused(r, "v1.2.3", "tag")
        self.assertEqual(self.calls("api", "-X", "PATCH"), [])
        self.assertEqual(self.calls("workflow"), [])

    def test_no_resume_unless_immutable(self):
        self.extra_state["not_immutable"] = True
        r = self.sign()
        self.assert_refused(r, "immutable")
        self.assertEqual(len(self.calls("api", "-X", "PATCH")), 1)
        self.assertEqual(self.calls("workflow"), [])
        self.assertNotIn("irsync.org", r.stdout)
        self.assertEqual(self.calls("run"), [])

    def test_bad_version_fails_before_gh(self):
        for version in ["v1.2.3", "1.2", "1.2.3;id"]:
            with self.subTest(version=version):
                r = self.sign(version=version)
                self.assertNotEqual(r.returncode, 0)
                self.assertEqual(self.state().get("calls", []), [])
                self.assertEqual(self.minisign_calls(), [])

    def test_usage(self):
        for args in [[], ["--bogus", "1.2.3"], ["1.2.3", "1.2.4"], ["--key"],
                     ["--no-publish", "1.2.3"]]:
            with self.subTest(args=args):
                r = subprocess.run([SIGN, *args], env=self.env, capture_output=True, text=True)
                self.assertEqual(r.returncode, 2)


def real_minisign():
    path = shutil.which("minisign")
    if not path:
        return None
    r = subprocess.run([path, "-v"], capture_output=True, text=True)
    m = re.match(r"minisign (\d+)\.(\d+)", r.stdout + r.stderr)
    return path if m and (int(m[1]), int(m[2])) >= (0, 10) else None


class RealMinisignSignTests(SignTests):
    """The same tests, with the real minisign doing the signing and checking.

    They skip without minisign 0.10 or newer, but fail when the CI
    environment variable is set, as on GitHub, so CI can't pass without
    running them.
    """

    REAL_MINISIGN = real_minisign()

    def setUp(self):
        if not self.REAL_MINISIGN:
            if os.environ.get("CI"):
                self.fail("needs minisign 0.10 or newer on PATH (CI is set, so this fails "
                          "instead of skipping)")
            self.skipTest("needs minisign 0.10 or newer")
        super().setUp()



class PinTests(FakeGh):
    SHA = "a" * 40
    REFUSED = "gh: Resource not accessible by integration (HTTP 403)"

    def pin(self, **state):
        self.set_state(**{"releases": {}, "tags": [], **state})
        self.env["GITHUB_SHA"] = self.SHA
        return self.run_script(PIN, "v1.3.0", "main")

    def test_pins_the_tested_commit(self):
        r = self.pin(branches=["main"])
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.state()["refs"], {"pinned/v1.3.0": self.SHA})

    def test_tag_exists_now(self):
        r = self.pin(branches=["main"], tags=["v1.3.0"])
        self.assertEqual(r.returncode, 1)
        self.assertIn("The tag v1.3.0 exists now", r.stderr)
        self.assertIn("release-resume.yml", r.stderr)
        self.assertNotIn("refs", self.state())

    def test_failed_tag_lookup_stops(self):
        r = self.pin(branches=["main"], api_fails=["git/matching-refs/tags/"])
        self.assertEqual(r.returncode, 1)
        self.assertIn("Could not read", r.stderr)
        self.assertNotIn("refs", self.state())

    def test_failed_branch_lookup_stops(self):
        r = self.pin(branches=["main"], api_fails=["git/matching-refs/heads/"])
        self.assertEqual(r.returncode, 1)
        self.assertIn("Could not read", r.stderr)
        self.assertNotIn("refs", self.state())

    def test_leftover_pin_is_replaced(self):
        r = self.pin(branches=["main", "pinned/v1.3.0"])
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("left over from an earlier attempt", r.stdout)
        s = self.state()
        self.assertEqual(s["branches"], ["main", "pinned/v1.3.0"])
        self.assertEqual(s["refs"], {"pinned/v1.3.0": self.SHA})

    def test_branch_changed_a_workflow(self):
        r = self.pin(branches=["main"], create_ref_fails=self.REFUSED,
                     compare_files=["src/a.rs", ".github/workflows/ci.yml",
                                    ".github/workflows/site.yml"])
        self.assertEqual(r.returncode, 1)
        self.assertIn("main has changed workflow files since this gate started on "
                      f"{self.SHA} (.github/workflows/ci.yml .github/workflows/site.yml)",
                      r.stderr)
        self.assertIn("gh workflow run release-gate.yml --ref main -f version=1.3.0", r.stderr)

    def test_refused_without_a_workflow_change(self):
        for files in (["src/a.rs"], []):
            r = self.pin(branches=["main"], create_ref_fails=self.REFUSED, compare_files=files)
            self.assertEqual(r.returncode, 1)
            self.assertNotIn("changed workflow files", r.stderr)
            self.assertIn(f"Could not create pinned/v1.3.0 at {self.SHA}: {self.REFUSED}",
                          r.stderr)

    def test_refused_and_compare_fails(self):
        r = self.pin(branches=["main"], create_ref_fails=self.REFUSED, api_fails=["compare/"])
        self.assertEqual(r.returncode, 1)
        self.assertIn("Could not create pinned/v1.3.0", r.stderr)

    def test_other_error(self):
        r = self.pin(branches=["main"], create_ref_fails="gh: Bad Gateway (HTTP 502)")
        self.assertEqual(r.returncode, 1)
        self.assertIn("Could not create pinned/v1.3.0 at", r.stderr)
        self.assertIn("HTTP 502", r.stderr)
        self.assertIn("gh workflow run release-gate.yml --ref main -f version=1.3.0", r.stderr)
        self.assertFalse([c for c in self.state()["calls"] if "compare" in " ".join(c)])


class NoPipeIntoGrepQTests(unittest.TestCase):
    """A script with pipefail must not pipe into grep -q.

    grep -q stops reading at the first match. Whatever writes into the pipe
    after that fails (SIGPIPE), and pipefail turns the match into a miss: a
    signed file read as missing, an existing tag read as not there. It
    depends on timing, so it fails only now and then. Search a variable with
    <<< instead.
    """

    def test_no_pipe_into_grep_q(self):
        files = subprocess.run(["git", "ls-files", "-z"], cwd=ROOT, capture_output=True,
                               text=True, check=True).stdout.split("\0")
        found = []
        for name in files:
            path = os.path.join(ROOT, name)
            if not os.path.isfile(path) or os.path.islink(path):
                continue
            try:
                with open(path) as f:
                    text = f.read()
            except UnicodeDecodeError:
                continue
            if not (name.endswith(".sh") or text.startswith("#!") and "sh" in text.split("\n", 1)[0]):
                continue
            if "pipefail" not in text:
                continue
            for m in PIPE_INTO_GREP_Q.finditer(text):
                found.append(f"{name}:{text.count(chr(10), 0, m.start()) + 1}: {m.group(0)!r}")
        self.assertEqual(found, [])

    def test_the_check_finds_one(self):
        for text in ["    printf '%s\\n' \"$x\" | grep -qxF -- \"$y\" || bad=1",
                     "if gh api \"$url\" --jq '.[].ref' |\n    grep -qxF -- \"$ref\"; then",
                     "gh api \"$url\" \\\n  | grep -q x"]:
            self.assertRegex(text, PIPE_INTO_GREP_Q)

    def test_the_check_skips_or(self):
        self.assertNotRegex("[ -f x ] || grep -q y z", PIPE_INTO_GREP_Q)

if __name__ == "__main__":
    unittest.main()
