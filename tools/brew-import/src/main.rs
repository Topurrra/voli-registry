//! `brew-import`: upstream GitHub releases → voli manifests with unix sources.
//!
//! Usage:
//!   brew-import --sources tools/brew-sources.toml --manifests manifests/
//!   brew-import --sources tools/brew-sources.toml --manifests manifests/ --only ripgrep,fd
//!   brew-import --registry-version --static-only --only ripgrep,fd --manifests manifests/
//!   brew-import --verify-only --manifests manifests/ ripgrep fd
//!
//! The default mode imports: for each allowlisted source it resolves the
//! release, downloads every listed platform asset, runs the linkage + smoke
//! gates on the payloads it can execute on THIS machine (Linux gates here,
//! macOS gates on a Mac), and merges verified blocks into the registry.
//! macOS payloads verified anywhere except a Mac are reported as UNVERIFIED
//! and still emitted — `--verify-only` on a Mac (CI `macos-*` legs) closes
//! that gap by downloading and gating them there.
//!
//! `--static-only` keeps digest/extraction/linkage checks but never executes
//! downloaded binaries. Generation workflows use it because they hold write
//! credentials; separate read-only native verification closes the smoke gap.
//!
//! Needs network. Set `GITHUB_TOKEN` to avoid anonymous rate limits.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use brew_import::{
    Error, Result, SourceSpec, VerifiedSource, download_hashed, elf_dependency_refs,
    extract_payload, has_brew_refs, macho_dependency_refs, manifest_path, merge_unix_sources,
    new_unix_manifest, parse_allowlist, registry_latest, render, strip_v,
};
use voli_core::manifest::Manifest;

fn main() {
    if let Err(e) = run() {
        eprintln!("brew-import: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut sources_file = PathBuf::from("tools/brew-sources.toml");
    let mut sources_given = false;
    let mut manifests_dir = PathBuf::from("manifests");
    let mut only: Option<Vec<String>> = None;
    let mut verify_only = false;
    let mut registry_version = false;
    let mut static_only = false;
    let mut verify_names = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--sources" => {
                i += 1;
                sources_file = PathBuf::from(arg(&args, i, "--sources")?);
                sources_given = true;
            }
            "--manifests" => {
                i += 1;
                manifests_dir = PathBuf::from(arg(&args, i, "--manifests")?);
            }
            "--only" => {
                i += 1;
                only = Some(
                    arg(&args, i, "--only")?
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .collect(),
                );
            }
            "--verify-only" => verify_only = true,
            "--registry-version" => registry_version = true,
            "--static-only" => static_only = true,
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            other if verify_only && !other.starts_with('-') => {
                verify_names.push(other.to_string());
            }
            other => return Err(Error::Config(format!("unknown argument '{other}'"))),
        }
        i += 1;
    }

    if static_only && verify_only {
        return Err(Error::Config(
            "--static-only cannot be combined with --verify-only".to_string(),
        ));
    }
    if registry_version && (verify_only || only.is_none()) {
        return Err(Error::Config(
            "--registry-version requires --only and cannot be combined with --verify-only"
                .to_string(),
        ));
    }
    if verify_only {
        // Smoke args come from the allowlist when it names the package;
        // otherwise the `--version` convention applies.
        let specs = if sources_given || sources_file.is_file() {
            parse_allowlist(&std::fs::read_to_string(&sources_file).map_err(|e| {
                Error::Config(format!("cannot read {}: {e}", sources_file.display()))
            })?)?
        } else {
            Vec::new()
        };
        return verify_only_mode(&manifests_dir, &verify_names, &specs);
    }

    let text = std::fs::read_to_string(&sources_file)
        .map_err(|e| Error::Config(format!("cannot read {}: {e}", sources_file.display())))?;
    let mut specs = parse_allowlist(&text)?;
    if let Some(only) = only {
        for name in &only {
            if !specs.iter().any(|spec| &spec.name == name) {
                return Err(Error::Config(format!(
                    "--only: '{name}' is not allowlisted"
                )));
            }
        }
        specs.retain(|s| only.contains(&s.name));
        if specs.is_empty() {
            return Err(Error::Config(
                "--only matched no allowlisted source".to_string(),
            ));
        }
    }
    let token = std::env::var("GITHUB_TOKEN").ok();
    for spec in &specs {
        import_one(
            spec,
            &manifests_dir,
            token.as_deref(),
            registry_version,
            static_only,
        )?;
    }
    println!("brew-import: {} source(s) done", specs.len());
    Ok(())
}

fn arg(args: &[String], i: usize, flag: &str) -> Result<String> {
    args.get(i)
        .cloned()
        .ok_or_else(|| Error::Config(format!("{flag} needs a value")))
}

fn print_help() {
    println!(
        "brew-import: upstream GitHub releases -> voli manifests (unix sources)\n\
         \n\
         import: brew-import --sources <allowlist> --manifests <dir> [--only a,b]\n\
         enrich existing registry versions: add --registry-version --only a,b\n\
         skip execution in privileged generation jobs: add --static-only\n\
         verify host payloads of existing manifests:\n\
         ·       brew-import --verify-only [--sources <allowlist>] --manifests <dir> <name>..."
    );
}

// ---- import -------------------------------------------------------------------

struct Release {
    tag: String,
    assets: Vec<ReleaseAsset>,
}

struct ReleaseAsset {
    name: String,
    url: String,
    digest: Option<String>,
}

// Only a genuine 404 permits trying the other exact spelling; rate limits,
// authentication and transport errors must not silently change the target.
fn fetch_release(repo: &str, tag: Option<&str>, token: Option<&str>) -> Result<Option<Release>> {
    let url = match tag {
        Some(t) => format!("https://api.github.com/repos/{repo}/releases/tags/{t}"),
        None => format!("https://api.github.com/repos/{repo}/releases/latest"),
    };
    let mut req = ureq::get(&url)
        .set("User-Agent", "voli-brew-import")
        .set("Accept", "application/vnd.github+json");
    if let Some(t) = token
        && !t.is_empty()
    {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    let response = match req.call() {
        Ok(response) => response,
        Err(ureq::Error::Status(404, _)) => return Ok(None),
        Err(error) => return Err(Error::Http(format!("release metadata for {repo}: {error}"))),
    };
    let text = response
        .into_string()
        .map_err(|e| Error::Http(e.to_string()))?;
    parse_release_metadata(&text).map(Some)
}

fn parse_release_metadata(text: &str) -> Result<Release> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| Error::Http(format!("bad release JSON: {e}")))?;
    let tag = v["tag_name"]
        .as_str()
        .ok_or_else(|| Error::Http("release has no tag_name".to_string()))?
        .to_string();
    let assets = v["assets"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|a| {
            Some(ReleaseAsset {
                name: a["name"].as_str()?.to_string(),
                url: a["browser_download_url"].as_str()?.to_string(),
                digest: a["digest"].as_str().map(str::to_string),
            })
        })
        .collect();
    Ok(Release { tag, assets })
}

fn resolve_exact_release(
    version: &str,
    pinned: Option<&str>,
    mut fetch: impl FnMut(&str) -> Result<Option<Release>>,
) -> Result<Release> {
    if version.is_empty()
        || version.contains("..")
        || !version.as_bytes()[0].is_ascii_alphanumeric()
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
    {
        return Err(Error::Config(format!(
            "unsafe registry version '{version}'"
        )));
    }
    let mut tags = Vec::new();
    if let Some(tag) = pinned.filter(|tag| strip_v(tag) == version) {
        tags.push(tag.to_string());
    }
    for tag in [format!("v{version}"), version.to_string()] {
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    for tag in &tags {
        if let Some(release) = fetch(tag)? {
            if strip_v(&release.tag) != version {
                return Err(Error::Http(format!(
                    "release tag '{}' does not match requested registry version '{version}'",
                    release.tag
                )));
            }
            return Ok(release);
        }
    }
    Err(Error::Http(format!(
        "no release for exact registry version '{version}' (tried {})",
        tags.join(", ")
    )))
}

fn registry_import_version(manifests_dir: &Path, name: &str) -> Result<String> {
    let Some((version, manifest)) = registry_latest(manifests_dir, name)? else {
        return Err(Error::Config(format!("no manifests for '{name}'")));
    };
    if manifest.name != name || !manifest_path(manifests_dir, name, &version).is_file() {
        return Err(Error::Manifest(format!(
            "registry latest for '{name}' has inconsistent name/version layout"
        )));
    }
    Ok(version)
}

fn asset_sha256(asset: &ReleaseAsset) -> Result<&str> {
    let hash = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"));
    match hash {
        Some(hash) if hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => Ok(hash),
        _ => Err(Error::Http(format!(
            "{} has no valid official GitHub SHA-256 digest",
            asset.name
        ))),
    }
}

fn verify_asset_digest(asset: &ReleaseAsset, actual: &str) -> Result<()> {
    let expected = asset_sha256(asset)?;
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(Error::Http(format!(
            "hash mismatch for {}: GitHub says {expected}, download is {actual}",
            asset.name
        )));
    }
    Ok(())
}

// Resolve every declared platform and its authoritative digest before any
// payload executes. A missing platform is a failed update, never a skip.
fn selected_assets<'a>(
    spec: &'a SourceSpec,
    version: &str,
    release: &'a Release,
) -> Result<Vec<(&'a String, &'a ReleaseAsset)>> {
    let names: Vec<String> = release
        .assets
        .iter()
        .map(|asset| asset.name.clone())
        .collect();
    let mut selected = Vec::new();
    for (key, asset_spec) in &spec.assets {
        let file = render(&asset_spec.file, version, &release.tag);
        brew_import::find_asset(&names, &file).map_err(|_| {
            let mut sorted = names.clone();
            sorted.sort();
            Error::AssetMissing {
                repo: spec.repo.clone(),
                tag: release.tag.clone(),
                expected: file.clone(),
                candidates: sorted.join("\n"),
            }
        })?;
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == file)
            .expect("find_asset matched above");
        asset_sha256(asset)?;
        selected.push((key, asset));
    }
    Ok(selected)
}

fn import_one(
    spec: &SourceSpec,
    manifests_dir: &Path,
    token: Option<&str>,
    registry_version: bool,
    static_only: bool,
) -> Result<()> {
    let release = if registry_version {
        let version = registry_import_version(manifests_dir, &spec.name)?;
        resolve_exact_release(&version, spec.tag.as_deref(), |tag| {
            fetch_release(&spec.repo, Some(tag), token)
        })?
    } else {
        fetch_release(&spec.repo, spec.tag.as_deref(), token)?
            .ok_or_else(|| Error::Http(format!("release not found for {}", spec.repo)))?
    };
    import_release(spec, manifests_dir, release, static_only)
}

fn import_release(
    spec: &SourceSpec,
    manifests_dir: &Path,
    release: Release,
    static_only: bool,
) -> Result<()> {
    let version = strip_v(&release.tag).to_string();
    println!("{}: {} ({})", spec.name, version, release.tag);

    let mut verified = Vec::new();
    let mut unverified: Vec<(String, String)> = Vec::new();
    for (key, asset) in selected_assets(spec, &version, &release)? {
        let asset_spec = &spec.assets[key];
        println!("  [{key}] {}", asset.name);
        let (archive, sha256) = download_hashed(&asset.url, None, &asset.name)?;
        // Check the official digest BEFORE extracting or executing anything.
        verify_asset_digest(asset, &sha256)?;
        println!("    sha256 {sha256} (GitHub digest verified)");
        let override_dir = asset_spec
            .extract_dir
            .as_ref()
            .map(|d| render(d, &version, &release.tag));
        let extract_dir = gate_payload(
            spec,
            key,
            &archive,
            override_dir,
            &mut unverified,
            static_only,
        )?;
        if let Some(d) = &extract_dir {
            println!("    extract_dir {d}");
        }
        verified.push(VerifiedSource {
            key: key.clone(),
            url: asset_url_with_fragment(&asset.url),
            sha256,
            extract_dir,
        });
    }

    write_release_manifest(manifests_dir, spec, &version, &verified)?;
    if !unverified.is_empty() {
        println!("  UNVERIFIED (hash + static checks only — needs a matching host):");
        for (key, reason) in &unverified {
            println!("    [{key}] {reason}; run --verify-only on matching CI to close");
        }
    }
    Ok(())
}

fn write_release_manifest(
    manifests_dir: &Path,
    spec: &SourceSpec,
    version: &str,
    verified: &[VerifiedSource],
) -> Result<()> {
    for key in spec.assets.keys() {
        if verified.iter().filter(|source| &source.key == key).count() != 1 {
            return Err(Error::Config(format!(
                "{} {version}: expected exactly one verified [{key}] source",
                spec.name
            )));
        }
    }
    if verified
        .iter()
        .any(|source| !spec.assets.contains_key(&source.key))
    {
        return Err(Error::Config(
            "verified source is not allowlisted".to_string(),
        ));
    }
    // A release's payloads belong only to its exact version. A pinned older
    // tag must never be merged into the newest Windows manifest.
    let path = manifest_path(manifests_dir, &spec.name, version);
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => {
            let manifest = Manifest::from_toml_str(&text)
                .map_err(|e| Error::Manifest(format!("{}: {e}", path.display())))?;
            if manifest.name != spec.name || manifest.version != version {
                return Err(Error::Manifest(format!(
                    "{} does not match {} {version}",
                    path.display(),
                    spec.name
                )));
            }
            Some(manifest)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(Error::Io(format!("cannot read {}: {e}", path.display()))),
    };
    if let Some(manifest) = existing {
        for (key, source) in [
            ("linux-x64", &manifest.source.linux_x64),
            ("linux-arm64", &manifest.source.linux_arm64),
            ("macos-x64", &manifest.source.macos_x64),
            ("macos-arm64", &manifest.source.macos_arm64),
        ] {
            if source.is_some() && !spec.assets.contains_key(key) {
                return Err(Error::Config(format!(
                    "{} {version}: refusing to retain unverified [{key}] source",
                    spec.name
                )));
            }
        }
        let merged = merge_unix_sources(manifest, verified);
        write_manifest_file(manifests_dir, &merged)?;
        println!("  merged unix blocks into {}", merged.version);
    } else {
        let mut templates = BTreeMap::new();
        for (key, asset_spec) in &spec.assets {
            // Future `bump` support: per-platform url templates.
            templates.insert(key.clone(), asset_spec.file.clone());
        }
        let manifest = new_unix_manifest(spec, version, verified, templates);
        write_manifest_file(manifests_dir, &manifest)?;
        println!("  new file for {version} (unix blocks only)");
    }
    Ok(())
}

/// The download URL as stored: plain URL (the `#/name` rename fragment is only
/// for `kind = "binary"` payloads, which the importer does not emit).
fn asset_url_with_fragment(url: &str) -> String {
    url.to_string()
}

fn write_manifest_file(manifests_dir: &Path, manifest: &Manifest) -> Result<()> {
    let text = manifest.to_canonical_toml();
    // Re-parse: the round-trip is the validation (hashes, layout, schema).
    Manifest::from_toml_str(&text)
        .map_err(|e| Error::Manifest(format!("emitted manifest rejected: {e}")))?;
    let path = manifest_path(manifests_dir, &manifest.name, &manifest.version);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Io(format!("cannot create {}: {e}", parent.display())))?;
    }
    std::fs::write(&path, text)
        .map_err(|e| Error::Io(format!("cannot write {}: {e}", path.display())))?;
    println!("  wrote {}", path.display());
    Ok(())
}

// ---- gates ----------------------------------------------------------------------

/// Linkage + smoke gates for one downloaded payload, returning the effective
/// wrapper dir (explicit override wins, else auto-detected from the layout).
/// Unix bins must be executable here (or on a Mac for macOS payloads) — that
/// is the entire proof a Homebrew bottle could never give.
fn gate_payload(
    spec: &SourceSpec,
    key: &str,
    archive: &Path,
    override_dir: Option<String>,
    unverified: &mut Vec<(String, String)>,
    static_only: bool,
) -> Result<Option<String>> {
    use brew_import::detect_extract_dir;

    let is_mac = key.starts_with("macos-");
    let dest = extract_payload(archive)?;
    let extract_dir = match override_dir {
        Some(d) => {
            if !dest.join(&d).is_dir() {
                return Err(Error::Config(format!(
                    "extract_dir '{d}' not found in {}",
                    archive.display()
                )));
            }
            Some(d)
        }
        None => detect_extract_dir(&dest, &spec.bin)?,
    };
    let root = match &extract_dir {
        Some(d) => dest.join(d),
        None => dest.clone(),
    };
    for bin in &spec.bin {
        let path = root.join(bin);
        let meta = std::fs::metadata(&path).map_err(|_| {
            Error::Config(format!(
                "bin '{bin}' not found under '{}' in {}",
                extract_dir.as_deref().unwrap_or("."),
                archive.display()
            ))
        })?;
        if !meta.is_file() {
            return Err(Error::Config(format!("bin '{bin}' is not a file")));
        }
        match gate_binary(&path, bin, &spec.smoke_args, is_mac, static_only)? {
            GateOutcome::Verified => {}
            GateOutcome::StaticOnly(reason) => {
                unverified.push((format!("{key}:{bin}"), reason));
            }
        }
    }
    // Keep tempdirs tidy; failures above propagate first.
    let _ = std::fs::remove_dir_all(dest);
    Ok(extract_dir)
}

/// What the gates established for one binary.
#[derive(Debug, PartialEq, Eq)]
enum GateOutcome {
    /// Static checks passed and the binary executed with exit 0 here.
    Verified,
    /// Static checks passed but execution was disabled or impossible on this
    /// host (wrong OS or foreign arch). The block is emitted with its hash;
    /// a matching CI leg must run `--verify-only` over it.
    StaticOnly(String),
}

/// Static linkage check + live smoke run for one binary.
fn gate_binary(
    path: &Path,
    bin: &str,
    smoke_args: &[String],
    is_mac: bool,
    static_only: bool,
) -> Result<GateOutcome> {
    use brew_import::{elf_machine, host_elf_machine};

    let bytes = std::fs::read(path)
        .map_err(|e| Error::Io(format!("cannot read {}: {e}", path.display())))?;
    if !is_mac {
        // Foreign-arch ELFs cannot execute here; static checks still apply.
        if let (Ok(machine), Some(host)) = (elf_machine(&bytes), host_elf_machine())
            && machine != host
        {
            let refs = elf_dependency_refs(&bytes).map_err(|e| with_bin(e, bin))?;
            let bad = has_brew_refs(&refs);
            if !bad.is_empty() {
                return Err(Error::Linkage {
                    bin: bin.to_string(),
                    reason: format!("package-manager prefix references: {}", bad.join(", ")),
                });
            }
            return Ok(GateOutcome::StaticOnly(format!(
                "foreign-arch ELF (machine {machine}); smoke needs a native host"
            )));
        }
    }
    let refs = if is_mac {
        macho_dependency_refs(&bytes).map_err(|e| with_bin(e, bin))?
    } else {
        elf_dependency_refs(&bytes).map_err(|e| with_bin(e, bin))?
    };
    let bad = has_brew_refs(&refs);
    if !bad.is_empty() {
        return Err(Error::Linkage {
            bin: bin.to_string(),
            reason: format!("package-manager prefix references: {}", bad.join(", ")),
        });
    }
    if is_mac && !cfg!(target_os = "macos") {
        // Mach-O parses anywhere, but only a Mac can execute it.
        return Ok(GateOutcome::StaticOnly(
            "not a Mac host; smoke needs macOS".to_string(),
        ));
    }
    if static_only {
        return Ok(GateOutcome::StaticOnly(
            "--static-only: execution deferred to read-only native verification".to_string(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)
            .map_err(|e| Error::Io(e.to_string()))?
            .permissions();
        perms.set_mode(perms.mode() | 0o111);
        std::fs::set_permissions(path, perms).map_err(|e| Error::Io(e.to_string()))?;
    }
    let out = std::process::Command::new(path)
        .args(smoke_args)
        .output()
        .map_err(|e| Error::Io(format!("cannot execute {}: {e}", path.display())))?;
    if !out.status.success() {
        return Err(Error::Smoke {
            bin: bin.to_string(),
            code: out
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            args: smoke_args.to_vec(),
        });
    }
    println!(
        "    smoke OK: {} {}",
        bin,
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
    );
    Ok(GateOutcome::Verified)
}

fn with_bin(e: Error, bin: &str) -> Error {
    match e {
        Error::Linkage { reason, .. } => Error::Linkage {
            bin: bin.to_string(),
            reason,
        },
        other => other,
    }
}

// ---- verify-only ------------------------------------------------------------------

/// Some packages expose PATH instead of declaring shims. Their allowlist
/// still names binaries for the smoke gate; never report an empty gate green.
fn verification_bins<'a>(
    manifest: &'a Manifest,
    spec: Option<&'a SourceSpec>,
) -> Result<Vec<&'a str>> {
    let bins: Vec<&str> = if manifest.bin.is_empty() {
        spec.map(|spec| spec.bin.iter().map(String::as_str).collect())
            .unwrap_or_default()
    } else {
        manifest.bin.iter().map(|bin| bin.path()).collect()
    };
    if bins.is_empty() {
        return Err(Error::Config(format!(
            "{} {} has no binaries to verify (set bin in the manifest or allowlist)",
            manifest.name, manifest.version
        )));
    }
    Ok(bins)
}

/// Download each named manifest's HOST-platform payload and run the gates over
/// it. No writes. CI runs this on ubuntu + macos legs so every emitted block
/// is executed somewhere.
fn verify_only_mode(manifests_dir: &Path, names: &[String], specs: &[SourceSpec]) -> Result<()> {
    if names.is_empty() {
        return Err(Error::Config(
            "--verify-only needs at least one package name".to_string(),
        ));
    }
    let host = voli_core::manifest::Platform::host();
    let key = host.to_string();
    for name in names {
        let Some((version, manifest)) = registry_latest(manifests_dir, name)? else {
            return Err(Error::Config(format!("no manifests for '{name}'")));
        };
        let spec = specs.iter().find(|s| &s.name == name);
        let source = match manifest.source.for_platform(host) {
            Some(source) => source,
            None if spec.is_some_and(|spec| !spec.assets.contains_key(&key)) => {
                println!("{name} {version} [{key}]: not supported by the allowlist; skipped");
                continue;
            }
            None => {
                return Err(Error::Config(format!(
                    "{name} {version} has no [{key}] block"
                )));
            }
        };
        let bins = verification_bins(&manifest, spec)?;
        println!("{name} {version} [{key}]: {}", source.url);
        let asset_name = source.url.rsplit('/').next().unwrap_or("payload");
        let (archive, sha) = download_hashed(&source.url, None, asset_name)?;
        if !sha.eq_ignore_ascii_case(source.hash()) {
            return Err(Error::Http(format!(
                "hash mismatch for {name}: index says {}, download is {sha}",
                source.hash()
            )));
        }
        println!("  sha256 OK");
        let dest = extract_payload(&archive)?;
        let root = match &source.extract_dir {
            Some(d) => dest.join(d),
            None => dest.clone(),
        };
        let smoke_args = spec
            .map(|s| s.smoke_args.clone())
            .unwrap_or_else(|| vec!["--version".to_string()]);
        for bin in bins {
            // Same resolution the install engine uses (unix payloads are
            // extensionless where Windows-first manifests name `.exe`).
            let path = voli_core::resolve_bin_target(&root, bin);
            if !path.is_file() {
                return Err(Error::Config(format!(
                    "bin '{}' resolves to {}, which is not a file",
                    bin,
                    path.display()
                )));
            }
            match gate_binary(&path, bin, &smoke_args, cfg!(target_os = "macos"), false)? {
                GateOutcome::Verified => {}
                GateOutcome::StaticOnly(reason) => {
                    return Err(Error::Config(format!(
                        "host payload for {name} could not be executed here: {reason}"
                    )));
                }
            }
        }
        let _ = std::fs::remove_dir_all(dest);
        println!("  {name}: host payload verified");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use brew_import::AssetSpec;

    fn windows_only_registry() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = manifest_path(dir.path(), "tool", "1.0.0");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!(
            "name = \"tool\"\nversion = \"1.0.0\"\nkind = \"app\"\nbin = [\"tool\"]\n\n[source.x64]\nurl = \"https://example.com/tool.zip\"\nsha256 = \"{}\"\n",
            "a".repeat(64)
        )).unwrap();
        dir
    }

    fn spec(platform: &str) -> SourceSpec {
        SourceSpec {
            name: "tool".to_string(),
            repo: "example/tool".to_string(),
            tag: None,
            description: String::new(),
            homepage: String::new(),
            license: "MIT".to_string(),
            bin: vec!["tool".to_string()],
            smoke_args: vec!["--version".to_string()],
            assets: BTreeMap::from([(
                platform.to_string(),
                AssetSpec {
                    file: "tool.tar.gz".to_string(),
                    extract_dir: None,
                },
            )]),
        }
    }

    #[test]
    fn pinned_older_release_never_overwrites_newer_manifest() {
        let dir = windows_only_registry();
        let (_, older) = registry_latest(dir.path(), "tool").unwrap().unwrap();
        let mut newer = older.clone();
        newer.version = "2.0.0".to_string();
        let newer_path = manifest_path(dir.path(), "tool", "2.0.0");
        std::fs::write(&newer_path, newer.to_canonical_toml()).unwrap();
        let before = std::fs::read(&newer_path).unwrap();
        let sources = vec![VerifiedSource {
            key: "linux-x64".to_string(),
            url: "https://example.com/tool-1.0.0.tar.gz".to_string(),
            sha256: "b".repeat(64),
            extract_dir: None,
        }];
        write_release_manifest(dir.path(), &spec("linux-x64"), "1.0.0", &sources).unwrap();
        assert_eq!(std::fs::read(newer_path).unwrap(), before);
        let updated = Manifest::from_toml_str(
            &std::fs::read_to_string(manifest_path(dir.path(), "tool", "1.0.0")).unwrap(),
        )
        .unwrap();
        assert_eq!(updated.version, "1.0.0");
        assert_eq!(updated.source.x64, older.source.x64);
        assert_eq!(updated.source.linux_x64.unwrap().url, sources[0].url);
    }

    #[test]
    fn absent_older_release_is_created_at_its_own_version() {
        let dir = windows_only_registry();
        let newer_path = manifest_path(dir.path(), "tool", "1.0.0");
        let before = std::fs::read(&newer_path).unwrap();
        let sources = vec![VerifiedSource {
            key: "linux-x64".to_string(),
            url: "https://example.com/tool-0.9.0.tar.gz".to_string(),
            sha256: "b".repeat(64),
            extract_dir: None,
        }];
        write_release_manifest(dir.path(), &spec("linux-x64"), "0.9.0", &sources).unwrap();
        assert_eq!(std::fs::read(newer_path).unwrap(), before);
        let created = Manifest::from_toml_str(
            &std::fs::read_to_string(manifest_path(dir.path(), "tool", "0.9.0")).unwrap(),
        )
        .unwrap();
        assert_eq!(created.version, "0.9.0");
        assert_eq!(created.source.linux_x64.unwrap().url, sources[0].url);
    }

    #[test]
    fn exact_release_tries_only_version_tags_and_ignores_stale_pin() {
        let mut requested = Vec::new();
        let release = resolve_exact_release("1.0.0", Some("v0.9.0"), |tag| {
            requested.push(tag.to_string());
            Ok((tag == "1.0.0").then(|| Release {
                tag: tag.to_string(),
                assets: vec![],
            }))
        })
        .unwrap();
        assert_eq!(requested, ["v1.0.0", "1.0.0"]);
        assert_eq!(release.tag, "1.0.0");
    }

    #[test]
    fn exact_release_uses_matching_pinned_bare_tag_first() {
        let mut requested = Vec::new();
        resolve_exact_release("1.0.0", Some("1.0.0"), |tag| {
            requested.push(tag.to_string());
            Ok(Some(Release {
                tag: tag.to_string(),
                assets: vec![],
            }))
        })
        .unwrap();
        assert_eq!(requested, ["1.0.0"]);
    }

    #[test]
    fn exact_release_rejects_returned_tag_mismatch_without_fallback() {
        let mut requested = Vec::new();
        let result = resolve_exact_release("1.0.0", None, |tag| {
            requested.push(tag.to_string());
            Ok(Some(Release {
                tag: "v2.0.0".to_string(),
                assets: vec![],
            }))
        });
        assert!(matches!(result, Err(Error::Http(message)) if message.contains("does not match")));
        assert_eq!(requested, ["v1.0.0"]);
    }

    #[test]
    fn exact_release_does_not_fallback_after_api_errors() {
        let mut requested = Vec::new();
        let result = resolve_exact_release("1.0.0", None, |tag| {
            requested.push(tag.to_string());
            Err(Error::Http("rate limited".to_string()))
        });
        assert!(result.is_err());
        assert_eq!(requested, ["v1.0.0"]);
    }

    #[test]
    fn exact_release_missing_tags_never_requests_latest() {
        let mut requested = Vec::new();
        let result = resolve_exact_release("1.0.0", None, |tag| {
            requested.push(tag.to_string());
            Ok(None)
        });
        assert!(result.is_err());
        assert_eq!(requested, ["v1.0.0", "1.0.0"]);
    }

    #[test]
    fn exact_release_rejects_unsafe_registry_version_before_fetching() {
        let result = resolve_exact_release("../latest", None, |_| {
            panic!("unsafe version must not be requested")
        });
        assert!(matches!(result, Err(Error::Config(_))));
    }

    #[test]
    fn registry_import_uses_existing_latest_version() {
        let dir = windows_only_registry();
        let (_, mut newer) = registry_latest(dir.path(), "tool").unwrap().unwrap();
        newer.version = "2.0.0".to_string();
        std::fs::write(
            manifest_path(dir.path(), "tool", "2.0.0"),
            newer.to_canonical_toml(),
        )
        .unwrap();
        assert_eq!(
            registry_import_version(dir.path(), "tool").unwrap(),
            "2.0.0"
        );
        assert!(registry_import_version(dir.path(), "missing").is_err());
    }

    #[test]
    fn official_asset_digest_is_required_and_checked() {
        let release = parse_release_metadata(&serde_json::json!({
            "tag_name": "v1.0.0",
            "assets": [{"name": "tool.tar.gz", "browser_download_url": "https://example.com/tool.tar.gz",
                        "digest": format!("sha256:{}", "a".repeat(64))}]
        }).to_string()).unwrap();
        let asset = &release.assets[0];
        verify_asset_digest(asset, &"a".repeat(64)).unwrap();
        assert!(verify_asset_digest(asset, &"b".repeat(64)).is_err());
        for digest in [
            None,
            Some("sha512:aaaa".to_string()),
            Some("sha256:bad".to_string()),
        ] {
            let asset = ReleaseAsset {
                name: "tool.tar.gz".to_string(),
                url: String::new(),
                digest,
            };
            assert!(verify_asset_digest(&asset, &"a".repeat(64)).is_err());
        }
    }

    #[test]
    fn static_only_checks_native_binary_without_executing_it() {
        let native_binary = std::env::current_exe().unwrap();
        let args = vec!["--voli-smoke-must-not-run".to_string()];
        // This executable would reject the supplied flag if it were run.
        let outcome = gate_binary(
            &native_binary,
            "fixture",
            &args,
            cfg!(target_os = "macos"),
            true,
        )
        .unwrap();
        assert!(
            matches!(outcome, GateOutcome::StaticOnly(reason) if reason.contains("static-only"))
        );
        assert!(matches!(
            gate_binary(
                &native_binary,
                "fixture",
                &args,
                cfg!(target_os = "macos"),
                false
            ),
            Err(Error::Smoke { .. })
        ));
    }

    #[test]
    fn static_only_still_rejects_invalid_native_payload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-native-binary");
        std::fs::write(&path, b"not an ELF or Mach-O payload").unwrap();
        assert!(gate_binary(&path, "fixture", &[], cfg!(target_os = "macos"), true).is_err());
    }

    #[test]
    fn mismatched_digest_is_rejected_before_archive_extraction_or_smoke() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/tool.tar.gz", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            stream.read(&mut request).unwrap();
            // Invalid archive bytes distinguish the digest check from the
            // extraction/linkage/smoke gate if their order ever regresses.
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nnot archive").unwrap();
        });
        let dir = windows_only_registry();
        let path = manifest_path(dir.path(), "tool", "1.0.0");
        let before = std::fs::read(&path).unwrap();
        let release = Release {
            tag: "v1.0.0".to_string(),
            assets: vec![ReleaseAsset {
                name: "tool.tar.gz".to_string(),
                url,
                digest: Some(format!("sha256:{}", "a".repeat(64))),
            }],
        };
        let error = import_release(&spec("linux-x64"), dir.path(), release, false).unwrap_err();
        server.join().unwrap();
        assert!(matches!(error, Error::Http(message) if message.contains("hash mismatch")));
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn missing_declared_release_asset_fails_before_download() {
        let release = Release {
            tag: "v1.0.0".to_string(),
            assets: vec![],
        };
        assert!(matches!(
            selected_assets(&spec("linux-x64"), "1.0.0", &release),
            Err(Error::AssetMissing { .. })
        ));
    }

    #[test]
    fn refuses_incomplete_verified_sources_without_mutating_manifest() {
        let dir = windows_only_registry();
        let path = manifest_path(dir.path(), "tool", "1.0.0");
        let before = std::fs::read(&path).unwrap();
        let error =
            write_release_manifest(dir.path(), &spec("linux-x64"), "1.0.0", &[]).unwrap_err();
        assert!(matches!(error, Error::Config(message) if message.contains("linux-x64")));
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn refuses_to_carry_forward_unverified_unix_platform() {
        let dir = windows_only_registry();
        let path = manifest_path(dir.path(), "tool", "1.0.0");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(&format!(
            "\n[source.macos-arm64]\nurl = \"https://example.com/old-mac.tar.gz\"\nsha256 = \"{}\"\n",
            "c".repeat(64)
        ));
        std::fs::write(&path, text).unwrap();
        let before = std::fs::read(&path).unwrap();
        let sources = [VerifiedSource {
            key: "linux-x64".to_string(),
            url: "https://example.com/tool.tar.gz".to_string(),
            sha256: "b".repeat(64),
            extract_dir: None,
        }];
        let error =
            write_release_manifest(dir.path(), &spec("linux-x64"), "1.0.0", &sources).unwrap_err();
        assert!(matches!(error, Error::Config(message) if message.contains("macos-arm64")));
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn newer_release_does_not_reuse_older_windows_metadata() {
        let dir = windows_only_registry();
        let older_path = manifest_path(dir.path(), "tool", "1.0.0");
        let before = std::fs::read(&older_path).unwrap();
        let sources = [VerifiedSource {
            key: "linux-x64".to_string(),
            url: "https://example.com/tool-2.0.0.tar.gz".to_string(),
            sha256: "b".repeat(64),
            extract_dir: None,
        }];
        write_release_manifest(dir.path(), &spec("linux-x64"), "2.0.0", &sources).unwrap();
        let newer = Manifest::from_toml_str(
            &std::fs::read_to_string(manifest_path(dir.path(), "tool", "2.0.0")).unwrap(),
        )
        .unwrap();
        assert!(newer.source.x64.is_none());
        assert_eq!(newer.version, "2.0.0");
        assert_eq!(std::fs::read(older_path).unwrap(), before);
    }

    #[test]
    fn verify_uses_allowlist_binaries_when_manifest_has_none() {
        let dir = windows_only_registry();
        let (_, mut manifest) = registry_latest(dir.path(), "tool").unwrap().unwrap();
        manifest.bin.clear();
        let spec = spec("linux-x64");
        assert_eq!(
            verification_bins(&manifest, Some(&spec)).unwrap(),
            vec!["tool"]
        );
    }

    #[test]
    fn verify_rejects_empty_binary_lists() {
        let dir = windows_only_registry();
        let (_, mut manifest) = registry_latest(dir.path(), "tool").unwrap().unwrap();
        manifest.bin.clear();
        assert!(matches!(
            verification_bins(&manifest, None),
            Err(Error::Config(_))
        ));
        let mut spec = spec("linux-x64");
        spec.bin.clear();
        assert!(matches!(
            verification_bins(&manifest, Some(&spec)),
            Err(Error::Config(_))
        ));
    }

    #[test]
    fn verify_keeps_manifest_binary_targets_when_present() {
        let dir = windows_only_registry();
        let (_, manifest) = registry_latest(dir.path(), "tool").unwrap().unwrap();
        let mut spec = spec("linux-x64");
        spec.bin = vec!["different-allowlist-tool".to_string()];
        assert_eq!(
            verification_bins(&manifest, Some(&spec)).unwrap(),
            vec!["tool"]
        );
    }

    #[test]
    fn verify_skips_only_explicitly_unsupported_host_platform() {
        let dir = windows_only_registry();
        let host = voli_core::manifest::Platform::host().to_string();
        let unsupported = if host == "linux-x64" {
            "macos-arm64"
        } else {
            "linux-x64"
        };
        verify_only_mode(dir.path(), &["tool".to_string()], &[spec(unsupported)]).unwrap();
    }

    #[test]
    fn verify_rejects_declared_but_missing_host_platform() {
        let dir = windows_only_registry();
        let host = voli_core::manifest::Platform::host().to_string();
        let err = verify_only_mode(dir.path(), &["tool".to_string()], &[spec(&host)]).unwrap_err();
        assert!(
            matches!(err, Error::Config(message) if message.contains(&format!("has no [{host}] block")))
        );
    }

    #[test]
    fn verify_without_allowlist_still_rejects_missing_host_platform() {
        let dir = windows_only_registry();
        let host = voli_core::manifest::Platform::host().to_string();
        let err = verify_only_mode(dir.path(), &["tool".to_string()], &[]).unwrap_err();
        assert!(
            matches!(err, Error::Config(message) if message.contains(&format!("has no [{host}] block")))
        );
    }
}
