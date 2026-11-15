#!/usr/bin/env python3
"""Check or apply the GitHub settings that guard irsync's supply chain.

    tools/github_settings.py [check]        print every difference; exit 1 if any
    tools/github_settings.py apply [--repo R]

Needs `gh`, logged in as an admin of the irsync-dev org. `check` only reads.
What each setting is for: book/src/contributing/supply-chain.md.

The desired state is desired() below. A setting that GitHub has but this
file doesn't mention is left alone.
"""
import argparse
import json
import os
import subprocess
import sys

ORG = "irsync-dev"
MAIN = f"{ORG}/irsync"
REQUIRED_CHECK = "CI result"  # job `ci-result` in .github/workflows/ci.yml

SECURITY_ON = {
    "secret_scanning": "enabled",
    "secret_scanning_push_protection": "enabled",
}


def ruleset(target, include, rules):
    return {"target": target, "include": include, "rules": rules, "bypass_actors": []}


def desired(maintainer_id):
    """repo -> settings. Rules map a rule type to its parameters (or {})."""
    protect = {"deletion": {}, "non_fast_forward": {}}
    return {
        MAIN: {
            "security": dict(SECURITY_ON, dependabot_security_updates="enabled"),
            "actions": {"enabled": True, "allowed_actions": "all",
                        "sha_pinning_required": True},
            "immutable_releases": True,
            # Needed by dependency review in ci.yml. No API turns it on.
            "dependency_graph": True,
            # Every Actions secret lives in an environment. The release gate
            # calls packages.yml and publish-homebrew.yml with
            # `secrets: inherit`, so a repo-wide secret would reach them.
            "repo_wide_secrets": [],
            "environments": {
                "release": {"policies": [{"name": "v*", "type": "tag"}],
                            "reviewers": [maintainer_id],
                            "prevent_self_review": False},
                "site": {"policies": [{"name": "main", "type": "branch"}],
                         "reviewers": [], "prevent_self_review": False},
            },
            "rulesets": {
                "main": ruleset("branch", ["~DEFAULT_BRANCH"], dict(protect, **{
                    "pull_request": {"required_approving_review_count": 0},
                    "required_status_checks": {"contexts": [REQUIRED_CHECK]},
                })),
                "Release branches": ruleset("branch", ["refs/heads/release-*"], dict(protect, **{
                    "pull_request": {"required_approving_review_count": 0},
                    "required_status_checks": {"contexts": [REQUIRED_CHECK]},
                })),
                # The release gate creates and deletes these; nobody can move one.
                "Pinned release commits": ruleset("branch", ["refs/heads/pinned/*"],
                                                  {"update": {}, "non_fast_forward": {}}),
                "Release tags": ruleset("tag", ["refs/tags/v*"],
                                        dict(protect, update={})),
            },
        },
        f"{ORG}/iroh": {
            "security": dict(SECURITY_ON),
            "rulesets": {"Pinned branches": ruleset("branch", ["refs/heads/irsync-*"],
                                                    dict(protect))},
        },
        # The release jobs push to these directly, so no pull request rule.
        f"{ORG}/homebrew-tap": {
            "security": dict(SECURITY_ON),
            "rulesets": {"Default branch": ruleset("branch", ["~DEFAULT_BRANCH"],
                                                   dict(protect))},
        },
        f"{ORG}/packages": {
            "security": dict(SECURITY_ON),
            "rulesets": {"Default branch": ruleset("branch", ["~DEFAULT_BRANCH"],
                                                   dict(protect))},
        },
    }


def diff(want, have, path=""):
    """One line per difference: '<repo>: <path>: want X, have Y'.

    Keys `have` holds but `want` doesn't are ignored, except in a ruleset's
    `rules`: an extra rule is reported (as the two lists of rule types)."""
    out = []
    for key, w in want.items():
        h = have.get(key) if isinstance(have, dict) else None
        here = f"{path}.{key}" if path else key
        if key == "rules" and isinstance(h, dict) and set(h) - set(w):
            # A rule GitHub has but we don't want is a difference too.
            out.append((here, sorted(w), sorted(h)))
        elif isinstance(w, dict) and w and isinstance(h, dict):
            out += diff(w, h, here)
        elif w != h:
            out.append((here, w, h))
    if path:
        return out
    lines = []
    for here, w, h in out:
        repo, _, rest = here.partition(".")
        if not rest:
            rest = "(repo)"
        lines.append(f"{repo}: {rest}: want {w}, have {'(missing)' if h is None else h}")
    return lines


# --- the GitHub API -------------------------------------------------------

def gh(*args, body=None, ok_missing=False):
    cmd = ["gh", "api", *args]
    if body is not None:
        cmd += ["--input", "-"]
    r = subprocess.run(cmd, input=json.dumps(body) if body is not None else None,
                       capture_output=True, text=True)
    if r.returncode != 0:
        if ok_missing and "404" in r.stderr + r.stdout:
            return None
        sys.exit(f"gh api {' '.join(args)} failed: {r.stderr.strip() or r.stdout.strip()}")
    return json.loads(r.stdout) if r.stdout.strip() else None


def normalize_ruleset(api):
    rules = {}
    for rule in api.get("rules", []):
        params = rule.get("parameters") or {}
        if rule["type"] == "required_status_checks":
            params = {"contexts": sorted(c["context"] for c in
                                         params.get("required_status_checks", []))}
        elif rule["type"] == "pull_request":
            params = {"required_approving_review_count":
                      params.get("required_approving_review_count")}
        rules[rule["type"]] = params
    return ruleset(api["target"], api["conditions"]["ref_name"]["include"],
                   rules) | {"bypass_actors": api.get("bypass_actors", [])}


def ruleset_payload(name, want):
    rules = []
    for kind, params in want["rules"].items():
        if kind == "required_status_checks":
            params = {"strict_required_status_checks_policy": False,
                      "required_status_checks": [{"context": c} for c in params["contexts"]]}
        elif kind == "pull_request":
            params = {"required_approving_review_count": params["required_approving_review_count"],
                      "dismiss_stale_reviews_on_push": False,
                      "require_code_owner_review": False,
                      "require_last_push_approval": False,
                      "required_review_thread_resolution": False}
        rules.append({"type": kind, "parameters": params} if params else {"type": kind})
    return {"name": name, "target": want["target"], "enforcement": "active",
            "conditions": {"ref_name": {"include": want["include"], "exclude": []}},
            "rules": rules, "bypass_actors": want["bypass_actors"]}


def fetch(repo):
    info = gh(f"repos/{repo}")
    sa = info.get("security_and_analysis") or {}
    state = {"security": {k: (v or {}).get("status") for k, v in sa.items()}}
    state["actions"] = gh(f"repos/{repo}/actions/permissions")
    imm = gh(f"repos/{repo}/immutable-releases", ok_missing=True)
    state["immutable_releases"] = bool(imm and imm.get("enabled"))
    state["repo_wide_secrets"] = sorted(
        s["name"] for s in (gh(f"repos/{repo}/actions/secrets") or {}).get("secrets", []))
    # The SBOM endpoint answers only while the dependency graph is on.
    state["dependency_graph"] = gh(f"repos/{repo}/dependency-graph/sbom",
                                   ok_missing=True) is not None
    envs = {}
    for env in (gh(f"repos/{repo}/environments", ok_missing=True) or {}).get("environments", []):
        reviewers, self_review = [], False
        for rule in env.get("protection_rules", []):
            if rule["type"] == "required_reviewers":
                reviewers = [r["reviewer"]["id"] for r in rule.get("reviewers", [])]
                self_review = rule.get("prevent_self_review", False)
        pol = gh(f"repos/{repo}/environments/{env['name']}/deployment-branch-policies",
                 ok_missing=True) or {}
        envs[env["name"]] = {
            "policies": sorted(({"name": p["name"], "type": p.get("type", "branch")}
                                for p in pol.get("branch_policies", [])),
                               key=lambda p: (p["type"], p["name"])),
            "reviewers": reviewers, "prevent_self_review": self_review}
    state["environments"] = envs
    state["rulesets"] = {r["name"]: normalize_ruleset(gh(f"repos/{repo}/rulesets/{r['id']}"))
                         for r in gh(f"repos/{repo}/rulesets") or []}
    return state


def stray_tag_lines(output):
    """One difference line per tag that stray-tags.sh printed."""
    return [f"{MAIN}: tag {tag}: has no GitHub Release (pushed by hand?)"
            for tag in output.split()]


def stray_tags():
    """Run .github/scripts/stray-tags.sh; return difference lines.

    The script exits 1 with tags on stdout when there are strays. Any other
    failure leaves stdout empty: report that as an error, not as clean."""
    script = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..",
                          ".github", "scripts", "stray-tags.sh")
    r = subprocess.run([script], capture_output=True, text=True,
                       env=dict(os.environ, GH_REPO=MAIN))
    if r.returncode != 0 and not r.stdout.strip():
        return [f"{MAIN}: could not check for stray tags: "
                f"{r.stderr.strip() or f'stray-tags.sh exited {r.returncode}'}"]
    return stray_tag_lines(r.stdout)


def required_check_has_passed(repo):
    """True if the checked-out commit has a successful REQUIRED_CHECK run."""
    sha = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True,
                         text=True, check=True).stdout.strip()
    runs = gh(f"repos/{repo}/commits/{sha}/check-runs?check_name={REQUIRED_CHECK.replace(' ', '%20')}",
              ok_missing=True) or {}
    return any(r.get("conclusion") == "success" for r in runs.get("check_runs", []))


def apply(repo, want):
    if "security" in want:
        sec = dict(want["security"])
        dep = sec.pop("dependabot_security_updates", None)
        gh(f"repos/{repo}", "-X", "PATCH",
           body={"security_and_analysis": {k: {"status": v} for k, v in sec.items()}})
        if dep == "enabled":
            gh(f"repos/{repo}/automated-security-fixes", "-X", "PUT")
    if "actions" in want:
        gh(f"repos/{repo}/actions/permissions", "-X", "PUT", body=want["actions"])
    if want.get("repo_wide_secrets") == []:
        extra = (gh(f"repos/{repo}/actions/secrets") or {}).get("secrets", [])
        if extra:
            print(f"{repo}: move these secrets into an environment and delete them "
                  f"repo-wide (not done automatically): {', '.join(s['name'] for s in extra)}")
    if want.get("immutable_releases"):
        gh(f"repos/{repo}/immutable-releases", "-X", "PUT")
    if want.get("dependency_graph") and gh(f"repos/{repo}/dependency-graph/sbom",
                                           ok_missing=True) is None:
        print(f"{repo}: turn on the dependency graph by hand: Settings, Code security, "
              "Dependency graph, Enable. There is no API for it.")
    for name, env in want.get("environments", {}).items():
        gh(f"repos/{repo}/environments/{name}", "-X", "PUT", body={
            "reviewers": [{"type": "User", "id": i} for i in env["reviewers"]],
            "prevent_self_review": env["prevent_self_review"],
            "deployment_branch_policy": {"protected_branches": False,
                                         "custom_branch_policies": True}})
        base = f"repos/{repo}/environments/{name}/deployment-branch-policies"
        have = (gh(base) or {}).get("branch_policies", [])
        for p in have:
            if {"name": p["name"], "type": p.get("type", "branch")} not in env["policies"]:
                gh(f"{base}/{p['id']}", "-X", "DELETE")
        for p in env["policies"]:
            if not any(h["name"] == p["name"] and h.get("type", "branch") == p["type"]
                       for h in have):
                gh(base, "-X", "POST", body=p)
    existing = {r["name"]: r["id"] for r in gh(f"repos/{repo}/rulesets") or []}
    for name, rs in want.get("rulesets", {}).items():
        if repo == MAIN and name in ("main", "Release branches") and name not in existing \
                and not required_check_has_passed(repo):
            print(f"{repo}: skipping ruleset '{name}': no successful '{REQUIRED_CHECK}' "
                  "check on this commit yet. Push it, let CI pass, and run apply again.")
            continue
        payload = ruleset_payload(name, rs)
        if name in existing:
            gh(f"repos/{repo}/rulesets/{existing[name]}", "-X", "PUT", body=payload)
        else:
            gh(f"repos/{repo}/rulesets", "-X", "POST", body=payload)


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("command", nargs="?", choices=["check", "apply"], default="check")
    ap.add_argument("--repo", help="only this repo, for example irsync-dev/iroh")
    args = ap.parse_args()

    want = desired(gh("user")["id"])
    if args.repo:
        want = {args.repo: want[args.repo]}

    if args.command == "apply":
        for repo, settings in want.items():
            print(f"applying {repo}")
            apply(repo, settings)

    have = {repo: fetch(repo) for repo in want}
    lines = diff(want, have)
    if MAIN in want:
        lines += stray_tags()
    org = gh(f"orgs/{ORG}")
    if not org.get("two_factor_requirement_enabled"):
        lines.append(f"{ORG}: two-factor authentication is not required. Turn it on in the "
                     "org settings (Authentication security); there is no API for it.")
    for line in lines:
        print(line)
    if not lines:
        print("All settings match.")
    sys.exit(1 if lines else 0)


if __name__ == "__main__":
    main()
