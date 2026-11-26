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

# Jobs in ci.yml that report on a pull request but that "CI result" does not
# wait for.
NON_BLOCKING = {"windows"}

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
    def test_top_level_permissions_read_only(self):
        for name, wf in load_workflows().items():
            if name == "release.yml":  # written by dist
                continue
            perms = wf.get("permissions")
            self.assertIsInstance(perms, dict, f"{name}: no top-level permissions block")
            self.assertNotIn("write", perms.values(), name)

    def test_no_unpinned_pip_install(self):
        for name in workflow_files():
            self.assertNotRegex(read(name), r"pip3? install(?!.*--require-hashes)", name)

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
        # Cargo twice: the workspace, and the fuzz targets with their own
        # Cargo.lock.
        by_dir = {(u["package-ecosystem"], u["directory"]): u for u in cfg["updates"]}
        self.assertEqual(len(by_dir), len(cfg["updates"]), "an ecosystem and directory twice")
        self.assertEqual(set(by_dir), {("cargo", "/"), ("cargo", "/fuzz"), ("github-actions", "/")})
        for key, u in by_dir.items():
            self.assertEqual(u["schedule"]["interval"], "weekly", key)
            self.assertEqual(u["cooldown"]["default-days"], 7, key)
            groups = list(u["groups"].values())
            self.assertEqual(len(groups), 1, key)
            self.assertEqual(groups[0]["patterns"], ["*"], key)


class CiWorkflowTests(unittest.TestCase):
    def setUp(self):
        self.ci = load_workflows()["ci.yml"]

    def test_ci_result_needs_every_job(self):
        j = jobs(self.ci)
        self.assertIn("ci-result", j)
        res = j["ci-result"]
        self.assertEqual(res["name"], "CI result")
        self.assertEqual(res["if"], "always()")
        self.assertEqual(set(res["needs"]), set(j) - {"ci-result"} - NON_BLOCKING)

    def test_windows_job_reports_but_does_not_block(self):
        # filegen, fileset and irsync-glob on Windows, x86_64 and ARM64. It
        # shows on every pull request that changes code, but a failure does
        # not stop a merge: "CI result" does not need it.
        j = jobs(self.ci)
        self.assertIn("windows", j)
        win = j["windows"]
        self.assertEqual(win["strategy"]["matrix"]["os"], ["windows-latest", "windows-11-arm"])
        self.assertIs(win["strategy"]["fail-fast"], False)
        self.assertEqual(win["runs-on"], "${{ matrix.os }}")
        self.assertEqual(win["defaults"]["run"]["shell"], "bash")
        self.assertEqual(win["env"]["CARGO_TARGET_DIR"], "C:\\t")
        self.assertEqual(win["needs"], "changes")
        # Left out of a release run, like release-build, so it never holds
        # up a release.
        self.assertEqual(win["if"], j["release-build"]["if"])
        self.assertEqual(win["if"],
                         "${{ !inputs.plan && needs.changes.outputs.code == 'true' }}")
        runs = [s.get("run", "").strip() for s in win["steps"]]
        self.assertIn("tools/ci.sh check", runs)
        nextest = [s for s in win["steps"] if step_uses(s, "taiki-e/install-action")]
        self.assertEqual(len(nextest), 1)
        self.assertEqual(nextest[0]["with"]["fallback"], "none")
        self.assertNotIn("windows", j["ci-result"]["needs"])

    def test_pull_request_has_no_paths_ignore(self):
        on = triggers(self.ci)
        self.assertNotIn("paths-ignore", on["pull_request"] or {})
        self.assertIn("paths-ignore", on["push"])

    def test_every_pull_request_checks_the_release_notes(self):
        # A Markdown-only pull request skips the builds, and with them the
        # test that reads CHANGELOG.md. The `changes` job always runs.
        steps = [s for s in jobs(self.ci)["changes"]["steps"]
                 if ".github/scripts/changelog-section.sh --current" in s.get("run", "")]
        self.assertEqual(len(steps), 1)
        self.assertEqual(steps[0]["if"], "github.event_name == 'pull_request'")

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
        self.ci_script_runs("release-build", ".github/scripts/release-yml.sh check")
        # dist's side of our installer, which build-global-artifacts runs.
        self.ci_script_runs("release-build", "dist build --artifacts=global || { clean_up; exit 1; }")

    def test_zizmor_in_ci(self):
        # zizmor, a linter for the security of GitHub workflows, runs next
        # to actionlint, at a pinned version. tools/dev-setup.sh reads the
        # pin from here.
        self.ci_script_runs("workflows", "zizmor --offline .github/workflows")
        steps = jobs(self.ci)["workflows"]["steps"]
        install = [s for s in steps if step_uses(s, "taiki-e/install-action")]
        self.assertEqual(len(install), 1)
        self.assertRegex(install[0]["with"]["tool"], r"^zizmor@\d+\.\d+\.\d+$")
        # Never cargo-binstall, which install-action would hand the GitHub
        # token, nor a build from source.
        self.assertEqual(install[0]["with"]["fallback"], "none")

    def test_zizmor_ignores_have_reasons(self):
        # A finding is ignored only in .github/zizmor.yml, each with a
        # reason on its line.
        path = os.path.join(ROOT, ".github", "zizmor.yml")
        with open(path) as f:
            text = f.read()
        config = yaml.safe_load(text)
        entries = 0
        for rule, conf in config["rules"].items():
            self.assertEqual(set(conf), {"ignore"}, rule)
            entries += len(conf["ignore"])
        self.assertGreater(entries, 0)
        for line in text.splitlines():
            if re.match(r"^\s*- ", line):
                self.assertRegex(line, r"^\s*- \S+ +# \S", line)

    def test_shellcheck_in_ci_on_every_change(self):
        # site/install.sh is a shell script, and a site-only pull request
        # skips the jobs that wait for `changes`; this one must not.
        self.ci_script_runs("shell-scripts", 'shellcheck "${files[@]}"')
        job = jobs(self.ci)["shellcheck"]
        self.assertNotIn("needs", job)
        self.assertNotIn("if", job)

    def test_jobs_running_the_script_tests_install_minisign(self):
        # The sign-release.sh tests with the real minisign fail without it
        # when CI is set, as on GitHub. Every job that runs
        # .github/scripts/tests must install it first: itself with apt, or
        # through install-packaging-tools.sh.
        with open(os.path.join(ROOT, ".github", "scripts", "install-packaging-tools.sh")) as f:
            packaging_tools = f.read()
        self.assertRegex(packaging_tools, r"apt-get install [^\n]*\bminisign\b")

        def installs(run):
            return (re.search(r"apt-get install [^\n]*\bminisign\b", run) is not None
                    or "install-packaging-tools.sh" in run)

        found = []
        for name, wf in load_workflows().items():
            for job_id, job in jobs(wf).items():
                runs = [st.get("run", "") for st in job.get("steps") or []]
                for i, run in enumerate(runs):
                    if ".github/scripts/tests" in run or "tools/ci.sh workflows" in run:
                        found.append((name, job_id))
                        self.assertTrue(any(installs(r) for r in runs[:i]),
                                        f"{name} job {job_id} runs the script tests "
                                        "without installing minisign first")
        self.assertIn(("ci.yml", "workflows"), found)
        self.assertIn(("release-tooling.yml", "scripts"), found)

    def test_deny_job_present(self):
        self.ci_script_runs(
            "deny",
            "cargo deny --locked check -D advisory-not-detected -D unmatched-skip -D unnecessary-skip")

    def test_stale_advisory_exception_fails(self):
        with open(os.path.join(ROOT, "tools", "ci.sh")) as f:
            self.assertIn("-D advisory-not-detected", f.read())

    def test_duplicate_crate_versions_fail(self):
        # A second version of a crate fails unless deny.toml lists it under
        # skip, and a skip entry that no longer matches fails too.
        with open(os.path.join(ROOT, "deny.toml"), "rb") as f:
            bans = tomllib.load(f)["bans"]
        self.assertEqual(bans["multiple-versions"], "deny")
        for entry in bans.get("skip", []):
            self.assertTrue(entry.get("reason", "").strip(),
                            f"skip entry {entry.get('crate')} has no reason")
        with open(os.path.join(ROOT, "tools", "ci.sh")) as f:
            script = f.read()
        self.assertIn("-D unmatched-skip", script)
        self.assertIn("-D unnecessary-skip", script)

    def test_deny_license_check_blocks_only_a_release(self):
        self.ci_script_runs(
            "deny-license-check",
            "cargo deny --locked --config deny-license-check.toml check licenses")
        job = jobs(self.ci)["deny-license-check"]
        # A failed job with continue-on-error counts as passed for "CI
        # result"; a release run (inputs.plan is set) turns that off.
        self.assertEqual(job["continue-on-error"], "${{ !inputs.plan }}")

    def test_deny_configs_agree(self):
        def load(name):
            with open(os.path.join(ROOT, name), "rb") as f:
                return tomllib.load(f)
        narrow, wide = load("deny.toml"), load("deny-license-check.toml")
        # The same licenses are allowed whichever platforms are looked at.
        self.assertEqual(narrow["licenses"], wide["licenses"])
        self.assertTrue(narrow["graph"]["all-features"])
        self.assertEqual(wide["graph"], {"all-features": True})
        # Every release target is one of the platforms deny.toml checks.
        released = set(dist_config()["dist"]["targets"])
        self.assertLessEqual(released, set(narrow["graph"]["targets"]))

    def test_nextest_pins_agree(self):
        # ci.yml pins cargo-nextest in more than one job; tools/dev-setup.sh
        # reads only the first.
        versions = re.findall(r"cargo-nextest@([0-9.]+)", read("ci.yml"))
        self.assertGreaterEqual(len(versions), 2)
        self.assertEqual(len(set(versions)), 1, versions)

    def test_deny_covers_windows(self):
        # filegen and fileset are built for Windows, so their dependencies
        # there are checked like those of the other platforms.
        with open(os.path.join(ROOT, "deny.toml"), "rb") as f:
            targets = tomllib.load(f)["graph"]["targets"]
        self.assertIn("x86_64-pc-windows-msvc", targets)
        self.assertIn("aarch64-pc-windows-msvc", targets)

    def test_deny_accepts_chrono_for_windows_only(self):
        # chrono is unmaintained and comes in only on Windows, through
        # netwatch (iroh). It is the one advisory accepted, and the reason
        # points at the issue that tracks it.
        with open(os.path.join(ROOT, "deny.toml"), "rb") as f:
            ignore = tomllib.load(f)["advisories"]["ignore"]
        self.assertEqual([e["id"] for e in ignore], ["RUSTSEC-2026-0543"])
        self.assertIn("#174", ignore[0]["reason"])

    def test_fuzz_build_job(self):
        # Every fuzz target must build and pass its seeds on a pull request
        # that touches code. The nightly comes from fuzz/rust-toolchain.toml.
        self.ci_script_runs("fuzz-build", 'cargo "+$nightly" fuzz build --target "$triple"')
        job = jobs(self.ci)["fuzz-build"]
        self.assertEqual(job["needs"], "changes")
        self.assertEqual(job["if"], "needs.changes.outputs.code == 'true'")
        self.assertEqual(job["permissions"], {"contents": "read"})
        text = job_text("ci.yml", "fuzz-build")
        self.assertIn("fuzz/rust-toolchain.toml", text)
        self.assertRegex(text, r"tool: cargo-fuzz@\d+\.\d+\.\d+\n")
        # Not cargo-binstall, which install-action would hand the GitHub token.
        self.assertIn("fallback: cargo-install", text)
        self.assertIn("workspaces: fuzz", text)

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
        self.assertIn(
            "cargo deny --locked check advisories -D advisory-not-detected",
            [r.strip() for r in runs],
        )

    def test_codeql(self):
        wf = load_workflows()["codeql.yml"]
        on = triggers(wf)
        self.assertIn("pull_request", on)
        self.assertEqual(on["push"]["branches"], ["main"])
        self.assertTrue(on["schedule"][0]["cron"])
        self.assertEqual(wf["permissions"], {"contents": "read"})
        job = next(iter(jobs(wf).values()))
        self.assertEqual(job["permissions"], {"contents": "read", "security-events": "write"})
        self.assertEqual(sorted(job["strategy"]["matrix"]["language"]), ["actions", "rust"])

    def test_scorecard(self):
        wf = load_workflows()["scorecard.yml"]
        on = triggers(wf)
        self.assertEqual(on["push"]["branches"], ["main"])
        self.assertTrue(on["schedule"][0]["cron"])
        self.assertIn("branch_protection_rule", on)
        # Scorecard publishes only from a workflow with no top-level write.
        self.assertEqual(wf["permissions"], {})
        self.assertNotIn("env", wf)
        self.assertNotIn("defaults", wf)
        job = next(iter(jobs(wf).values()))
        self.assertEqual(job["permissions"], {"security-events": "write", "id-token": "write"})
        steps = job["steps"]
        card = next(s for s in steps if step_uses(s, "ossf/scorecard-action"))
        self.assertIs(card["with"]["publish_results"], True)
        self.assertTrue(any(step_uses(s, "github/codeql-action/upload-sarif") for s in steps))
        self.assertFalse(any("run" in s for s in steps), "Scorecard allows no run steps here")

    def test_nightly_fuzz(self):
        wf = load_workflows()["nightly.yml"]
        on = triggers(wf)
        self.assertTrue(on["schedule"][0]["cron"])
        self.assertIn("workflow_dispatch", on)
        self.assertEqual(wf["permissions"], {"contents": "read"})
        text = job_text("nightly.yml", "fuzz")
        self.assertIn("tools/ci.sh fuzz", text)
        self.assertIn("FUZZ_SECONDS: 600", text)
        self.assertIn("if: failure()", text)        # the crash upload
        self.assertIn("fuzz/artifacts", text)
        # The corpus carries over from night to night, also after a crash.
        steps = jobs(wf)["fuzz"]["steps"]
        restore = [s for s in steps if step_uses(s, "actions/cache/restore")]
        save = [s for s in steps if step_uses(s, "actions/cache/save")]
        self.assertEqual(len(restore), 1)
        self.assertEqual(len(save), 1)
        self.assertEqual(restore[0]["with"]["path"], "fuzz/corpus")
        self.assertEqual(save[0]["with"]["path"], "fuzz/corpus")
        self.assertEqual(save[0]["if"], "always()")
        self.assertIn("${{ github.run_id }}", restore[0]["with"]["key"])
        self.assertTrue(restore[0]["with"]["key"].startswith(restore[0]["with"]["restore-keys"].strip()))
        # The same cargo-fuzz as ci.yml, which tools/dev-setup.sh reads.
        pin = re.compile(r"cargo-fuzz@([0-9.]+)")
        self.assertEqual(pin.findall(text), pin.findall(job_text("ci.yml", "fuzz-build")))
        self.assertIn("fallback: cargo-install", text)

    def test_nightly_miri(self):
        # Miri runs nightly, in the same scheduled workflow as fuzzing.
        text = job_text("nightly.yml", "miri")
        self.assertIn("tools/ci.sh miri", text)
        self.assertIn("fuzz/rust-toolchain.toml", text)
        self.assertIn("--component miri", text)


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

    def test_release_creates_a_draft(self):
        # A maintainer signs the files, then publishes the draft by hand.
        lines = [l for l in read("release.yml").splitlines() if "gh release create" in l]
        self.assertEqual(len(lines), 1)
        self.assertIn("--draft", lines[0])

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


class PrepareTests(unittest.TestCase):
    NAME = "release-prepare.yml"

    def wf(self):
        return load_workflows()[self.NAME]

    def test_starts_only_by_hand(self):
        on = triggers(self.wf())
        self.assertEqual(set(on), {"workflow_dispatch"})
        self.assertEqual(set(on["workflow_dispatch"]["inputs"]), {"version", "fixed-branch"})

    def test_one_run_at_a_time(self):
        self.assertEqual(self.wf()["concurrency"],
                         {"group": "release-prepare", "cancel-in-progress": False})

    def test_check_job_has_no_secrets(self):
        job = jobs(self.wf())["check"]
        self.assertNotIn("secrets.", job_text(self.NAME, "check"))
        self.assertIsNone(env_name(job))
        self.assertEqual(job["permissions"], {"contents": "read"})
        self.assertIn("prepare-check.sh", job_text(self.NAME, "check"))

    def test_push_job_in_release_branch_env(self):
        job = jobs(self.wf())["push"]
        self.assertEqual(env_name(job), "release-branch")
        self.assertIn("check", job["needs"])
        self.assertEqual(job["permissions"], {})
        checkout = [s for s in job["steps"] if s.get("uses", "").startswith("actions/checkout@")]
        self.assertEqual(len(checkout), 1)
        self.assertEqual(checkout[0]["with"]["ssh-key"], "${{ secrets.RELEASE_BRANCH_KEY }}")
        self.assertEqual(checkout[0]["with"]["fetch-depth"], 0)
        self.assertIn("prepare-branch.sh", job_text(self.NAME, "push"))

    def test_inputs_only_through_env(self):
        for job_id, job in jobs(self.wf()).items():
            for step in job.get("steps", []):
                self.assertNotIn("${{", step.get("run", ""), f"{job_id}: {step.get('name')}")

    def test_points_to_the_gate(self):
        self.assertIn("release-gate.yml --ref", job_text(self.NAME, "push"))


class EnvironmentTests(unittest.TestCase):
    def test_branch_key_only_in_release_branch_env(self):
        found = False
        for name, wf in load_workflows().items():
            for job_id, job in jobs(wf).items():
                if "secrets.RELEASE_BRANCH_KEY" in job_text(name, job_id):
                    found = True
                    self.assertEqual(env_name(job), "release-branch", f"{name}: {job_id}")
                    self.assertNotIn("uses", job, f"{name}: {job_id}")
        self.assertTrue(found)

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

    def test_promote_checks_the_versions_before_it_deploys(self):
        # irsync.org must not show a version that is not published: the
        # promote job reads the build's VERSIONS before it deploys.
        steps = jobs(load_workflows()["site.yml"])["promote"]["steps"]
        runs = [str(s.get("run", "")) for s in steps]
        check = [i for i, r in enumerate(runs) if "versions.py promotable site-out/VERSIONS" in r]
        deploy = [i for i, s in enumerate(steps)
                  if "CLOUDFLARE_API_TOKEN_PUB" in str(s.get("env", ""))]
        self.assertEqual(len(check), 1, runs)
        self.assertEqual(len(deploy), 1)
        self.assertLess(check[0], deploy[0])
        # Only the explicit input skips it.
        self.assertEqual(steps[check[0]].get("if"), "${{ !inputs.allow-unpublished }}")
        inputs = triggers(load_workflows()["site.yml"])["workflow_dispatch"]["inputs"]
        self.assertEqual(inputs["allow-unpublished"]["default"], False)

    def test_promote_takes_the_newest_staging_run_whatever_its_state(self):
        # With --status success it would ship the run before a failed or
        # running one, without a word.
        steps = jobs(load_workflows()["site.yml"])["promote"]["steps"]
        download = [str(s.get("run", "")) for s in steps if "gh run download" in str(s.get("run", ""))]
        self.assertEqual(len(download), 1)
        self.assertNotIn("--status success", download[0])
        self.assertIn("conclusion", download[0])

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
        self.assertNotIn("refs/heads/main", text)
        self.assertNotIn("default_branch", text)
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


# A pipe into grep -q; the same as in test_release_scripts.py.
PIPE_INTO_GREP_Q = re.compile(r"(?<!\|)\|(?!\|)\s*(\\\n\s*)?grep\s+(-\w*\s+)*-\w*q")


def run_steps():
    """(file, job id, step, shell) for every step with a run: script."""
    out = []
    for name, wf in load_workflows().items():
        wf_shell = ((wf.get("defaults") or {}).get("run") or {}).get("shell")
        for job_id, job in jobs(wf).items():
            job_shell = ((job.get("defaults") or {}).get("run") or {}).get("shell")
            for step in job.get("steps") or []:
                if "run" in step:
                    out.append((name, job_id, step, step.get("shell", job_shell or wf_shell)))
    return out


class RunScriptTests(unittest.TestCase):
    def test_no_pipe_into_grep_q(self):
        # With pipefail, grep -q stops reading at the first match, the writer
        # fails (SIGPIPE) and the match reads as a miss, now and then. See
        # NoPipeIntoGrepQTests in test_release_scripts.py. shell: bash runs
        # with -o pipefail too.
        found = []
        for name, job_id, step, shell in run_steps():
            run = step["run"]
            if "pipefail" not in run and shell != "bash":
                continue
            for m in PIPE_INTO_GREP_Q.finditer(run):
                found.append(f"{name} {job_id} {step.get('name')!r}: {m.group(0)!r}")
        self.assertEqual(found, [])

    def test_run_steps_finds_the_steps(self):
        steps = run_steps()
        self.assertIn("Pin the tested commit",
                      [s.get("name") for n, _, s, _ in steps if n == "release-gate.yml"])


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
        # pin-release.sh rechecks the tag and pins $GITHUB_SHA; its tests are
        # PinTests in test_release_scripts.py.
        steps = jobs(self.gate())["release"]["steps"]
        names = [s.get("name") for s in steps]
        pin = steps[names.index("Pin the tested commit")]
        self.assertEqual(pin["id"], "pin")
        self.assertEqual(pin["run"], '.github/scripts/pin-release.sh "$TAG" "$BRANCH"')
        self.assertEqual(pin["env"]["BRANCH"], "${{ needs.check.outputs.branch }}")
        self.assertLess(names.index("Pin the tested commit"),
                        names.index("Launch release.yml on the pinned commit"))
        text = job_text("release-gate.yml", "release")
        self.assertIn('--ref "refs/heads/pinned/$TAG"', text)
        self.assertIn('"$run_sha" != "$GITHUB_SHA"', text)

    def test_gate_clears_stale_pin(self):
        steps = jobs(self.gate())["release"]["steps"]
        last = steps[-1]
        self.assertEqual(last.get("if"), "always()")
        self.assertIn("pinned/$TAG", last["run"])

    def test_gate_refuses_while_old_release_run_active(self):
        text = job_text("release-gate.yml", "release")
        self.assertIn("in_progress", text)
        self.assertIn("queued", text)

    def test_resume_makes_no_branch(self):
        self.assertNotIn("branch", jobs(load_workflows()["release-resume.yml"]))
        self.assertNotIn("new-branch", read("release-resume.yml"))

    def test_gate_check_outputs(self):
        self.assertEqual(set(jobs(self.gate())["check"]["outputs"]), {"tag", "branch"})

    def test_gate_header_names_the_new_flow(self):
        with open(os.path.join(WORKFLOWS, "release-gate.yml")) as f:
            text = f.read()
        header = text[:text.index("\nname:")]
        self.assertIn("--ref release-1.2 -f version=1.2.0", header)
        self.assertIn("release-prepare.yml", header)

    def test_gate_points_to_signing(self):
        self.assertIn("sign-release.sh", job_text("release-gate.yml", "draft"))

    def test_gate_draft_job_needs_check_and_release(self):
        # check gives the tag; release must have made the draft.
        self.assertEqual(set(jobs(self.gate())["draft"]["needs"]), {"check", "release"})

    def test_gate_draft_job_can_see_drafts(self):
        # GitHub shows a draft release only to callers with push access.
        self.assertEqual(jobs(self.gate())["draft"]["permissions"]["contents"], "write")

    def test_gate_leftover_draft_step_fails_on_other_errors(self):
        text = job_text("release-gate.yml", "release")
        self.assertIn("release not found", text)
        self.assertIn("Could not look up the release", text)

    def test_gate_has_no_branch_or_finish_job(self):
        for job_id in ("branch", "finish"):
            self.assertNotIn(job_id, jobs(self.gate()))

    def test_gate_deletes_only_a_leftover_draft(self):
        text = job_text("release-gate.yml", "release")
        self.assertIn("isDraft", text)
        self.assertLess(text.index("isDraft"), text.index("gh workflow run release.yml"))

    def test_gate_and_resume_share_concurrency(self):
        self.assertEqual(self.gate()["concurrency"]["group"], "release-v${{ inputs.version }}")
        self.assertEqual(load_workflows()["release-resume.yml"]["concurrency"]["group"],
                         "release-${{ inputs.tag }}")

    def test_ci_runs_on_release_branches(self):
        push = triggers(load_workflows()["ci.yml"])["push"]
        self.assertEqual(push["branches"], ["main", "release-*"])


REPLACE_KEY = ("replace the test key with the real release key "
               "(book/src/contributing/release-signing-key.md)")


def key_lines(text):
    """The base64 key lines of a minisign public key file."""
    return [l.strip() for l in text.splitlines()
            if l.strip() and not l.startswith("untrusted comment:")]


class ReleaseKeyTests(unittest.TestCase):
    """Fails while packaging/signing/release-keys.pub holds the test key.

    This is meant to keep a pull request red until the maintainer puts in
    the real key. It fails in both workflows that run these tests: the
    `workflows` job of ci.yml (`tools/ci.sh workflows`) and the `scripts`
    job of release-tooling.yml. It is not part of `tools/ci.sh check`.
    """

    def read_root(self, *path):
        with open(os.path.join(ROOT, *path)) as f:
            return f.read()

    def readme_keys(self):
        """The lines of the ```text block under "## Verify a download"."""
        readme = self.read_root("README.md")
        section = readme.split("## Verify a download", 1)[1]
        m = re.search(r"```text\n(.*?)```", section, re.S)
        self.assertIsNotNone(m, f"no ```text block with the key in README.md; {REPLACE_KEY}")
        return [l.strip() for l in m[1].splitlines() if l.strip()]

    def test_replace_the_test_key_with_the_real_release_key(self):
        keys = self.read_root("packaging", "signing", "release-keys.pub")
        test_pub = self.read_root(".github", "scripts", "tests", "fixtures", "minisign",
                                  "test.pub")
        test_id = test_pub.splitlines()[0].split()[-1]
        for line in key_lines(test_pub):
            self.assertNotIn(line, keys, f"release-keys.pub holds the test key; {REPLACE_KEY}")
        self.assertNotIn(test_id, keys, f"release-keys.pub names the test key; {REPLACE_KEY}")
        self.assertTrue(key_lines(keys), f"release-keys.pub holds no key; {REPLACE_KEY}")
        for doc in ("README.md", os.path.join("book", "src", "how-to", "verify-a-download.md")):
            self.assertNotIn("REPLACE-WITH-RELEASE-KEY", self.read_root(doc),
                             f"{doc} has no real key yet; {REPLACE_KEY}")
        self.assertEqual(self.readme_keys(), key_lines(keys),
                         f"the key in README.md is not the one in release-keys.pub; {REPLACE_KEY}")


if __name__ == "__main__":
    unittest.main()
