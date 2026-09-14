# voli-registry

The package catalog for **[Voli](https://github.com/Topurrra/voli)** — a fast,
honest, no-admin package manager for Windows.

Each package version is a single declarative TOML manifest. CI compiles every
manifest into a signed SQLite snapshot (`index.sqlite`) that the `voli` client
downloads, verifies, and searches offline. **No manifest can execute a script**
— the schema cannot express one. That is the security moat versus Scoop/Choco.

## Layout

```
manifests/<first-letter>/<name>/<version>.toml
manifests/skills/<first-letter>/<name>/<version>.toml
```

See [`manifests/README.md`](manifests/README.md) for the full layout contract
and schema. Example: `manifests/r/ripgrep/14.1.1.toml`.

## Contributing a manifest

1. Add `manifests/<first-letter>/<name>/<version>.toml`. The `<first-letter>`,
   `<name>` directory, and `<version>` filename must match the `name`/`version`
   fields inside the file.
2. Follow the rules in [`docs/Voli.md` §4][spec]:
   - Exactly one strong hash is mandatory on every `[source.<arch>]`:
     `sha256` (64 hex chars) or `sha512` (128 hex chars).
   - **No scripts.** There is no `pre_install`/`post_install`/`installer` field;
     unknown fields are rejected.
   - Portable archives are preferred. Hash-pinned MSI and explicitly identified
     Inno Setup packages may use `kind = "installer-archive"` for no-execute
     7-Zip extraction. Standalone EXEs remain unsupported.
3. Open a PR. CI validates it (below). Green check required to merge.

## Unix sources (Linux / macOS)

App manifests may carry unix blocks alongside the Windows ones:

```toml
[source.linux-x64]
url = "https://github.com/BurntSushi/ripgrep/releases/download/15.2.0/ripgrep-15.2.0-x86_64-unknown-linux-musl.tar.gz"
sha256 = "…"
extract_dir = "ripgrep-15.2.0-x86_64-unknown-linux-musl"

[source.macos-arm64]
url = "https://github.com/BurntSushi/ripgrep/releases/download/15.2.0/ripgrep-15.2.0-aarch64-apple-darwin.tar.gz"
sha256 = "…"
extract_dir = "ripgrep-15.2.0-aarch64-apple-darwin"
```

Rules (enforced by `voli-index-tool validate` and by the importer):

- Keys are `linux-x64`, `linux-arm64`, `macos-x64`, `macos-arm64`. `x64`/`arm64`
  keep meaning Windows. Selection never crosses OS lines: a Linux client only
  considers `linux-*`, and a Windows-only manifest yields a clear "no source
  for linux-x64" instead of installing foreign binaries.
- The top-level `extract_dir` belongs to the Windows archive and is **never
  inherited** by unix blocks. Unix payloads with a wrapper dir set a
  per-source `extract_dir`; flat archives (binary at the root) omit it.
- The shared `bin` list resolves `.exe`-tolerantly on unix: `bin = ["rg.exe"]`
  finds an extensionless `rg` in a unix payload (exact matches always win).
- Prefer musl-static Linux assets where upstream ships them; they run on any
  distro. `installer-archive` (EXE/MSI payloads) is Windows-only and
  meaningless in a unix block.
- Skills (`[source.any]`) are OS-independent already — nothing per-OS needed.

Payloads come from **upstream release assets, never Homebrew bottles**: a
bottle's ELF INTERP is `@@HOMEBREW_PREFIX@@/lib/ld.so`, so the kernel refuses
to exec it without Homebrew (proven by direct experiment). Homebrew's formulae
API is the recommended *discovery* source (version, SPDX license,
`executables` list) — see `tools/brew-sources.toml`.

`tools/brew-import` automates all of it: given the allowlist it resolves each
release, downloads every platform asset, runs static linkage gates (no
package-manager prefix references) plus a live `--version` smoke run for every
binary the host can execute, and merges verified blocks into the registry
(canonical TOML, round-trip validated). macOS/arm64 payloads a Linux run
cannot execute are hash-verified and marked UNVERIFIED; the `verify-unix` CI
job (`tools.yml`, ubuntu + macos legs) runs `brew-import --verify-only` over
them, which downloads, re-hashes, and executes each host payload.

### Coordinating the voli schema tag

Everything unix-shaped depends on a voli client/index-tool that understands
the new source keys. Until the cutover, every voli pin in
`.github/workflows/` carries a `PLACEHOLDER (unix schema)` comment pointing
here. To cut over, in ONE reviewed change:

1. Set every `VOLI_TAG` / `--tag` / `--branch` placeholder to the first voli
   tag carrying the schema (grep for `PLACEHOLDER` — validate, publish, bump,
   scoop-sync ×2, tools ×2). skill-sync stays: skills are unaffected.
2. Regenerate the importer locks against matching checkouts:
   `cargo update -p voli-core` in `tools/scoop-import` and
   `tools/brew-import`, commit both `Cargo.lock` files.
3. Merge a pilot manifest change and watch `validate`, `verify-unix`, and
   `publish` go green before anything else lands.

## Tier-1 skill catalog

`skill-sources.toml` allowlists exact upstream revisions, license hashes,
discovery roots, and exclusions. `tools/skill-import.py` validates that policy
and creates deterministic ZIP archives plus `kind = "skill"` manifests.

Run the importer with Python 3.11 or newer:

```sh
python tools/skill-import.py --self-test
python tools/skill-import.py --refresh-pins
python tools/skill-import.py \
  --checkouts /tmp/skill-sources \
  --manifests manifests \
  --assets /tmp/skill-assets \
  --report skill-import-report.md
voli-index-tool validate manifests/
```

The scheduled `skill-sync.yml` workflow refreshes pins, validates licenses,
packages the catalog, uploads the archives to the `skills` release, and opens a
PR when tracked output changes. Merging the PR stays a manual action.

Archives are published *before* the PR is opened, deliberately. An archive's
filename embeds its source id and version, so a bumped upstream revision renames
every archive for that source; if the manifests merged first, their download
URLs would 404 until someone uploaded by hand. `publish.yml` also refuses to
publish an index while any `manifests/skills/` URL is unreachable.

When two sources ship a skill under the same name, the bare name stays with the
first source in `skill-sources.toml` order and later claimants are published as
`<prefix>-<name>` (`prefix` defaults to the source id minus a trailing
`-skills`, overridable per source). Every rename is listed in
`skill-import-report.md`; see `ATTRIBUTION.md` for what the importer rewrites.

## How CI works

- **`validate.yml`** (on PR): installs `voli-index-tool` and runs
  `voli-index-tool validate manifests/`. It parses every `.toml`, checks the
  layout, enforces exactly one strong hash, and rejects duplicates — reporting all errors,
  not just the first.
- **`publish.yml`** (on push to `main`): rebuilds the signed index and uploads
  the triple to the `index` release tag, replacing the assets in place.
- **`skill-sync.yml`** (weekly or manual): refreshes the allowlisted Tier-1
  skill catalog, uploads the archives to the `skills` release, and opens a
  review PR without merging it.
- **`tools.yml`** (on PR touching `tools/**`): runs
  `python tools/skill-import.py --self-test` and the `scoop-import` Rust tests.

Every workflow `cargo install --git … voli-index-tool` from source on each run,
pinned to a full commit SHA. Keep it pinned: `publish.yml` runs that binary in
the step that holds `VOLI_INDEX_SIGNING_KEY`, so a floating branch ref would
hand code execution next to the signing key to anyone who can land a commit on
`voli@main`. Once the main repo ships prebuilt `voli-index-tool` release
binaries, swap that step for a checksum-verified binary download.

## Published index

`publish.yml` writes three assets to the GitHub Release tagged **`index`**:

| Asset               | Purpose                                                    |
| ------------------- | ---------------------------------------------------------- |
| `index.json`        | Tiny freshness pointer: `{ epoch, sha256, size }`.         |
| `index.sqlite.zst`  | zstd-compressed SQLite catalog (the payload).              |
| `index.sig`         | Ed25519 signature over the **decompressed** `index.sqlite`.|

The client fetches `<index_url>/index.json` first, compares `epoch`, and only
then downloads the snapshot, checks its size + sha256, and verifies `index.sig`
before atomically swapping its local index. Any check failing leaves the
existing index untouched.

### Point a client at this registry

```
voli config set index_url https://github.com/Topurrra/voli-registry/releases/download/index
voli update
```

That base URL resolves to the three release assets above
(`…/releases/download/index/index.json`, etc.).

## Signing key

The index is signed with an offline Ed25519 key, supplied to `publish.yml` via
the GitHub Actions secret **`VOLI_INDEX_SIGNING_KEY`** (hex-encoded 32-byte
secret). The client verifies against the public key embedded in the `voli`
binary.

## License

MIT — see [`LICENSE`](LICENSE). Manifests imported from Scoop's Main and Extras
buckets retain their upstream Unlicense attribution; see
[`ATTRIBUTION.md`](ATTRIBUTION.md).

[spec]: https://github.com/Topurrra/voli/blob/main/docs/Voli.md
