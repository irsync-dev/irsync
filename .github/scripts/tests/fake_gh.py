#!/usr/bin/env python3
"""A stand-in for the gh CLI, for the release script tests.

State lives in the JSON file named by FAKE_GH_STATE:
  releases: {tag: {"body": str, "isDraft": bool, "isPrerelease": bool}}; for
    sign-release.sh also "databaseId": int, "targetCommitish": str,
    "immutable": bool and "assets": {name: content}
  tags: [tag, ...]
  branches: [name, ...] (optional, default [])
  generated: str (what generate-notes returns)
  bad_attestations: [asset name, ...] (attestation verify fails for these)
  drop_upload: [asset name, ...] (release upload leaves these out)
  not_immutable: bool (publishing leaves the release mutable)
  patched: {field: value} (the fields of the last PATCH of a release)
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


def fields(flags=("-f",)):
    return {a.split("=", 1)[0]: a.split("=", 1)[1]
            for i, a in enumerate(args) if i and args[i - 1] in flags}


def positional(start):
    """The arguments from args[start:] that are not options or their values."""
    takes_value = {"-X", "-f", "-F", "-H", "--jq", "--dir", "--repo",
                   "--signer-workflow", "--ref", "--json"}
    out, i = [], start
    while i < len(args):
        if args[i] in takes_value:
            i += 2
        elif args[i].startswith("-"):
            i += 1
        else:
            out.append(args[i])
            i += 1
    return out


def release(tag):
    rel = state["releases"].get(tag)
    if rel is None:
        fail("release not found")
    return rel


if args[:2] == ["release", "view"]:
    rel = state["releases"].get(args[2])
    if rel is None:
        fail("release not found")
    want = opt("--json")
    if want == "body":
        print(rel["body"])
    elif want == "isDraft,isPrerelease":
        print(f'{str(rel["isDraft"]).lower()} {str(rel["isPrerelease"]).lower()}')
    elif want == "isDraft,assets,targetCommitish,databaseId":
        print(f'{str(rel["isDraft"]).lower()} {rel["targetCommitish"]} {rel["databaseId"]}')
        for name in rel["assets"]:
            print(name)
    elif want == "assets":
        for name in rel["assets"]:
            print(name)
    elif want == "isDraft,isImmutable":
        print(f'{str(rel["isDraft"]).lower()} {str(rel.get("immutable", False)).lower()}')
    else:
        fail(f"unexpected --json {want}")
elif args[:2] == ["release", "edit"]:
    with open(opt("--notes-file")) as f:
        state["releases"][args[2]]["body"] = f.read()
elif args[:2] == ["release", "download"]:
    rel = release(args[2])
    for name, content in rel["assets"].items():
        with open(os.path.join(opt("--dir"), name), "w") as f:
            f.write(content)
elif args[:2] == ["release", "upload"]:
    rel = release(args[2])
    for upload in positional(3):
        name = os.path.basename(upload)
        if name in rel["assets"] and "--clobber" not in args:
            fail(f"asset {name} already exists")
        if name in state.get("drop_upload", []):
            continue
        with open(upload) as f:
            rel["assets"][name] = f.read()
elif args[:2] == ["attestation", "verify"]:
    if os.path.basename(args[2]) in state.get("bad_attestations", []):
        fail(f"no attestation for {args[2]}")
    print("Verification succeeded!")
elif args[:2] == ["workflow", "run"]:
    pass
elif args[0] == "api":
    endpoint = positional(1)[0]
    repo = os.environ["GH_REPO"]
    tags_prefix = f"repos/{repo}/git/matching-refs/tags/"
    heads_prefix = f"repos/{repo}/git/matching-refs/heads/"
    if endpoint.startswith(tags_prefix):
        for t in state["tags"]:
            if t.startswith(endpoint[len(tags_prefix):]):
                print(t)
    elif endpoint.startswith(heads_prefix):
        for b in state.get("branches", []):
            if b.startswith(endpoint[len(heads_prefix):]):
                print(b)
    elif endpoint == f"repos/{repo}/releases":
        for t in state["releases"]:
            print(t)
    elif endpoint.startswith(f"repos/{repo}/releases/") and opt("-X") == "PATCH":
        rid = int(endpoint.rsplit("/", 1)[1])
        tag = next(t for t, r in state["releases"].items() if r.get("databaseId") == rid)
        state["patched"] = fields(("-f", "-F"))
        if state["patched"].get("draft") == "false":
            state["releases"][tag].update(isDraft=False,
                                          immutable=not state.get("not_immutable"))
            state["tags"].append(tag)
    elif endpoint == f"repos/{repo}/releases/generate-notes":
        state["generate"] = fields()
        print(state["generated"])
    else:
        fail(f"unexpected endpoint {endpoint}")
else:
    fail(f"unexpected call {args}")
save()
