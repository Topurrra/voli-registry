#!/usr/bin/env python3
"""Reject pull requests that add a reserved shim name to a package manifest.

Supply-chain review item 2.4: voli's shims dir is first on PATH, so a manifest
whose `bin` exposes `sudo` (or `ssh`, `git`, ...) replaces the system tool for
every user who installs it. The install client asks for explicit consent at
install time; this gate keeps such names out of the registry in the first
place — a package that already had a reserved name is allowed to keep it
(removing it would break users), but no package may gain one.

Usage:
  python tools/check_reserved_shims.py                # compare against origin/main
  python tools/check_reserved_shims.py --base <ref>
  python tools/check_reserved_shims.py --self-test    # self-test, no git needed
"""

import argparse
import subprocess
import sys
import tomllib
from pathlib import Path

# Keep in sync with RESERVED_NAMES in voli's crates/voli-core/src/shims.rs.
RESERVED_NAMES = {
    "sudo", "su", "ssh", "scp", "sftp", "git", "gpg", "python", "python3", "py",
    "pip", "pip3", "node", "npm", "npx", "bash", "sh", "zsh", "pwsh",
    "powershell", "cmd", "curl", "wget", "docker", "kubectl", "aws", "az",
    "gcloud", "gh", "pyenv", "brew", "java", "code", "openssl",
}


def shim_stem(path_like: str) -> str:
    """Shim base name for a bin path entry, matching the client's
    `file_stem_any_sep` (works with both `/` and `\\` separators)."""
    name = path_like.replace("\\", "/").rsplit("/", 1)[-1]
    # strip one extension (rg.exe -> rg), like std::path::Path::file_stem
    dot = name.rfind(".")
    if dot > 0:
        name = name[:dot]
    return name.lower()


def manifest_shim_names(text: str) -> set:
    """All shim names a manifest's `bin` claims."""
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError:
        return set()  # validate.yml's own parse check reports malformed TOML
    names = set()
    for entry in data.get("bin", []):
        if isinstance(entry, str):
            names.add(shim_stem(entry).lower())
        elif isinstance(entry, dict):
            raw = entry.get("name") or entry.get("path", "")
            names.add(str(raw).strip().lower())
    return names


def added_reserved_names(old_text: str, new_text: str) -> set:
    """Reserved names present in the new manifest but not the old one."""
    return manifest_shim_names(new_text) - manifest_shim_names(old_text)


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], check=True, capture_output=True, text=True
    ).stdout


def check_pr(base: str, manifests_dir: Path) -> list:
    """Changed manifest files that GAIN a reserved shim name."""
    problems = []
    changed = [
        line.strip()
        for line in git("diff", "--name-only", f"{base}...", "--", str(manifests_dir)).splitlines()
        if line.strip().endswith(".toml")
    ]
    for path in changed:
        try:
            old = subprocess.run(
                ["git", "show", f"{base}:{path}"],
                capture_output=True, text=True, check=True,
            ).stdout
        except subprocess.CalledProcessError:
            old = ""  # new file: every name is "added"
        new = Path(path).read_text(encoding="utf-8")
        gained = added_reserved_names(old, new) & RESERVED_NAMES
        if gained:
            problems.append((path, sorted(gained)))
    return problems


def self_test() -> int:
    failures = 0

    def expect(desc, got, want):
        nonlocal failures
        if got != want:
            print(f"FAIL {desc}: got {got!r}, want {want!r}")
            failures += 1
        else:
            print(f"ok   {desc}")

    expect("shim stem windows path", shim_stem("bin\\sudo.exe"), "sudo")
    expect("shim stem unix path", shim_stem("tools/rg.exe"), "rg")
    expect("shim stem bare", shim_stem("gpg"), "gpg")
    expect("shim stem dotfile-ish keeps stem", shim_stem(".hidden.sh"), ".hidden")

    old = 'bin = ["rg.exe"]'
    new = 'bin = ["rg.exe", "sudo"]'
    expect("sudo added is flagged", added_reserved_names(old, new), {"sudo"})
    expect("unchanged bins are not flagged", added_reserved_names(old, old), set())

    # A package that ALREADY had the reserved name keeps it (review item 2.4:
    # "the package did not already have" is the failing case).
    keep_old = 'bin = ["ssh.exe"]'
    keep_new = 'bin = ["ssh.exe"]'
    expect("pre-existing reserved name is allowed",
           added_reserved_names(keep_old, keep_new), set())

    table_old = '[[bin]]\nname = "rg"\npath = "bin/rg.exe"\n'
    table_new = '[[bin]]\nname = "rg"\npath = "bin/rg.exe"\n[[bin]]\nname = "curl"\npath = "bin/curl"\n'
    expect("table entry gain flagged",
           added_reserved_names(table_old, table_new), {"curl"})

    expect("all reserved names are lowercase", all(n == n.lower() for n in RESERVED_NAMES), True)

    print(f"self-test: {'PASS' if failures == 0 else 'FAIL'} ({failures} failure(s))")
    return 1 if failures else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", default="origin/main",
                        help="base ref to diff against (default origin/main)")
    parser.add_argument("--manifests", default="manifests",
                        help="manifests directory (default manifests/)")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    problems = check_pr(args.base, Path(args.manifests))
    if problems:
        for path, names in problems:
            for name in names:
                print(
                    f"::error file={path}::"
                    f"adds reserved shim name '{name}' — a voli shim shadows the "
                    f"same-named system command for every user who installs it. "
                    f"Rename the bin entry or remove it."
                )
        return 1
    print("no reserved shim names added")
    return 0


if __name__ == "__main__":
    sys.exit(main())
