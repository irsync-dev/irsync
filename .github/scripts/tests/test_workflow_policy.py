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

    def ci_script_runs(self, job, command):
        """ci.yml runs `tools/ci.sh JOB`, and that job runs `command`."""
        runs = [s.get("run", "").strip() for j in jobs(self.ci).values()
                for s in j.get("steps", [])]
        self.assertIn(f"tools/ci.sh {job}", runs)
        with open(os.path.join(ROOT, "tools", "ci.sh")) as f:
            script = f.read()
        fn = "job_" + job.replace("-", "_")
        body = re.search(rf"^{fn}\(\) \{{\n(.*?)^\}}", script, re.M | re.S)
        self.assertIsNotNone(body, f"no {fn} in tools/ci.sh")
        self.assertRegex(body.group(1), rf"(?m)^ +run {re.escape(command)}$")

    def test_workflow_checks_in_ci(self):
        # The rules in this file and actionlint must run on every pull request
        # that touches a workflow, inside a job "CI result" needs. The
        # commands are in tools/ci.sh, which ci.yml runs.
        self.ci_script_runs("workflows", "python3 -m unittest discover -s .github/scripts/tests")
        self.ci_script_runs("workflows", "actionlint -shellcheck= -pyflakes=")
        self.ci_script_runs("release-build", "dist generate --check")
        # dist's side of our installer, which build-global-artifacts runs.
        self.ci_script_runs("release-build", "dist build --artifacts=global || { clean_up; exit 1; }")

    def test_deny_job_present(self):
        self.ci_script_runs("deny", "cargo deny --locked check")

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

    def test_every_release_file_attested(self):
        # By default dist attests only the per-target archives; our installer,
        # the Homebrew formula and the checksum files go unattested. In the
        # host phase it attests everything it is about to publish.
        self.assertEqual(dist_config()["dist"].get("github-attestations-phase"), "host")
        host = jobs(load_workflows()["release.yml"])["host"]
        steps = [s for s in host["steps"] if step_uses(s, self.ATTEST)]
        self.assertEqual(len(steps), 1)
        self.assertEqual(steps[0]["with"]["subject-path"].strip(), "artifacts/*")

    def test_installer_is_ours(self):
        # Our installer (packaging/installer) replaces dist's shell installer,
        # so it can install the man pages too.
        dist = dist_config()["dist"]
        self.assertEqual(dist["installers"], ["homebrew"])
        extra = dist["extra-artifacts"]
        self.assertEqual(len(extra), 1)
        self.assertEqual(extra[0]["artifacts"], ["irsync-installer.sh"])
        self.assertEqual(extra[0]["build"][0], "packaging/installer/build-installer.sh")

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
        perms = jobs(load_workflows()["release-resume.yml"])["packages"]["permissions"]
        self.assertEqual(perms, {"contents": "read", **self.WRITE})


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

    def test_release_env_callers_inherit_secrets(self):
        # A called workflow's job sees its environment's secrets only when the
        # caller passes `secrets: inherit`: without it they arrive empty (seen
        # on the v0.1.0-beta.2 resume dry run).
        for job_id, job in jobs(load_workflows()["release-resume.yml"]).items():
            target = job.get("uses", "")
            if target in ("./.github/workflows/packages.yml",
                          "./.github/workflows/publish-homebrew.yml"):
                self.assertEqual(job.get("secrets"), "inherit", job_id)

    def test_gate_has_no_release_env_jobs(self):
        # The gate hands packages and Homebrew to the resume run it starts.
        for job_id, job in jobs(load_workflows()["release-gate.yml"]).items():
            self.assertNotIn(job.get("uses"), ("./.github/workflows/packages.yml",
                                               "./.github/workflows/publish-homebrew.yml"),
                             job_id)

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

    def homebrew_caller(self):
        callers = [j for j in jobs(load_workflows()["release-resume.yml"]).values()
                   if j.get("uses") == "./.github/workflows/publish-homebrew.yml"]
        self.assertEqual(len(callers), 1)
        return callers[0]

    def test_homebrew_job_skips_prereleases(self):
        resume = self.homebrew_caller()
        self.assertEqual(resume["if"], "needs.check.outputs.homebrew == 'true'")

    def test_homebrew_job_in_release_env(self):
        own = jobs(load_workflows()["publish-homebrew.yml"])["homebrew"]
        self.assertEqual(env_name(own), "release")

    def test_formula_script_runs_before_the_tap_token(self):
        # formula-man-pages.sh is repo code. It runs before the tap is checked
        # out with HOMEBREW_TAP_TOKEN, so the token is never on disk while it
        # runs, and the irsync checkout keeps no credentials either.
        steps = jobs(load_workflows()["publish-homebrew.yml"])["homebrew"]["steps"]
        script = [i for i, s in enumerate(steps)
                  if "formula-man-pages.sh" in s.get("run", "")]
        token = [i for i, s in enumerate(steps)
                 if step_uses(s, "actions/checkout")
                 and "secrets.HOMEBREW_TAP_TOKEN" in str((s.get("with") or {}).get("token", ""))]
        self.assertEqual(len(script), 1)
        self.assertEqual(len(token), 1)
        self.assertLess(script[0], token[0])
        irsync = [s for s in steps[:script[0]] if step_uses(s, "actions/checkout")
                  and (s.get("with") or {}).get("path") == "irsync"]
        self.assertEqual(len(irsync), 1)
        self.assertIs(irsync[0]["with"].get("persist-credentials"), False)

    def test_resume_has_no_main_guard(self):
        text = read("release-resume.yml")
        self.assertNotIn("MAIN_REF", text)
        self.assertNotIn("refs/heads/", text)


class FreeBsdTests(unittest.TestCase):
    """freebsd.yml: the FreeBSD test run and build, started by hand."""

    def wf(self):
        return load_workflows()["freebsd.yml"]

    def steps(self):
        return jobs(self.wf())["freebsd"]["steps"]

    def step_using(self, action):
        (s,) = [s for s in self.steps() if s.get("uses", "").startswith(action + "@")]
        return s

    def test_starts_only_by_hand(self):
        on = triggers(self.wf())
        self.assertEqual(set(on), {"workflow_dispatch"})
        self.assertEqual(set(on["workflow_dispatch"]["inputs"]), {"ref", "freebsd"})
        self.assertEqual(on["workflow_dispatch"]["inputs"]["freebsd"]["default"], "14")

    def test_no_workflow_calls_it(self):
        # It must never become part of `CI result` or of a release.
        for name in workflow_files():
            if name != "freebsd.yml":
                self.assertNotIn("freebsd.yml", read(name), name)

    def test_read_only_and_time_limited(self):
        self.assertEqual(self.wf()["permissions"], {"contents": "read"})
        job = jobs(self.wf())["freebsd"]
        self.assertNotIn("permissions", job)
        self.assertEqual(job["timeout-minutes"], 120)

    def test_ref_defaults_to_the_started_branch(self):
        with_ = self.step_using("actions/checkout")["with"]
        self.assertEqual(with_["ref"], "${{ inputs.ref || github.ref }}")
        self.assertIs(with_["persist-credentials"], False)

    def test_old_versions_stop_with_a_message(self):
        # A version from before this workflow has no script to run.
        steps = self.steps()
        vm = steps.index(self.step_using("vmactions/freebsd-vm"))
        guards = [i for i, s in enumerate(steps)
                  if "This version has no FreeBSD build" in s.get("run", "")]
        self.assertEqual(len(guards), 1)
        self.assertLess(guards[0], vm)

    def test_vm_runs_the_script(self):
        with_ = self.step_using("vmactions/freebsd-vm")["with"]
        self.assertEqual(with_["run"].strip(), ".github/scripts/freebsd-vm.sh")
        self.assertEqual(with_["release"], "${{ inputs.freebsd }}")
        self.assertEqual(with_["arch"], "x86_64")

    def test_upload_only_after_success(self):
        up = self.step_using("actions/upload-artifact")
        self.assertNotIn("if", up)
        self.assertEqual(self.steps()[-1], up)
        self.assertEqual(up["with"]["if-no-files-found"], "error")
        self.assertEqual(up["with"]["path"].rstrip("/"), "freebsd-out")

    def test_inputs_never_expand_in_scripts(self):
        # An input pasted into a script could run as shell.
        for s in self.steps():
            self.assertNotIn("${{", s.get("run", ""))

    def test_script_runs_the_ci_jobs_with_cis_nextest(self):
        with open(os.path.join(ROOT, ".github", "scripts", "freebsd-vm.sh")) as f:
            script = f.read()
        self.assertRegex(script, r"(?m)^\s*tools/ci\.sh check freebsd-build$")
        ci = re.search(r"cargo-nextest@([\d.]+)", read("ci.yml")).group(1)
        self.assertRegex(script, rf"(?m)^NEXTEST_VERSION={re.escape(ci)}$")

    def test_checks_run_strict_in_the_vm(self):
        # `su -l` drops the environment. Without CI set, tests that must not
        # skip on CI (no usable rsync, say) would skip and the run go green.
        with open(os.path.join(ROOT, ".github", "scripts", "freebsd-vm.sh")) as f:
            script = f.read()
        self.assertRegex(
            script,
            r"su -l .*env CI=true RUST_BACKTRACE=1 CARGO_TERM_COLOR=always "
            r"\.github/scripts/freebsd-vm\.sh")

    def test_script_does_not_lean_on_rustup_extras(self):
        with open(os.path.join(ROOT, ".github", "scripts", "freebsd-vm.sh")) as f:
            script = f.read()
        # rustup may not write ~/.cargo/env when told to leave PATH alone.
        self.assertNotIn(".cargo/env", script)
        self.assertRegex(script, r'(?m)^PATH="\$HOME/\.cargo/bin:\$PATH"$')
        # `rustup toolchain install` with no name needs rustup 1.28, so the
        # script names the version from rust-toolchain.toml itself.
        self.assertNotRegex(script, r"(?m)^rustup toolchain install$")
        self.assertRegex(
            script,
            r'(?m)^rustup toolchain install "\$channel" --profile minimal '
            r"--component rustfmt --component clippy$")


class GateTests(unittest.TestCase):
    def gate(self):
        return load_workflows()["release-gate.yml"]

    def test_gate_starts_only_by_hand_with_a_version(self):
        on = triggers(self.gate())
        self.assertEqual(set(on), {"workflow_dispatch"})
        self.assertEqual(set(on["workflow_dispatch"]["inputs"]), {"version"})

    def test_no_workflow_runs_on_a_tag_push(self):
        for name, wf in load_workflows().items():
            push = triggers(wf).get("push") or {}
            self.assertNotIn("tags", push, name)

    def test_gate_pins_the_tested_commit(self):
        text = job_text("release-gate.yml", "release")
        self.assertIn('ref="refs/heads/pinned/$TAG" -f sha="$GITHUB_SHA"', text)
        self.assertIn('--ref "refs/heads/pinned/$TAG"', text)
        self.assertIn('"$run_sha" != "$GITHUB_SHA"', text)

    def test_gate_rechecks_the_tag_before_pinning(self):
        text = job_text("release-gate.yml", "release")
        self.assertIn("git/matching-refs/tags/$TAG", text)
        self.assertIn('grep -qxF -- "refs/tags/$TAG"', text)
        self.assertIn("release-resume.yml", text)
        self.assertLess(
            text.index("git/matching-refs/tags/$TAG"),
            text.index('ref="refs/heads/pinned/$TAG"'),
        )

    def test_gate_clears_stale_pin(self):
        steps = jobs(self.gate())["release"]["steps"]
        last = steps[-1]
        self.assertEqual(last.get("if"), "always()")
        self.assertIn("pinned/$TAG", last["run"])

    def test_gate_refuses_while_old_release_run_active(self):
        text = job_text("release-gate.yml", "release")
        self.assertIn("in_progress", text)
        self.assertIn("queued", text)

    def test_gate_makes_release_branch_after_release(self):
        job = jobs(self.gate())["branch"]
        self.assertIn("release", job["needs"])
        self.assertIn("new-branch", job["if"])

    def test_gate_finishes_on_the_tag(self):
        self.assertIn('release-resume.yml --ref "refs/tags/$TAG"',
                      job_text("release-gate.yml", "finish"))

    def test_gate_and_resume_share_concurrency(self):
        self.assertEqual(self.gate()["concurrency"]["group"], "release-v${{ inputs.version }}")
        self.assertEqual(load_workflows()["release-resume.yml"]["concurrency"]["group"],
                         "release-${{ inputs.tag }}")

    def test_ci_runs_on_release_branches(self):
        push = triggers(load_workflows()["ci.yml"])["push"]
        self.assertEqual(push["branches"], ["main", "release-*"])


if __name__ == "__main__":
    unittest.main()
