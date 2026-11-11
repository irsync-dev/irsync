#!/usr/bin/env python3
"""A stand-in for the gh CLI, for the release script tests.

State lives in the JSON file named by FAKE_GH_STATE:
  releases: {tag: {"body": str, "isDraft": bool, "isPrerelease": bool}}
  tags: [tag, ...]
  generated: str (what generate-notes returns)
  calls: [argv, ...] (appended by every call)
Only the calls the scripts make are understood; anything else fails.
"""
import json
import os
import sys

path = os.environ["FAKE_GH_STATE"]
with open(path) as f:
    state = json.load(f)
args = sys.argv[1:]
state.setdefault("calls", []).append(args)


def save():
    with open(path, "w") as f:
        json.dump(state, f)


def fail(msg):
    save()
    print(msg, file=sys.stderr)
    sys.exit(1)


def opt(name):
    return args[args.index(name) + 1] if name in args else None


def fields():
    return {a.split("=", 1)[0]: a.split("=", 1)[1]
            for i, a in enumerate(args) if i and args[i - 1] == "-f"}


if args[:2] == ["release", "view"]:
    rel = state["releases"].get(args[2])
    if rel is None:
        fail("release not found")
    want = opt("--json")
    if want == "body":
        print(rel["body"])
    elif want == "isDraft,isPrerelease":
        print(f'{str(rel["isDraft"]).lower()} {str(rel["isPrerelease"]).lower()}')
    else:
        fail(f"unexpected --json {want}")
elif args[:2] == ["release", "edit"]:
    with open(opt("--notes-file")) as f:
        state["releases"][args[2]]["body"] = f.read()
elif args[0] == "api":
    rest = [a for a in args[1:] if a != "--paginate"]
    endpoint = rest[0]
    repo = os.environ["GH_REPO"]
    if endpoint == f"repos/{repo}/git/matching-refs/tags/v":
        for t in state["tags"]:
            print(t)
    elif endpoint == f"repos/{repo}/releases/generate-notes":
        state["generate"] = fields()
        print(state["generated"])
    else:
        fail(f"unexpected endpoint {endpoint}")
else:
    fail(f"unexpected call {args}")
save()
