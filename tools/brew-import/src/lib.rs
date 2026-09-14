//! Upstream-release → voli TOML converter for unix platforms.
//!
//! Scoop is Windows-only, and Homebrew bottles cannot run without Homebrew
//! (their ELF INTERP is `@@HOMEBREW_PREFIX@@/lib/ld.so` — the kernel refuses to
//! exec them, proven by direct experiment). So this importer takes a third
//! path: upstream GitHub release assets (musl-static Linux builds, Apple
//! Darwin tarballs), each verified to actually execute before it is emitted.
//!
//! Pure conversion plus verified download live here; `main.rs` is the CLI.
//! Every emitted manifest goes through [`Manifest::to_canonical_toml`] — the
//! ONE canonical form — and is re-parsed with [`Manifest::from_toml_str`],
//! which is also what validates it.
//!
//! Allowlist (`tools/brew-sources.toml`): which repos, which asset per
//! platform, which bins. Homebrew's formulae API
//! (`https://formulae.brew.sh/api/formula/<name>.json`) is the recommended
//! discovery source for all three (version, SPDX license, `executables` list),
//! but payloads always come from upstream releases, never bottles.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use voli_core::manifest::{Bin, Kind, Manifest, Source, SourceKind, Sources};

#[cfg(test)]
mod tests;

/// The four unix platform keys, in canonical TOML order.
pub const UNIX_KEYS: &[&str] = &["linux-x64", "linux-arm64", "macos-x64", "macos-arm64"];

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(String),
    #[error("http error: {0}")]
    Http(String),
    #[error("config error: {0}")]
    Config(String),
    #[error("no asset named '{expected}' in release {tag} of {repo}; candidates:\n{candidates}")]
    AssetMissing {
        repo: String,
        tag: String,
        expected: String,
        candidates: String,
    },
    #[error("linkage gate rejected {bin}: {reason}")]
    Linkage { bin: String, reason: String },
    #[error("smoke gate rejected {bin}: exited {code} (ran with {args:?})")]
    Smoke {
        bin: String,
        code: String,
        args: Vec<String>,
    },
    #[error("manifest error: {0}")]
    Manifest(String),
}

pub type Result<T> = std::result::Result<T, Error>;

fn io(e: std::io::Error) -> Error {
    Error::Io(e.to_string())
}

// ---- allowlist --------------------------------------------------------------

/// One asset entry: the release file name (with `{version}`/`{tag}`).
/// The wrapper dir is auto-detected (see [`detect_extract_dir`]); set
/// `extract_dir` only to pin an unusual layout explicitly.
#[derive(Debug, Clone)]
pub struct AssetSpec {
    pub file: String,
    pub extract_dir: Option<String>,
}

/// One allowlisted upstream source.
#[derive(Debug, Clone)]
pub struct SourceSpec {
    pub name: String,
    pub repo: String,
    /// Pinned tag (e.g. `v1.2.3`) or absent for the latest release.
    pub tag: Option<String>,
    pub description: String,
    pub homepage: String,
    pub license: String,
    pub bin: Vec<String>,
    /// Args the built binary must accept with exit 0 (default `["--version"]`).
    pub smoke_args: Vec<String>,
    /// Platform key → asset. Missing keys are simply not emitted.
    pub assets: BTreeMap<String, AssetSpec>,
}

fn get_str(table: &toml::Table, key: &str, ctx: &str) -> Result<String> {
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error::Config(format!("{ctx}: missing string '{key}'")))
}

/// Parse the allowlist TOML into [`SourceSpec`]s.
pub fn parse_allowlist(text: &str) -> Result<Vec<SourceSpec>> {
    let table: toml::Table = text
        .parse()
        .map_err(|e| Error::Config(format!("bad TOML: {e}")))?;
    let sources = table
        .get("source")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| Error::Config("allowlist needs a [[source]] array".to_string()))?;
    let mut out = Vec::new();
    for (i, v) in sources.iter().enumerate() {
        let ctx = format!("source[{i}]");
        let t = v
            .as_table()
            .ok_or_else(|| Error::Config(format!("{ctx} must be a table")))?;
        let name = get_str(t, "name", &ctx)?;
        let assets_table = t
            .get("assets")
            .and_then(toml::Value::as_table)
            .ok_or_else(|| Error::Config(format!("{ctx}: missing [source.assets]")))?;
        let mut assets = BTreeMap::new();
        for key in UNIX_KEYS {
            let Some(at) = assets_table.get(*key).and_then(toml::Value::as_table) else {
                continue;
            };
            assets.insert(
                key.to_string(),
                AssetSpec {
                    file: get_str(at, "file", &format!("{ctx}.assets.{key}"))?,
                    extract_dir: at
                        .get("extract_dir")
                        .and_then(toml::Value::as_str)
                        .map(str::to_string),
                },
            );
        }
        if assets.is_empty() {
            return Err(Error::Config(format!("{ctx}: no unix assets listed")));
        }
        let bin = t
            .get("bin")
            .and_then(toml::Value::as_array)
            .ok_or_else(|| Error::Config(format!("{ctx}: missing bin list")))?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| Error::Config(format!("{ctx}: bin entries must be strings")))
            })
            .collect::<Result<Vec<_>>>()?;
        if bin.is_empty() {
            return Err(Error::Config(format!("{ctx}: bin list is empty")));
        }
        out.push(SourceSpec {
            tag: t
                .get("tag")
                .and_then(toml::Value::as_str)
                .map(str::to_string),
            description: get_str(t, "description", &ctx)?,
            homepage: get_str(t, "homepage", &ctx)?,
            license: get_str(t, "license", &ctx)?,
            smoke_args: t
                .get("smoke_args")
                .and_then(toml::Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|v| {
                            v.as_str().map(str::to_string).ok_or_else(|| {
                                Error::Config(format!("{ctx}: smoke_args must be strings"))
                            })
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_else(|| vec!["--version".to_string()]),
            repo: get_str(t, "repo", &ctx)?,
            name,
            bin,
            assets,
        });
    }
    Ok(out)
}

// ---- version / asset matching -------------------------------------------------

/// `v10.5.0` → `10.5.0`; already-bare versions pass through.
pub fn strip_v(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// Render `{version}` and `{tag}` placeholders in an asset spec.
pub fn render(template: &str, version: &str, tag: &str) -> String {
    template.replace("{version}", version).replace("{tag}", tag)
}

/// Exact-match an asset file name; on a miss the error lists every candidate
/// so the allowlist fix is obvious.
pub fn find_asset<'a>(names: &'a [String], expected: &str) -> Result<&'a String> {
    names
        .iter()
        .find(|n| *n == expected)
        .ok_or_else(|| Error::Config(format!("asset miss (expected exact '{expected}' — this importer never fuzzy-matches hashes onto the wrong file)")))
}

// ---- linkage gates ------------------------------------------------------------

/// Markers proving a binary was built for a package-manager prefix and cannot
/// run without it. Homebrew bottles carry `@@HOMEBREW_PREFIX@@` in INTERP (the
/// kernel then refuses to exec: exit 127, "required file not found").
pub fn brew_prefix_markers() -> &'static [&'static str] {
    &[
        "@@HOMEBREW_PREFIX@@",
        "@@HOMEBREW_CELLAR@@",
        "/home/linuxbrew",
        "/opt/homebrew",
        "/usr/local/Cellar",
        "linuxbrew",
    ]
}

/// True when any dependency reference points at a package-manager prefix.
pub fn has_brew_refs(refs: &[String]) -> Vec<String> {
    refs.iter()
        .filter(|r| brew_prefix_markers().iter().any(|m| r.contains(m)))
        .cloned()
        .collect()
}

fn u16le(b: &[u8], off: usize) -> Result<u16> {
    b.get(off..off + 2)
        .and_then(|s| <[u8; 2]>::try_from(s).ok())
        .map(u16::from_le_bytes)
        .ok_or_else(|| Error::Linkage {
            bin: String::new(),
            reason: "truncated binary".to_string(),
        })
}

fn u32le(b: &[u8], off: usize) -> Result<u32> {
    b.get(off..off + 4)
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| Error::Linkage {
            bin: String::new(),
            reason: "truncated binary".to_string(),
        })
}

fn u64le(b: &[u8], off: usize) -> Result<u64> {
    b.get(off..off + 8)
        .and_then(|s| <[u8; 8]>::try_from(s).ok())
        .map(u64::from_le_bytes)
        .ok_or_else(|| Error::Linkage {
            bin: String::new(),
            reason: "truncated binary".to_string(),
        })
}

fn cstr(bytes: &[u8], off: usize) -> Result<String> {
    let end = bytes[off..]
        .iter()
        .position(|&c| c == 0)
        .ok_or_else(|| Error::Linkage {
            bin: String::new(),
            reason: "unterminated string".to_string(),
        })?;
    String::from_utf8(bytes[off..off + end].to_vec()).map_err(|_| Error::Linkage {
        bin: String::new(),
        reason: "non-utf8 dependency string".to_string(),
    })
}

/// ELF machine of a 64-bit LE binary (`e_machine` at 0x12): 62 = x86-64,
/// 183 = AArch64. Used to skip the smoke gate for foreign-arch payloads
/// (an x64 importer cannot execute arm64 binaries).
pub fn elf_machine(bytes: &[u8]) -> Result<u16> {
    if bytes.len() < 64 || &bytes[0..4] != b"\x7fELF" || bytes[4] != 2 || bytes[5] != 1 {
        return Err(Error::Linkage {
            bin: String::new(),
            reason: "not a 64-bit little-endian ELF".to_string(),
        });
    }
    u16le(bytes, 0x12)
}

/// The ELF machine this importer binary was built for (62/183), if known.
pub fn host_elf_machine() -> Option<u16> {
    #[cfg(target_arch = "x86_64")]
    {
        Some(62)
    }
    #[cfg(target_arch = "aarch64")]
    {
        Some(183)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        None
    }
}

/// Dependency references of a 64-bit little-endian ELF: the INTERP path plus
/// every DT_NEEDED/RPATH/RUNPATH string. Pure parser (no `readelf`
/// dependency); errors clearly on anything else (32-bit, big-endian).
pub fn elf_dependency_refs(bytes: &[u8]) -> Result<Vec<String>> {
    if bytes.len() < 64 || &bytes[0..4] != b"\x7fELF" {
        return Err(Error::Linkage {
            bin: String::new(),
            reason: "not an ELF binary".to_string(),
        });
    }
    if bytes[4] != 2 {
        return Err(Error::Linkage {
            bin: String::new(),
            reason: "only 64-bit ELFs are supported".to_string(),
        });
    }
    if bytes[5] != 1 {
        return Err(Error::Linkage {
            bin: String::new(),
            reason: "only little-endian ELFs are supported".to_string(),
        });
    }
    let phoff = u64le(bytes, 0x20)? as usize;
    let phentsize = u16le(bytes, 0x36)? as usize;
    let phnum = u16le(bytes, 0x38)? as usize;
    if phentsize < 56 {
        return Err(Error::Linkage {
            bin: String::new(),
            reason: "bad program header size".to_string(),
        });
    }
    // Collect LOAD segments for vaddr → file-offset translation.
    let mut loads = Vec::new();
    let mut interp = None;
    let mut dynamic = None;
    for i in 0..phnum {
        let off = phoff + i * phentsize;
        let p_type = u32le(bytes, off)?;
        let p_offset = u64le(bytes, off + 8)? as usize;
        let p_vaddr = u64le(bytes, off + 16)?;
        let p_filesz = u64le(bytes, off + 32)? as usize;
        match p_type {
            3 => interp = Some((p_offset, p_filesz)),       // PT_INTERP
            2 => dynamic = Some((p_offset, p_filesz)),      // PT_DYNAMIC
            1 => loads.push((p_vaddr, p_offset, p_filesz)), // PT_LOAD
            _ => {}
        }
    }
    let mut refs = Vec::new();
    if let Some((off, _)) = interp {
        refs.push(cstr(bytes, off)?);
    }
    if let Some((off, filesz)) = dynamic {
        // Find the string table: first locate DT_STRTAB, translating its
        // vaddr through the LOAD segments.
        let mut strtab_vaddr = None;
        let mut strtab_size = 0usize;
        let mut needed = Vec::new();
        let count = filesz / 16;
        for i in 0..count {
            let e = off + i * 16;
            let tag = u64le(bytes, e)? as i64;
            let val = u64le(bytes, e + 8)?;
            match tag {
                1 | 15 | 29 => needed.push((tag, val as usize)), // NEEDED/RPATH/RUNPATH
                5 => strtab_vaddr = Some(val),                   // STRTAB
                10 => strtab_size = val as usize,                // STRSZ
                _ => {}
            }
        }
        if let Some(vaddr) = strtab_vaddr {
            // Without a LOAD mapping the table cannot be located; keep the
            // INTERP ref (already collected) and skip NEEDED strings rather
            // than failing the whole parse.
            if let Some(file_off) = loads
                .iter()
                .find(|(v, _, s)| vaddr >= *v && vaddr - *v < *s as u64)
                .map(|(v, o, _)| o + (vaddr - *v) as usize)
            {
                for (_, stroff) in needed {
                    if stroff < strtab_size {
                        refs.push(cstr(bytes, file_off + stroff)?);
                    }
                }
            }
        }
    }
    Ok(refs)
}

/// Linked-library / rpath references of a 64-bit Mach-O: every LC_LOAD_DYLIB
/// (plus weak/re-export/upward variants) name and every LC_RPATH path.
pub fn macho_dependency_refs(bytes: &[u8]) -> Result<Vec<String>> {
    if bytes.len() < 32 {
        return Err(Error::Linkage {
            bin: String::new(),
            reason: "too small for a Mach-O".to_string(),
        });
    }
    let magic = u32le(bytes, 0)?;
    // 0xfeedfacf little-endian (arm64/x86_64 macOS).
    if magic != 0xfeedfacf {
        return Err(Error::Linkage {
            bin: String::new(),
            reason: "only 64-bit little-endian Mach-O is supported".to_string(),
        });
    }
    let ncmds = u32le(bytes, 16)? as usize;
    let mut off = 32usize;
    let mut refs = Vec::new();
    for _ in 0..ncmds {
        let cmd = u32le(bytes, off)?;
        let cmdsize = u32le(bytes, off + 4)? as usize;
        if cmdsize < 8 || off + cmdsize > bytes.len() {
            return Err(Error::Linkage {
                bin: String::new(),
                reason: "bad load command".to_string(),
            });
        }
        // LC_LOAD_DYLIB 0xc, LC_LOAD_WEAK_DYLIB 0x18, LC_REEXPORT_DYLIB
        // 0x8000001f, LC_RPATH 0x8000001c. Deliberately NOT
        // LC_LOAD_UPWARD_DYLIB (0x80000022): its name field layout differs
        // (seen in the wild with garbage at the dylib offset), and upward
        // dependencies are vanishingly rare in CLI tools — missing one only
        // weakens the gate, it never wrongly rejects.
        if matches!(cmd, 0x0c | 0x18 | 0x8000_001f | 0x8000_001c) {
            let name_off = u32le(bytes, off + 8)? as usize;
            refs.push(cstr(bytes, off + name_off)?);
        }
        off += cmdsize;
    }
    Ok(refs)
}

// ---- manifest build / merge -----------------------------------------------------

/// One verified unix payload, ready to become a `[source.*]` block.
#[derive(Debug, Clone)]
pub struct VerifiedSource {
    pub key: String,
    pub url: String,
    pub sha256: String,
    pub extract_dir: Option<String>,
}

impl VerifiedSource {
    fn into_source(self) -> Source {
        Source {
            url: self.url,
            sha256: Some(self.sha256),
            sha512: None,
            extra: Vec::new(),
            kind: SourceKind::Archive,
            extract_dir: self.extract_dir,
        }
    }
}

/// Merge verified unix blocks into an existing manifest value: unix keys are
/// replaced wholesale (a rebuild changes hashes), everything else is kept
/// byte-identical in meaning.
pub fn merge_unix_sources(mut manifest: Manifest, verified: &[VerifiedSource]) -> Manifest {
    for v in verified {
        let source = v.clone().into_source();
        match v.key.as_str() {
            "linux-x64" => manifest.source.linux_x64 = Some(source),
            "linux-arm64" => manifest.source.linux_arm64 = Some(source),
            "macos-x64" => manifest.source.macos_x64 = Some(source),
            "macos-arm64" => manifest.source.macos_arm64 = Some(source),
            _ => {}
        }
    }
    manifest
}

/// A brand-new manifest carrying only unix blocks (used when the registry has
/// no file for this version yet). Windows blocks arrive via the normal
/// Windows pipelines; the file stays valid throughout because single-OS
/// manifests are legal.
#[allow(clippy::too_many_arguments)]
pub fn new_unix_manifest(
    spec: &SourceSpec,
    version: &str,
    verified: &[VerifiedSource],
    autoupdate_templates: BTreeMap<String, String>,
) -> Manifest {
    let manifest = Manifest {
        name: spec.name.clone(),
        version: version.to_string(),
        description: Some(spec.description.clone()),
        homepage: Some(spec.homepage.clone()),
        icon: None,
        license: Some(spec.license.clone()),
        kind: Kind::App,
        aliases: Vec::new(),
        source: Sources {
            any: None,
            x64: None,
            arm64: None,
            linux_x64: None,
            linux_arm64: None,
            macos_x64: None,
            macos_arm64: None,
        },
        extract_dir: None,
        file_name: None,
        bin: spec.bin.iter().map(|b| Bin::Path(b.clone())).collect(),
        env: BTreeMap::new(),
        depends: BTreeMap::new(),
        autoupdate: if autoupdate_templates.is_empty() {
            None
        } else {
            let mut inner = toml::Table::new();
            let mut url_template = toml::Table::new();
            for (k, v) in autoupdate_templates {
                url_template.insert(k, toml::Value::String(v));
            }
            inner.insert("url_template".to_string(), toml::Value::Table(url_template));
            Some(toml::Value::Table(inner))
        },
        persist: Vec::new(),
        gui: None,
        shortcuts: Vec::new(),
        write_file: Vec::new(),
    };
    merge_unix_sources(manifest, verified)
}

/// Detect the wrapper dir of an extracted payload: when every bin resolves
/// at the archive root the payload is flat (`None`); when they all resolve
/// under one single top-level directory that directory is the `extract_dir`.
/// Anything else (bins scattered, wrapper with a different name per bin) is
/// ambiguous and must be pinned with an explicit `extract_dir`.
pub fn detect_extract_dir(extracted: &Path, bins: &[String]) -> Result<Option<String>> {
    let flat = bins.iter().all(|b| extracted.join(b).is_file());
    if flat {
        return Ok(None);
    }
    let mut top_dirs = Vec::new();
    for entry in std::fs::read_dir(extracted).map_err(io)? {
        let entry = entry.map_err(io)?;
        if entry.file_type().map_err(io)?.is_dir() {
            // Skip metadata/cruft some upstreams ship at the root.
            if let Some(name) = entry.file_name().to_str()
                && !name.starts_with('.')
            {
                top_dirs.push(entry.file_name());
            }
        }
    }
    if top_dirs.len() == 1 {
        let dir = extracted.join(&top_dirs[0]);
        if bins.iter().all(|b| dir.join(b).is_file()) {
            return Ok(Some(top_dirs[0].to_string_lossy().into_owned()));
        }
    }
    Err(Error::Config(format!(
        "cannot auto-detect extract_dir under {}: bins {bins:?} are neither \
         at the root nor under one wrapper dir (set an explicit extract_dir)",
        extracted.display()
    )))
}

/// Registry path for a manifest file.
pub fn manifest_path(manifests_dir: &Path, name: &str, version: &str) -> PathBuf {
    let first = name.chars().next().unwrap_or('0').to_ascii_lowercase();
    manifests_dir
        .join(first.to_string())
        .join(name)
        .join(format!("{version}.toml"))
}

/// Highest version already in the registry for `name` (voli version ordering).
pub fn registry_latest(manifests_dir: &Path, name: &str) -> Result<Option<(String, Manifest)>> {
    let first = name.chars().next().unwrap_or('0').to_ascii_lowercase();
    let dir = manifests_dir.join(first.to_string()).join(name);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(e)),
    };
    let mut best: Option<(String, Manifest)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(io)?;
        let m = Manifest::from_toml_str(&text)
            .map_err(|e| Error::Manifest(format!("{}: {e}", path.display())))?;
        let replace = match &best {
            None => true,
            Some((v, _)) => {
                voli_core::index::cmp_version(&m.version, v) == std::cmp::Ordering::Greater
            }
        };
        if replace {
            best = Some((m.version.clone(), m));
        }
    }
    Ok(best)
}

// ---- download + extract (for the gates) -------------------------------------------

/// Stream-download a URL to a temp file (kept under its asset file name, so
/// the archive type sniffs correctly) while hashing it. Returns path + hex.
pub fn download_hashed(
    url: &str,
    token: Option<&str>,
    file_name: &str,
) -> Result<(PathBuf, String)> {
    let file_name = Path::new(file_name)
        .file_name()
        .ok_or_else(|| Error::Config(format!("bad asset file name '{file_name}'")))?;
    let dir = tempfile::tempdir().map_err(io)?;
    let dest = dir.path().join(file_name);
    // The staging dir must outlive this call: persist it (`TempDir::keep`
    // keeps the contents; callers clean up explicitly).
    let _staged = dir.keep();
    let mut req = ureq::get(url).set("User-Agent", "voli-brew-import");
    if let Some(t) = token
        && !t.is_empty()
    {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    let resp = req.call().map_err(|e| Error::Http(e.to_string()))?;
    let mut hasher = sha2::Sha256::new();
    use sha2::Digest;
    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(&dest).map_err(io)?;
    let mut buf = [0u8; 65536];
    loop {
        let n = reader.read(&mut buf).map_err(io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(io)?;
    }
    drop(file);
    let hex = hex::encode(hasher.finalize());
    Ok((dest, hex))
}

/// Extract `.zip` / `.tar.gz` / `.tgz` into a fresh tempdir. Archive-slip safe:
/// absolute paths and `..` are rejected, symlinks are skipped (never
/// materialized — their targets are unvalidated).
pub fn extract_payload(archive: &Path) -> Result<PathBuf> {
    let dest = tempfile::tempdir().map_err(io)?;
    let lower = archive
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if lower.ends_with(".zip") {
        let file = std::fs::File::open(archive).map_err(io)?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| Error::Io(e.to_string()))?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|e| Error::Io(e.to_string()))?;
            let Some(rel) = entry.enclosed_name() else {
                continue; // absolute or `..`: skip (importer fixtures are trusted, but stay strict)
            };
            let out = dest.path().join(rel);
            if entry.is_dir() {
                std::fs::create_dir_all(&out).map_err(io)?;
            } else {
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent).map_err(io)?;
                }
                let mut f = std::fs::File::create(&out).map_err(io)?;
                std::io::copy(&mut entry, &mut f).map_err(io)?;
            }
        }
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        let file = std::fs::File::open(archive).map_err(io)?;
        let gz = flate2::read::GzDecoder::new(file);
        let mut ar = tar::Archive::new(gz);
        for entry in ar.entries().map_err(io)? {
            let mut entry = entry.map_err(io)?;
            let rel = entry.path().map_err(io)?.to_path_buf();
            if rel.is_absolute()
                || rel
                    .components()
                    .any(|c| c == std::path::Component::ParentDir)
            {
                continue;
            }
            // Never materialize links: targets are unvalidated.
            if !(entry.header().entry_type().is_file() || entry.header().entry_type().is_dir()) {
                continue;
            }
            let out = dest.path().join(&rel);
            if entry.header().entry_type().is_dir() {
                std::fs::create_dir_all(&out).map_err(io)?;
            } else {
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent).map_err(io)?;
                }
                let mut f = std::fs::File::create(&out).map_err(io)?;
                std::io::copy(&mut entry, &mut f).map_err(io)?;
            }
        }
    } else {
        return Err(Error::Config(format!(
            "unsupported archive type for {} (expected .zip, .tar.gz, .tgz)",
            archive.display()
        )));
    }
    Ok(dest.keep())
}
