"""Tests for tools/github_settings.py: the desired state and the diff. No network."""
import importlib.util
import os
import unittest

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
