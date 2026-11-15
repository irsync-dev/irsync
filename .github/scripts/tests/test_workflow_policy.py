"""Supply chain rules for the GitHub workflows and repo config.

Each test reads the files in .github/ (and dist-workspace.toml) and fails
when a rule from docs/superpowers/specs/2026-11-14-supply-chain-security-design.md
is broken.
"""
import os
import re
import unittest

import tomllib

import yaml

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(HERE)))
WORKFLOWS = os.path.join(ROOT, ".github", "workflows")

# A pinned third-party Action: owner/repo[/path]@<40 hex> # v<version>.
PIN = re.compile(r"^\s*(- )?uses: [\w.-]+/[\w./-]+@[0-9a-f]{40} # v\d+(\.\d+)*$")
USES = re.compile(r"^\s*(- )?uses:\s*(\S+)")

# release.yml is written by dist, which pins from [dist.github-action-commits]
# in dist-workspace.toml but cannot add the version comment. The version is
# kept next to the SHA there instead.
DIST_PIN = re.compile(r"^\s*(- )?uses: [\w.-]+/[\w./-]+@[0-9a-f]{40}$")

# Actions dist 0.34.0 writes into release.yml that its config cannot pin.
# Empty unless dist turns out unable to pin one; then name the limitation.
DIST_UNPINNABLE = set()


def workflow_files():
    return sorted(f for f in os.listdir(WORKFLOWS) if f.endswith((".yml", ".yaml")))


def read(name):
    with open(os.path.join(WORKFLOWS, name)) as f:
        return f.read()


def load_workflows():
    """File name -> parsed workflow."""
    return {name: yaml.safe_load(read(name)) for name in workflow_files()}


def triggers(wf):
    # PyYAML reads the bare key `on` as the boolean True.
    on = wf.get("on", wf.get(True))
    if isinstance(on, str):
        return {on: None}
    if isinstance(on, list):
        return {k: None for k in on}
    return on or {}


def jobs(wf):
    return wf.get("jobs") or {}


def raw_uses_lines():
    """(file, line number, text) for every `uses:` line."""
    out = []
    for name in workflow_files():
        for i, line in enumerate(read(name).splitlines(), 1):
            if USES.match(line):
                out.append((name, i, line.rstrip()))
    return out


def job_text(name, job_id):
    """The YAML text of one job: from its key under `jobs:` to the next job."""
    lines = read(name).splitlines()
    start = None
    for i, line in enumerate(lines):
        if start is None:
            if re.match(r"^  " + re.escape(job_id) + r":\s*$", line):
                start = i
        elif re.match(r"^  [\w-]+:\s*$", line) or re.match(r"^\S", line):
            return "\n".join(lines[start:i])
    if start is None:
        raise KeyError(f"{name}: no job {job_id}")
    return "\n".join(lines[start:])


class PinTests(unittest.TestCase):
    def test_third_party_actions_pinned(self):
        bad = []
        for name, i, line in raw_uses_lines():
            target = USES.match(line).group(2)
            if target.startswith("./"):
                continue
            if name == "release.yml" and target.split("@")[0] in DIST_UNPINNABLE:
                continue
            pattern = DIST_PIN if name == "release.yml" else PIN
            if not pattern.match(line):
                bad.append(f"{name}:{i}: {line.strip()}")
        self.assertEqual(bad, [], "Actions not pinned to a commit SHA with a version comment")

    def test_pin_format_accepts_dependabot_style(self):
        sha = "08eba0b27e820071cde6df949e0beb9ba4906955"
        self.assertTrue(PIN.match(f"      - uses: actions/checkout@{sha} # v6.0.1"))
        self.assertTrue(PIN.match(f"        uses: actions/checkout@{sha} # v6"))
        self.assertFalse(PIN.match("      - uses: actions/checkout@v6"))
        self.assertFalse(PIN.match(f"      - uses: actions/checkout@{sha[:39]} # v6"))
        self.assertFalse(PIN.match(f"      - uses: actions/checkout@{sha}"))

    def test_no_pull_request_target(self):
        for name, wf in load_workflows().items():
            self.assertNotIn("pull_request_target", triggers(wf), name)


class DependabotTests(unittest.TestCase):
    def test_dependabot_config(self):
        path = os.path.join(ROOT, ".github", "dependabot.yml")
        self.assertTrue(os.path.exists(path), ".github/dependabot.yml is missing")
        with open(path) as f:
            cfg = yaml.safe_load(f)
        self.assertEqual(cfg["version"], 2)
        by_eco = {u["package-ecosystem"]: u for u in cfg["updates"]}
        self.assertEqual(set(by_eco), {"cargo", "github-actions"})
        for eco, u in by_eco.items():
            self.assertEqual(u["schedule"]["interval"], "weekly", eco)
            self.assertEqual(u["cooldown"]["default-days"], 7, eco)
            groups = list(u["groups"].values())
            self.assertEqual(len(groups), 1, eco)
            self.assertEqual(groups[0]["patterns"], ["*"], eco)


class CiWorkflowTests(unittest.TestCase):
    def setUp(self):
        self.ci = load_workflows()["ci.yml"]

    def test_ci_result_needs_every_job(self):
        j = jobs(self.ci)
        self.assertIn("ci-result", j)
        res = j["ci-result"]
        self.assertEqual(res["name"], "CI result")
        self.assertEqual(res["if"], "always()")
        self.assertEqual(set(res["needs"]), set(j) - {"ci-result"})

    def test_pull_request_has_no_paths_ignore(self):
        on = triggers(self.ci)
        self.assertNotIn("paths-ignore", on["pull_request"] or {})
        self.assertIn("paths-ignore", on["push"])

    def test_deny_job_present(self):
        runs = [s.get("run", "") for j in jobs(self.ci).values() for s in j.get("steps", [])]
        self.assertTrue(any("cargo deny --locked check" == r.strip() for r in runs), runs)

    def test_dependency_review_on_prs_only(self):
        found = False
        for j in jobs(self.ci).values():
            for s in j.get("steps", []):
                if s.get("uses", "").startswith("actions/dependency-review-action@"):
                    found = True
                    self.assertEqual(j.get("if"), "github.event_name == 'pull_request'")
                    self.assertEqual(s["with"]["fail-on-severity"], "low")
        self.assertTrue(found, "no dependency-review job")

    def test_advisories_daily(self):
        wf = load_workflows()["advisories.yml"]
        on = triggers(wf)
        self.assertTrue(on["schedule"][0]["cron"])
        self.assertIn("workflow_dispatch", on)
        self.assertEqual(wf["permissions"], {"contents": "read"})
        runs = [s.get("run", "") for j in jobs(wf).values() for s in j.get("steps", [])]
        self.assertIn("cargo deny --locked check advisories", [r.strip() for r in runs])


def dist_config():
    with open(os.path.join(ROOT, "dist-workspace.toml"), "rb") as f:
        return tomllib.load(f)


def step_uses(step, action):
    return step.get("uses", "").split("@")[0] == action


class AttestationTests(unittest.TestCase):
    ATTEST = "actions/attest"
    WRITE = {"id-token": "write", "attestations": "write"}

    def test_dist_attestations_on(self):
        self.assertIs(dist_config()["dist"].get("github-attestations"), True)

    def test_release_yml_attests(self):
        wf = load_workflows()["release.yml"]
        attesting = [j for j in jobs(wf).values()
                     if any(step_uses(s, self.ATTEST) for s in j.get("steps", []))]
        self.assertTrue(attesting, "release.yml attests nothing")
        for j in attesting:
            perms = {**(wf.get("permissions") or {}), **(j.get("permissions") or {})}
            for k, v in self.WRITE.items():
                self.assertEqual(perms.get(k), v)

    def test_packages_attested_not_uploaded(self):
        self.assertNotIn("gh release upload", read("packages.yml"))
        job = jobs(load_workflows()["packages.yml"])["packages"]
        steps = [s for s in job["steps"] if step_uses(s, self.ATTEST)]
        self.assertEqual(len(steps), 1)
        self.assertIn("runner.temp }}/packages/", steps[0]["with"]["subject-path"])
        self.assertEqual(steps[0]["if"], "${{ !inputs.dry-run }}")
        for k, v in self.WRITE.items():
            self.assertEqual(job["permissions"].get(k), v)

    def test_package_callers_grant_attest_permissions(self):
        for name in ["release-gate.yml", "release-resume.yml"]:
            perms = jobs(load_workflows()[name])["packages"]["permissions"]
            self.assertEqual(perms, {"contents": "read", **self.WRITE}, name)


RELEASE_SECRET = re.compile(r"secrets\.(PACKAGES_\w+|HOMEBREW_TAP_TOKEN)")


def env_name(job):
    e = job.get("environment")
    return e.get("name") if isinstance(e, dict) else e


class EnvironmentTests(unittest.TestCase):
    def test_release_secrets_only_in_release_env(self):
        for name, wf in load_workflows().items():
            for job_id, job in jobs(wf).items():
                if "uses" in job:
                    continue  # a call: checked by test_callers_pass_no_release_secrets
                if RELEASE_SECRET.search(job_text(name, job_id)):
                    self.assertEqual(env_name(job), "release", f"{name}: {job_id}")

    def test_callers_pass_no_release_secrets(self):
        for name, wf in load_workflows().items():
            for job_id, job in jobs(wf).items():
                if job.get("uses", "").startswith("./"):
                    secrets = job.get("secrets")
                    if isinstance(secrets, dict):
                        self.assertFalse(
                            [k for k in secrets if RELEASE_SECRET.search(f"secrets.{k}")],
                            f"{name}: {job_id} passes release secrets")
        call = triggers(load_workflows()["packages.yml"])["workflow_call"]
        self.assertNotIn("secrets", call)

    def test_cloudflare_pub_in_site_env(self):
        wf = load_workflows()["site.yml"]
        found = False
        for job_id, job in jobs(wf).items():
            if "secrets.CLOUDFLARE_API_TOKEN_PUB" in job_text("site.yml", job_id):
                found = True
                self.assertEqual(env_name(job), "site", job_id)
        self.assertTrue(found)

    def test_dist_homebrew_replaced(self):
        # dist 0.34.0 writes `uses: $/.github/...` for a local custom publish
        # job, so the release gate publishes the formula instead (see
        # publish-homebrew.yml). release.yml then needs no secrets at all.
        self.assertEqual(dist_config()["dist"]["publish-jobs"], [])
        self.assertNotIn("publish-homebrew-formula", jobs(load_workflows()["release.yml"]))
        self.assertIsNone(RELEASE_SECRET.search(read("release.yml")))

    def homebrew_caller(self, name):
        callers = [j for j in jobs(load_workflows()[name]).values()
                   if j.get("uses") == "./.github/workflows/publish-homebrew.yml"]
        self.assertEqual(len(callers), 1, name)
        return callers[0]

    def test_homebrew_job_skips_prereleases(self):
        gate = self.homebrew_caller("release-gate.yml")
        self.assertIn("needs.check.outputs.prerelease == 'false'", gate["if"])
        self.assertIn("needs.release.result == 'success'", gate["if"])
        resume = self.homebrew_caller("release-resume.yml")
        self.assertEqual(resume["if"], "needs.check.outputs.homebrew == 'true'")

    def test_homebrew_job_in_release_env(self):
        own = jobs(load_workflows()["publish-homebrew.yml"])["homebrew"]
        self.assertEqual(env_name(own), "release")

    def test_resume_has_no_main_guard(self):
        text = read("release-resume.yml")
        self.assertNotIn("MAIN_REF", text)
        self.assertNotIn("refs/heads/", text)


if __name__ == "__main__":
    unittest.main()
