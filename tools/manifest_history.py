"""What a changed manifest looked like before, for the PR gates.

A bump does not edit `manifests/<x>/<name>/<old>.toml`; it ADDS
`manifests/<x>/<name>/<new>.toml`. Gates that diffed "the same path at base"
therefore saw every bump as a brand-new package: the reserved-shim gate
flagged every routine bump of git/gh/curl/openssh, and the provenance report
skipped every bump. This module answers the real question: what was the
newest version of THIS package before the PR?

Pure helpers take plain data so they can be tested without git; the `git_*`
helpers wrap the subprocess calls.
"""

import re
import subprocess
from pathlib import PurePosixPath

_SPLIT = re.compile(r"[.\-_+]")


def version_key(version: str) -> tuple:
    """Numeric-aware sort key, like voli's `cmp_version` (1.10.0 > 1.9.0).

    Numbers compare as numbers and sort after text, so `1.0.0` > `1.0.rc1`.
    """
    key = []
    for token in _SPLIT.split(version):
        if token.isdigit():
            key.append((1, int(token), ""))
        else:
            key.append((0, 0, token))
    return tuple(key)


def package_dir(path: str) -> str:
    """`manifests/c/croc/11.6.3.toml` -> `manifests/c/croc`."""
    return str(PurePosixPath(path.replace("\\", "/")).parent)


def previous_version_path(path: str, siblings: list) -> str | None:
    """The newest sibling manifest strictly older than `path`'s version.

    `siblings` are the package's manifest paths at base. Returns None when the
    package has no older version there (a genuinely new package, or a
    downgrade, which callers treat as "no baseline").
    """
    own = PurePosixPath(path.replace("\\", "/"))
    own_key = version_key(own.stem)
    older = []
    for s in siblings:
        p = PurePosixPath(s.replace("\\", "/"))
        if p.suffix != ".toml" or p.name == own.name:
            continue
        if version_key(p.stem) < own_key:
            older.append(p)
    if not older:
        return None
    return str(max(older, key=lambda p: version_key(p.stem)))


def _git(*args: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(["git", *args], capture_output=True, text=True,
                          encoding="utf-8", check=check)


def git_show(ref: str, path: str) -> str | None:
    """`path` at `ref`, or None when it does not exist there."""
    out = _git("show", f"{ref}:{path}", check=False)
    return out.stdout if out.returncode == 0 else None


def git_siblings(ref: str, path: str) -> list:
    """Manifest paths in `path`'s package directory at `ref`."""
    out = _git("ls-tree", "--name-only", f"{ref}:{package_dir(path)}", check=False)
    if out.returncode != 0:
        return []
    return [f"{package_dir(path)}/{n}" for n in out.stdout.split() if n.endswith(".toml")]


def git_baseline(ref: str, path: str) -> tuple:
    """(baseline_path, text) for a changed manifest.

    The same path at `ref` when it existed (an edit), else the package's
    newest older version at `ref` (a bump), else (None, "") (a new package).
    """
    same = git_show(ref, path)
    if same is not None:
        return path, same
    prev = previous_version_path(path, git_siblings(ref, path))
    if prev is not None:
        text = git_show(ref, prev)
        if text is not None:
            return prev, text
    return None, ""


def git_merge_base(base: str) -> str:
    """The commit this branch forked from `base` at.

    Baselines come from here, not from `base` itself: when main moves on
    after a PR opens, main's newer files must not look like PR changes.
    """
    return _git("merge-base", base, "HEAD").stdout.strip()


def git_changed_manifests(fork_point: str, manifests_dir: str) -> list:
    """Manifest .toml paths changed since `fork_point`, committed or not.

    Includes uncommitted and untracked files: the bump workflow writes new
    version files and runs its report BEFORE committing them, so
    `git diff base...` alone reported "No package manifests changed" on every
    bump. Deleted paths are included; callers check existence.
    """
    committed = _git("diff", "--name-only", f"{fork_point}...HEAD", "--",
                     manifests_dir).stdout.split()
    worktree = _git("diff", "--name-only", "HEAD", "--", manifests_dir).stdout.split()
    untracked = _git("ls-files", "--others", "--exclude-standard", "--",
                     manifests_dir).stdout.split()
    return sorted({p for p in committed + worktree + untracked if p.endswith(".toml")})
