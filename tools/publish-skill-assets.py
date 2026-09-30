#!/usr/bin/env python3
"""Upload a generated skill release plan, preserving all existing assets."""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path


class PublishFailure(RuntimeError):
    pass


def gh(*args: str, missing_ok: bool = False) -> str | None:
    result = subprocess.run(["gh", *args], capture_output=True, text=True, check=False)
    if result.returncode:
        if missing_ok and "(HTTP 404)" in result.stderr:
            return None
        raise PublishFailure(result.stderr.strip() or f"gh failed: {args}")
    return result.stdout


def publish(plan: dict, assets: Path, repo: str, target: str) -> None:
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo):
        raise PublishFailure("invalid repository")
    if not re.fullmatch(r"[0-9a-f]{40}", target):
        raise PublishFailure("target must be the checked-out commit SHA")
    releases = plan.get("releases")
    if not isinstance(releases, list):
        raise PublishFailure("release plan must contain a releases list")
    tags = set()
    # Check the complete local plan before making any remote change.
    for release in releases:
        tag = release["tag"]
        if not re.fullmatch(r"skills-[a-z0-9]+(?:-[a-z0-9]+)*-v[0-9]+(?:\.[0-9]+)*", tag) or tag in tags:
            raise PublishFailure(f"invalid or duplicate release tag: {tag}")
        tags.add(tag)
        entries = release["assets"]
        if not entries or len(entries) > 1000:
            raise PublishFailure(f"release {tag} needs between 1 and 1000 assets")
        names = set()
        for entry in entries:
            name = entry["path"]
            if not re.fullmatch(r"[A-Za-z0-9_.-]+\.zip", name) or name in names:
                raise PublishFailure(f"unsafe or duplicate asset path: {name}")
            names.add(name)
            path = assets / name
            if path.is_symlink() or not path.is_file():
                raise PublishFailure(f"missing regular archive: {name}")
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            if entry["sha256"] != digest:
                raise PublishFailure(f"local checksum mismatch: {name}")

    for release in releases:
        tag = release["tag"]
        response = gh("api", f"repos/{repo}/releases/tags/{tag}", missing_ok=True)
        remote = json.loads(response) if response is not None else None
        if remote is not None and remote.get("draft"):
            raise PublishFailure(f"release {tag} is a draft; public archive URLs are unavailable")
        existing = remote["assets"] if remote is not None else []
        by_name = {asset["name"]: asset for asset in existing}
        pending = []
        for entry in release["assets"]:
            name = entry["path"]
            if name in by_name:
                # An unknown/mismatching remote digest is not permission to
                # overwrite content referenced by a published manifest.
                if by_name[name].get("digest") != "sha256:" + entry["sha256"]:
                    raise PublishFailure(f"remote checksum conflict: {tag}/{name}")
            else:
                pending.append(str(assets / name))
        if len(existing) + len(pending) > 1000:
            raise PublishFailure(f"release {tag} would exceed the 1000-asset limit")
        if response is None:
            gh("release", "create", tag, "--repo", repo, "--target", target,
               "--latest=false", "--title", f"Skill archives: {tag}",
               "--notes", "Deterministic skill archives from pinned, license-checked sources.")
        if pending:
            gh("release", "upload", tag, *pending, "--repo", repo)
        print(f"{tag}: uploaded {len(pending)}, retained {len(release['assets']) - len(pending)}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--assets", type=Path, required=True)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--target", required=True)
    args = parser.parse_args()
    try:
        plan = json.loads((args.assets / "release-plan.json").read_text(encoding="utf-8"))
        publish(plan, args.assets, args.repo, args.target)
        return 0
    except (PublishFailure, OSError, ValueError, KeyError, TypeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
