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
packages the catalog, uploads archives as review artifacts, and opens a PR when
tracked output changes. Publishing those archives and merging the PR are
deferred manual actions.

## How CI works

- **`validate.yml`** (on PR): installs `voli-index-tool` and runs
  `voli-index-tool validate manifests/`. It parses every `.toml`, checks the
  layout, enforces exactly one strong hash, and rejects duplicates — reporting all errors,
  not just the first.
- **`publish.yml`** (on push to `main`): rebuilds the signed index and uploads
  the triple to the `index` release tag, replacing the assets in place.
- **`skill-sync.yml`** (weekly or manual): refreshes the allowlisted Tier-1
  skill catalog and opens a review PR without publishing or merging it.

Both workflows currently `cargo install --git … voli-index-tool` from source on
every run. Once the main repo ships prebuilt `voli-index-tool` release binaries,
swap that step for a binary download to cut CI from minutes to seconds.

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
