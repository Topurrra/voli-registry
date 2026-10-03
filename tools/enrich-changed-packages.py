#!/usr/bin/env python3
"""Enrich changed allowlisted app packages at their exact registry latest version.

Run from the registry checkout after the Windows importer/bump and before PR
creation. Git paths are NUL-delimited and importer arguments never use a shell.
No changed allowlisted packages means no importer execution or network access.
Generation holds write credentials, so payload execution is always deferred to
the separate read-only native verification jobs.
"""
import argparse
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tomllib


SAFE_NAME = re.compile(r"[a-z0-9][a-z0-9._+-]*\Z")
SAFE_VERSION = re.compile(r"[A-Za-z0-9][A-Za-z0-9._+-]*\Z")


def changed_packages(sources):
    with open(sources, "rb") as file:
        allowed = {source["name"] for source in tomllib.load(file)["source"]}
    if any(not SAFE_NAME.fullmatch(name) or ".." in name for name in allowed):
        raise ValueError("unsafe package name in allowlist")
    tracked = subprocess.check_output([
        "git", "diff", "--name-only", "-z", "--no-renames", "--diff-filter=AMT", "HEAD", "--", "manifests/",
    ])
    untracked = subprocess.check_output([
        "git", "ls-files", "--others", "--exclude-standard", "-z", "--", "manifests/",
    ])
    names = set()
    for raw in (tracked + untracked).split(b"\0"):
        if not raw:
            continue
        path = PurePosixPath(raw.decode("utf-8"))
        if path.suffix != ".toml" or path.parts[:2] == ("manifests", "skills"):
            continue
        parts = path.parts
        # Unrelated packages may have valid registry versions (for example
        # .june.2020.2) that do not use the strict exact-release tag syntax.
        if len(parts) < 3 or parts[2] not in allowed:
            continue
        if (len(parts) != 4 or parts[0] != "manifests"
                or not SAFE_NAME.fullmatch(parts[2]) or ".." in parts[2]
                or parts[1] != parts[2][0]
                or not SAFE_VERSION.fullmatch(path.stem) or ".." in path.stem
                or any(parent.is_symlink() for parent in [Path(path), *Path(path).parents])):
            raise ValueError(f"unsafe manifest path: {str(path)!r}")
        names.add(parts[2])
    return sorted(names)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sources", default="tools/brew-sources.toml")
    parser.add_argument("--importer", default="tools/brew-import/target/release/brew-import")
    args = parser.parse_args()
    try:
        names = changed_packages(args.sources)
        if not names:
            print("No changed allowlisted packages; skipping Unix enrichment.")
            return 0
        print("Enriching exact registry versions: " + ", ".join(names), flush=True)
        return subprocess.run([
            args.importer, "--sources", args.sources, "--manifests", "manifests",
            "--registry-version", "--static-only", "--only", ",".join(names),
        ], check=False).returncode
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"enrich-changed-packages: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
