#!/usr/bin/env python3
"""Decide whether an auto-bump pull request may merge without the owner.

The supply-chain review (2026-11-03) counted "bot PRs never merge by
themselves" as a protection, so this gate merges only the narrowest kind of
change and re-checks everything itself instead of trusting the bump job:

  * The PR comes from this repo's own bump workflow: author
    `app/github-actions`, branch `auto-bump/...`, not a fork, base `main`,
    every commit made by `voli-registry-ci`, no `hold` label, and it has been
    open for the cool-down period (so the owner can veto any bump).
  * Every changed file is a NEW version file of a package that already exists
    (`manifests/<x>/<name>/<new>.toml` beside an older `<name>/<old>.toml`).
    Nothing is modified or deleted; nothing outside `manifests/` changes.
  * The new file equals the previous version with the old version string
    replaced by the new one, except for hash values. So the name, bins,
    shims, env, shortcuts, persist, depends, extract layout and the set of
    source blocks are all unchanged, every URL is the previous URL with the
    version swapped in, on the same host, over https.
  * CI downloads every URL itself and recomputes sha256 (and sha512 when the
    manifest has one). Any mismatch, download error or size-cap hit fails.

Anything else (new packages, other field changes, workflow or tool changes,
scoop-sync and skill-sync PRs, PRs from people) waits for the owner, exactly
as before.

Usage:
  automerge_gate.py candidates --repo OWNER/NAME [--pr N] [--cooldown-hours H]
  automerge_gate.py classify --base origin/main --out gate.json
  automerge_gate.py rehash gate.json
"""

import argparse
import datetime as dt
import hashlib
import json
import os
import subprocess
import sys
import tomllib
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import PurePosixPath
from urllib.parse import urlsplit

# Sibling module; also found under `python -I`, which drops the script dir.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import manifest_history as mh  # noqa: E402

BOT_LOGIN = "app/github-actions"
BOT_COMMITTER = "voli-registry-ci"
BRANCH_PREFIX = "auto-bump/"
HOLD_LABEL = "hold"
MAX_PACKAGES = 25
HASH_KEYS = ("sha256", "sha512")
MAX_DOWNLOAD_BYTES = 2 * 1024 * 1024 * 1024
USER_AGENT = "voli-registry-automerge"


# --------------------------------------------------------------- PR facts

def pr_refusals(pr: dict, now: dt.datetime, cooldown_hours: float) -> list:
    """Why `pr` (gh pr view --json shape) is not an auto-merge candidate."""
    why = []
    author = (pr.get("author") or {}).get("login", "")
    if author != BOT_LOGIN:
        why.append(f"author is {author!r}, not {BOT_LOGIN}")
    if not pr.get("headRefName", "").startswith(BRANCH_PREFIX):
        why.append(f"branch {pr.get('headRefName')!r} is not {BRANCH_PREFIX}*")
    if pr.get("isCrossRepository"):
        why.append("PR comes from a fork")
    if pr.get("baseRefName") != "main":
        why.append(f"base is {pr.get('baseRefName')!r}, not main")
    if pr.get("state") != "OPEN":
        why.append(f"state is {pr.get('state')}")
    if pr.get("isDraft"):
        why.append("PR is a draft")
    labels = {lbl.get("name", "").lower() for lbl in pr.get("labels") or []}
    if HOLD_LABEL in labels:
        why.append(f"labelled '{HOLD_LABEL}' by the owner")
    for c in pr.get("commits") or []:
        names = {a.get("name") for a in c.get("authors") or []}
        if names != {BOT_COMMITTER}:
            why.append(f"commit {c.get('oid', '?')[:9]} is not only by {BOT_COMMITTER}: {sorted(names)}")
    created = pr.get("createdAt")
    if created:
        opened = dt.datetime.fromisoformat(created.replace("Z", "+00:00"))
        age_h = (now - opened).total_seconds() / 3600
        if age_h < cooldown_hours:
            why.append(f"open {age_h:.1f} h, cool-down is {cooldown_hours:g} h")
    else:
        why.append("no createdAt")
    return why


# ------------------------------------------------------------ manifest diff

def _substitute(value, old: str, new: str):
    """`value` with every `old` inside every string replaced by `new`."""
    if isinstance(value, str):
        return value.replace(old, new)
    if isinstance(value, list):
        return [_substitute(v, old, new) for v in value]
    if isinstance(value, dict):
        return {k: _substitute(v, old, new) for k, v in value.items()}
    return value


def _diff(expected, actual, path: str, out: list) -> None:
    """Paths where `actual` differs from `expected`, hash values excepted."""
    if isinstance(expected, dict) and isinstance(actual, dict):
        for k in sorted(set(expected) | set(actual)):
            p = f"{path}.{k}" if path else k
            if k not in expected:
                out.append(f"{p} added")
            elif k not in actual:
                out.append(f"{p} removed")
            elif k in HASH_KEYS:
                if not (isinstance(actual[k], str) and len(actual[k]) in (64, 128)
                        and all(c in "0123456789abcdefABCDEF" for c in actual[k])):
                    out.append(f"{p} is not a hex digest")
            else:
                _diff(expected[k], actual[k], p, out)
    elif isinstance(expected, list) and isinstance(actual, list):
        if len(expected) != len(actual):
            out.append(f"{path} length {len(expected)} -> {len(actual)}")
        else:
            for i, (e, a) in enumerate(zip(expected, actual)):
                _diff(e, a, f"{path}[{i}]", out)
    elif expected != actual:
        out.append(f"{path} changed")


def _downloads(data: dict, label: str = "") -> list:
    """Every {url, sha256[, sha512]} entry in a manifest, wherever it sits."""
    found = []
    if isinstance(data, dict):
        if isinstance(data.get("url"), str):
            found.append({"where": label or "(root)", "url": data["url"],
                          **{k: data[k] for k in HASH_KEYS if isinstance(data.get(k), str)}})
        for k, v in data.items():
            if k != "autoupdate":
                found.extend(_downloads(v, f"{label}.{k}" if label else k))
    elif isinstance(data, list):
        for i, v in enumerate(data):
            found.extend(_downloads(v, f"{label}[{i}]"))
    return found


def compare_bump(prev_text: str, new_text: str) -> tuple:
    """(reasons, downloads) for a new version file against its predecessor.

    `reasons` is empty when the new file is exactly a version bump.
    """
    try:
        prev = tomllib.loads(prev_text)
        new = tomllib.loads(new_text)
    except tomllib.TOMLDecodeError as e:
        return [f"TOML does not parse: {e}"], []
    old_v, new_v = str(prev.get("version", "")), str(new.get("version", ""))
    reasons = []
    if not old_v or not new_v:
        return ["missing version"], []
    if mh.version_key(new_v) <= mh.version_key(old_v):
        reasons.append(f"version {new_v} is not newer than {old_v}")
    expected = _substitute(prev, old_v, new_v)
    _diff(expected, new, "", reasons)
    prev_dl = {d["where"]: d for d in _downloads(prev)}
    downloads = _downloads(new)
    for d in downloads:
        u = urlsplit(d["url"])
        if u.scheme != "https":
            reasons.append(f"{d['where']}: {u.scheme}:// URL, only https auto-merges")
        before = prev_dl.get(d["where"])
        if before is None:
            reasons.append(f"{d['where']}: download did not exist before")
        elif urlsplit(before["url"]).netloc.lower() != u.netloc.lower():
            reasons.append(f"{d['where']}: host {urlsplit(before['url']).netloc} -> {u.netloc}")
        if "sha256" not in d and "sha512" not in d:
            reasons.append(f"{d['where']}: no hash")
    return reasons, downloads


def classify(name_status: list, read_new, baseline) -> dict:
    """Verdict for a PR's changed files.

    name_status: [(status, path)] from `git diff --name-status` (A/M/D/R...).
    read_new(path) -> text of the PR's file; baseline(path) -> (path, text).
    """
    reasons, packages, downloads = [], [], []
    if not name_status:
        reasons.append("no changes")
    for status, path in name_status:
        p = PurePosixPath(path)
        if p.parts[:1] != ("manifests",) or p.suffix != ".toml" or len(p.parts) != 4:
            reasons.append(f"{path}: outside manifests/<x>/<name>/<version>.toml")
            continue
        if status != "A":
            reasons.append(f"{path}: status {status}; only new version files auto-merge")
            continue
        base_path, base_text = baseline(path)
        if base_path is None:
            reasons.append(f"{path}: new package (no earlier version)")
            continue
        why, dls = compare_bump(base_text, read_new(path))
        reasons.extend(f"{path}: {w}" for w in why)
        packages.append({"path": path, "previous": base_path})
        downloads.extend({**d, "manifest": path} for d in dls)
    if len(packages) > MAX_PACKAGES:
        reasons.append(f"{len(packages)} packages, more than {MAX_PACKAGES}")
    return {"eligible": not reasons, "reasons": reasons,
            "packages": packages, "downloads": downloads}


# ------------------------------------------------------------------- rehash

def hash_url(url: str, want: set, limit: int = MAX_DOWNLOAD_BYTES, opener=None) -> dict:
    """Stream `url` and return {algo: hexdigest} for each algo in `want`."""
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    hashers = {a: hashlib.new(a) for a in want}
    total = 0
    with (opener or urllib.request.urlopen)(req, timeout=120) as resp:
        while chunk := resp.read(1 << 20):
            total += len(chunk)
            if total > limit:
                raise ValueError(f"larger than {limit} bytes")
            for h in hashers.values():
                h.update(chunk)
    return {a: h.hexdigest() for a, h in hashers.items()}


def rehash(downloads: list, fetch=hash_url, workers: int = 4) -> list:
    """Failures (empty when every download hashes to its manifest value)."""

    def check(d) -> list:
        want = {a for a in HASH_KEYS if a in d}
        try:
            got = fetch(d["url"], want)
        except Exception as e:  # noqa: BLE001 - any failure blocks the merge
            return [f"{d['manifest']} {d['where']}: download failed: {e}"]
        return [f"{d['manifest']} {d['where']}: {a} mismatch "
                f"(manifest {d[a][:16]}..., download {got[a][:16]}...)"
                for a in sorted(want) if got[a].lower() != d[a].lower()]

    with ThreadPoolExecutor(max_workers=workers) as pool:
        return [f for result in pool.map(check, downloads) for f in result]


# --------------------------------------------------------------------- CLI

def _git(*args) -> str:
    return subprocess.run(["git", *args], check=True, capture_output=True,
                          text=True, encoding="utf-8").stdout


def _gh_json(*args):
    return json.loads(subprocess.run(["gh", *args], check=True, capture_output=True,
                                     text=True, encoding="utf-8").stdout)


PR_FIELDS = ("number,author,headRefName,headRefOid,isCrossRepository,baseRefName,"
             "state,isDraft,labels,commits,createdAt")


def cmd_candidates(args) -> int:
    if args.pr:
        if not args.pr.isdigit():
            print(f"::error::--pr must be a number, got {args.pr!r}")
            return 1
        prs = [_gh_json("pr", "view", args.pr, "-R", args.repo, "--json", PR_FIELDS)]
    else:
        prs = _gh_json("pr", "list", "-R", args.repo, "--state", "open", "--limit", "50",
                       "--json", PR_FIELDS)
    now = dt.datetime.now(dt.timezone.utc)
    picked = []
    for pr in prs:
        why = pr_refusals(pr, now, args.cooldown_hours)
        if why:
            print(f"#{pr['number']}: owner review: " + "; ".join(why), file=sys.stderr)
        else:
            picked.append({"pr": pr["number"], "sha": pr["headRefOid"]})
    print(f"prs={json.dumps(picked)}")
    return 0


def cmd_classify(args) -> int:
    fork = mh.git_merge_base(args.base)
    rows = []
    for line in _git("diff", "--name-status", "--no-renames", f"{fork}...HEAD").splitlines():
        status, _, path = line.partition("\t")
        rows.append((status.strip(), path.strip()))
    verdict = classify(
        rows,
        read_new=lambda p: _git("show", f"HEAD:{p}"),
        baseline=lambda p: mh.git_baseline(fork, p),
    )
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(verdict, f, indent=2)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    lines = [f"auto-merge eligible: **{verdict['eligible']}** "
             f"({len(verdict['packages'])} packages, {len(verdict['downloads'])} downloads)"]
    lines += [f"- {r}" for r in verdict["reasons"]]
    print("\n".join(lines))
    if summary:
        with open(summary, "a", encoding="utf-8") as f:
            f.write("\n".join(lines) + "\n")
    return 0


def cmd_rehash(args) -> int:
    with open(args.gate, encoding="utf-8") as f:
        verdict = json.load(f)
    if not verdict.get("eligible"):
        print("::error::gate verdict is not eligible; refusing to rehash")
        return 1
    failures = rehash(verdict["downloads"])
    for msg in failures:
        print(f"::error::{msg}")
    if failures:
        return 1
    print(f"all {len(verdict['downloads'])} downloads match their manifest hashes")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("candidates")
    c.add_argument("--repo", required=True)
    c.add_argument("--pr", default="")
    c.add_argument("--cooldown-hours", type=float, default=24)
    k = sub.add_parser("classify")
    k.add_argument("--base", default="origin/main")
    k.add_argument("--out", default="gate.json")
    r = sub.add_parser("rehash")
    r.add_argument("gate")
    args = ap.parse_args()
    return {"candidates": cmd_candidates, "classify": cmd_classify,
            "rehash": cmd_rehash}[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
