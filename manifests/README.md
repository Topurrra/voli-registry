# `manifests/` — the package catalog

Every package version is **one TOML file**. This directory is the sole input to
the index build; nothing else in the repo affects what users can install.

## Layout contract

```
manifests/<first-letter>/<name>/<version>.toml
```

- `<first-letter>` — the first character of `<name>` (lowercase). `ripgrep` →
  `r`, `7zip` → `7`.
- `<name>` — the package name. **Must equal the `name` field inside the file**,
  and must be lowercase alphanumeric + dashes only (`a-z`, `0-9`, `-`).
- `<version>` — **must equal the `version` field inside the file.** One file per
  version; keep old versions (the index retains all of them).

Example:

```
manifests/
├── r/
│   └── ripgrep/
│       ├── 14.1.0.toml       # name = "ripgrep", version = "14.1.0"
│       └── 14.1.1.toml       # name = "ripgrep", version = "14.1.1"
└── f/
    └── fd/
        └── 10.1.0.toml       # name = "fd", version = "10.1.0"
```

`voli-index-tool validate manifests/` enforces all three matches (letter,
directory, filename ↔ manifest fields) and rejects duplicate `(name, version)`
pairs. CI runs it on every PR — see the repo root `README.md`.

## `_examples/` is excluded

Files under `manifests/_examples/` are **skipped** by both `validate` and the
index build. Put sample/reference manifests there; they never reach users. Do
not put real, installable packages under `_examples/`.

## Manifest schema

Full schema and rules live in the main repo: [`docs/Voli.md` §4][spec]. The hard
rules CI enforces:

- **`sha256` is mandatory** on every `[source.<arch>]` (64 hex chars). No hash,
  no merge.
- **No scripts, ever.** The manifest grammar cannot express code — there is no
  `pre_install` / `post_install` / `installer` field, and unknown fields are
  rejected. This is the security moat; do not add script-like escape hatches.
- **Portable archives only.** MSI/EXE installers are not supported in v1.
- At least one of `[source.x64]` / `[source.arm64]` must be present.
- `bin` paths must be relative (no absolute paths, no `..`).
- `[env]` values may only use the `{dir}` template variable.

Minimal example:

```toml
name = "ripgrep"
version = "14.1.1"
description = "Recursively search directories with a regex"
homepage = "https://github.com/BurntSushi/ripgrep"
license = "MIT OR Unlicense"
kind = "app"

# Top-level scalar keys MUST appear before any [table] header, or TOML absorbs
# them into the preceding table.
extract_dir = "ripgrep-14.1.1-x86_64-pc-windows-msvc"
bin = ["rg.exe"]

[source.x64]
url = "https://github.com/BurntSushi/ripgrep/releases/download/14.1.1/ripgrep-14.1.1-x86_64-pc-windows-msvc.zip"
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
```

## Imported (Scoop) manifests

Manifests converted from Scoop's `main` bucket are MIT-licensed from
[`ScoopInstaller/Main`](https://github.com/ScoopInstaller/Main) — see the repo
root `ATTRIBUTION.md`. The importer lives in `tools/`.

[spec]: https://github.com/Topurrra/voli/blob/main/docs/Voli.md
