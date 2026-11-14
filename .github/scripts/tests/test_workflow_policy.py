"""Supply chain rules for the GitHub workflows and repo config.

Each test reads the files in .github/ (and dist-workspace.toml) and fails
when a rule from docs/superpowers/specs/2026-11-14-supply-chain-security-design.md
is broken.
"""
import os
import re
import unittest

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


if __name__ == "__main__":
    unittest.main()
