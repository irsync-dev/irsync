"""Tests for tools/github_settings.py: the desired state and the diff. No network."""
import importlib.util
import os
import subprocess
import unittest
from unittest import mock

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
spec = importlib.util.spec_from_file_location(
    "github_settings", os.path.join(ROOT, "tools", "github_settings.py"))
gs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gs)

REPO = "irsync-dev/irsync"


class DiffTests(unittest.TestCase):
    def test_diff_empty_when_equal(self):
        want = gs.desired(1)
        self.assertEqual(gs.diff(want, want), [])

    def test_diff_reports_missing_ruleset(self):
        want = {REPO: {"rulesets": {"main": gs.desired(1)[REPO]["rulesets"]["main"]}}}
        have = {REPO: {"rulesets": {}}}
        lines = gs.diff(want, have)
        self.assertEqual(len(lines), 1, lines)
        self.assertIn(REPO, lines[0])
        self.assertIn("main", lines[0])

    def test_diff_reports_bypass_actor(self):
        tags = gs.desired(1)[REPO]["rulesets"]["Release tags"]
        want = {REPO: {"rulesets": {"Release tags": tags}}}
        have_tags = dict(tags, bypass_actors=[
            {"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always"}])
        have = {REPO: {"rulesets": {"Release tags": have_tags}}}
        lines = gs.diff(want, have)
        self.assertEqual(len(lines), 1, lines)
        self.assertIn("bypass_actors", lines[0])

    def test_diff_reports_extra_rule(self):
        tags = gs.desired(1)[REPO]["rulesets"]["Release tags"]
        want = {REPO: {"rulesets": {"Release tags": tags}}}
        have_tags = dict(tags, rules=dict(tags["rules"], creation={}))
        have = {REPO: {"rulesets": {"Release tags": have_tags}}}
        lines = gs.diff(want, have)
        self.assertEqual(len(lines), 1, lines)
        self.assertIn("rules", lines[0])
        self.assertIn("creation", lines[0])

    def test_diff_ignores_extra_keys(self):
        want = {REPO: {"actions": {"sha_pinning_required": True}}}
        have = {REPO: {"actions": {"sha_pinning_required": True, "enabled": True},
                       "immutable_releases": False}}
        self.assertEqual(gs.diff(want, have), [])

    def test_diff_reports_changed_value(self):
        want = {REPO: {"actions": {"sha_pinning_required": True}}}
        have = {REPO: {"actions": {"sha_pinning_required": False}}}
        self.assertEqual(gs.diff(want, have),
                         [f"{REPO}: actions.sha_pinning_required: want True, have False"])


class DesiredTests(unittest.TestCase):
    def test_desired_main_needs_ci_result(self):
        main = gs.desired(1)[REPO]["rulesets"]["main"]
        self.assertEqual(main["rules"]["required_status_checks"]["contexts"], ["CI result"])
        self.assertEqual(main["rules"]["pull_request"]["required_approving_review_count"], 0)
        self.assertEqual(main["bypass_actors"], [])

    def test_desired_dependency_graph_on(self):
        self.assertIs(gs.desired(1)[REPO]["dependency_graph"], True)

    def test_desired_no_repo_wide_secrets(self):
        # The release callers pass `secrets: inherit`, so a repo-wide secret
        # would reach the packages and Homebrew jobs.
        self.assertEqual(gs.desired(1)[REPO]["repo_wide_secrets"], [])

    def test_diff_reports_repo_wide_secret(self):
        want = {REPO: {"repo_wide_secrets": []}}
        have = {REPO: {"repo_wide_secrets": ["PACKAGES_GPG_PRIVATE_KEY"]}}
        lines = gs.diff(want, have)
        self.assertEqual(len(lines), 1, lines)
        self.assertIn("PACKAGES_GPG_PRIVATE_KEY", lines[0])

    def test_desired_private_vulnerability_reporting_main_only(self):
        want = gs.desired(1)
        self.assertIs(want[REPO]["private_vulnerability_reporting"], True)
        for repo, settings in want.items():
            if repo != REPO:
                self.assertNotIn("private_vulnerability_reporting", settings, repo)

    def test_diff_reports_private_vulnerability_reporting_off(self):
        want = {REPO: {"private_vulnerability_reporting": True}}
        have = {REPO: {"private_vulnerability_reporting": False}}
        self.assertEqual(gs.diff(want, have),
                         [f"{REPO}: private_vulnerability_reporting: want True, have False"])

    def test_fetch_reads_private_vulnerability_reporting(self):
        for answer, expected in [({"enabled": True}, True), ({"enabled": False}, False),
                                 (None, False)]:
            calls = []

            def fake_gh(*args, **kw):
                calls.append(args[0])
                if args[0].endswith("/private-vulnerability-reporting"):
                    return answer
                return {}

            with mock.patch.object(gs, "gh", fake_gh):
                state = gs.fetch(REPO)
            self.assertIs(state["private_vulnerability_reporting"], expected)
            self.assertIn(f"repos/{REPO}/private-vulnerability-reporting", calls)

    def test_apply_turns_on_private_vulnerability_reporting(self):
        calls = []
        with mock.patch.object(gs, "gh", lambda *a, **kw: calls.append(a)):
            gs.apply(REPO, {"private_vulnerability_reporting": True})
        self.assertIn((f"repos/{REPO}/private-vulnerability-reporting", "-X", "PUT"), calls)

    def test_apply_leaves_private_vulnerability_reporting_alone_when_unwanted(self):
        calls = []
        with mock.patch.object(gs, "gh", lambda *a, **kw: calls.append(a)):
            gs.apply(REPO, {})
        self.assertFalse([c for c in calls if "private-vulnerability" in c[0]])

    def test_desired_release_branches_like_main(self):
        rs = gs.desired(1)[REPO]["rulesets"]
        self.assertEqual(rs["Release branches"]["include"], ["refs/heads/release-*"])
        self.assertEqual(rs["Release branches"]["rules"], rs["main"]["rules"])
        self.assertEqual(rs["Release branches"]["bypass_actors"], [])

    def test_desired_pins_cannot_move(self):
        pins = gs.desired(1)[REPO]["rulesets"]["Pinned release commits"]
        self.assertEqual(pins["include"], ["refs/heads/pinned/*"])
        self.assertEqual(set(pins["rules"]), {"update", "non_fast_forward"})

    def test_stray_tags_reported(self):
        self.assertEqual(gs.stray_tag_lines("v1.0.1\n"),
                         ["irsync-dev/irsync: tag v1.0.1: has no GitHub Release (pushed by hand?)"])
        self.assertEqual(gs.stray_tag_lines(""), [])

    def test_stray_tag_script_failure_is_an_error(self):
        # Non-zero exit with empty stdout is a gh failure, not "clean".
        with mock.patch.object(gs.subprocess, "run", return_value=subprocess.CompletedProcess(
                [], 1, stdout="", stderr="gh: boom")):
            lines = gs.stray_tags()
        self.assertEqual(len(lines), 1, lines)
        self.assertIn("boom", lines[0])

    def test_stray_tag_script_finds_tags(self):
        with mock.patch.object(gs.subprocess, "run", return_value=subprocess.CompletedProcess(
                [], 1, stdout="v1.0.1\nv1.0.2\n", stderr="::error::x")):
            self.assertEqual(len(gs.stray_tags()), 2)
        with mock.patch.object(gs.subprocess, "run", return_value=subprocess.CompletedProcess(
                [], 0, stdout="", stderr="")):
            self.assertEqual(gs.stray_tags(), [])

    def test_desired_tags_allow_creation(self):
        tags = gs.desired(1)[REPO]["rulesets"]["Release tags"]
        self.assertNotIn("creation", tags["rules"])
        self.assertEqual(set(tags["rules"]), {"update", "deletion", "non_fast_forward"})
        self.assertEqual(tags["bypass_actors"], [])

    def test_desired_release_env_tag_only(self):
        env = gs.desired(42)[REPO]["environments"]["release"]
        self.assertEqual(env["policies"], [{"name": "v*", "type": "tag"}])
        self.assertEqual(env["reviewers"], [42])
        self.assertIs(env["prevent_self_review"], False)

    def test_desired_site_env_main_only(self):
        env = gs.desired(42)[REPO]["environments"]["site"]
        self.assertEqual(env["policies"], [{"name": "main", "type": "branch"}])
        self.assertEqual(env["reviewers"], [])

    def test_desired_other_repos(self):
        want = gs.desired(1)
        iroh = want["irsync-dev/iroh"]["rulesets"]["Pinned branches"]
        self.assertEqual(iroh["include"], ["refs/heads/irsync-*"])
        for repo in ["irsync-dev/homebrew-tap", "irsync-dev/packages"]:
            rules = want[repo]["rulesets"]["Default branch"]["rules"]
            self.assertEqual(set(rules), {"deletion", "non_fast_forward"}, repo)


class RulesetPayloadTests(unittest.TestCase):
    def test_roundtrip(self):
        """A ruleset written by apply() reads back to the same state."""
        main = gs.desired(1)[REPO]["rulesets"]["main"]
        api = gs.ruleset_payload("main", main)
        self.assertEqual(api["enforcement"], "active")
        self.assertEqual(gs.normalize_ruleset(api), main)


if __name__ == "__main__":
    unittest.main()
