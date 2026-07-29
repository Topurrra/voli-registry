# Attribution

## Scoop Main and Extras buckets

A large share of the manifests under `manifests/` are automatically converted
from Scoop's [Main](https://github.com/ScoopInstaller/Main) and
[Extras](https://github.com/ScoopInstaller/Extras) buckets. Both are
distributed under the **Unlicense**.

The conversion is mechanical (Scoop JSON → Voli TOML; see `tools/`). Package
metadata, including names, versions, download URLs, hashes, bins, and shortcuts,
originates upstream.

Manifests requiring executable install or uninstall scripts are not imported.
Benign `post_install` and `post_uninstall` metadata is dropped because Voli
expresses persistence declaratively and cannot execute scripts.

Upstream licenses:

- <https://github.com/ScoopInstaller/Main/blob/master/LICENSE>
- <https://github.com/ScoopInstaller/Extras/blob/master/LICENSE>

## Agent Skills catalog

The manifests under `manifests/skills/` and the archives on the `skills`
release are built by `tools/skill-import.py` from the pinned upstream
repositories listed in `skill-sources.toml`. Each source's license is recorded
there and pinned by SHA-256; every archive ships the upstream `LICENSE` (as
`LICENSE.upstream` when the skill directory does not carry its own).

Skill content is copied **byte for byte**, with exactly one declared exception.
When two upstreams ship a skill under the same name, the bare name stays with
the first source in `skill-sources.toml` order and later claimants are
published as `<prefix>-<name>`. Voli's client requires the manifest name, the
archive's top-level directory, and the `name:` field in the archived `SKILL.md`
to be identical, so for a renamed skill the importer rewrites that one
frontmatter field. Nothing else in the file is touched, and every rename is
listed in `skill-import-report.md`. The `homepage` in each manifest points at
the exact upstream commit and path the skill came from.

## This repository

Everything else in this repository (the layout, CI, and Voli-authored
manifests) is MIT-licensed — see `LICENSE`.
