//! `brew-import`: upstream GitHub releases → voli manifests with unix sources.
//!
//! Usage:
//!   brew-import --sources tools/brew-sources.toml --manifests manifests/
//!   brew-import --sources tools/brew-sources.toml --manifests manifests/ --only ripgrep,fd
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
        specs.retain(|s| only.contains(&s.name));
        if specs.is_empty() {
            return Err(Error::Config(
                "--only matched no allowlisted source".to_string(),
            ));
        }
    }
    let token = std::env::var("GITHUB_TOKEN").ok();
    for spec in &specs {
        import_one(spec, &manifests_dir, token.as_deref())?;
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
         verify host payloads of existing manifests:\n\
         ·       brew-import --verify-only [--sources <allowlist>] --manifests <dir> <name>..."
    );
}

// ---- import -------------------------------------------------------------------

struct Release {
    tag: String,
    assets: Vec<(String, String)>,
}

fn fetch_release(repo: &str, tag: Option<&str>, token: Option<&str>) -> Result<Release> {
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
    let text = req
        .call()
        .map_err(|e| Error::Http(format!("release metadata for {repo}: {e}")))?
        .into_string()
        .map_err(|e| Error::Http(e.to_string()))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| Error::Http(format!("bad release JSON: {e}")))?;
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
            Some((
                a["name"].as_str()?.to_string(),
                a["browser_download_url"].as_str()?.to_string(),
            ))
        })
        .collect();
    Ok(Release { tag, assets })
}

fn import_one(spec: &SourceSpec, manifests_dir: &Path, token: Option<&str>) -> Result<()> {
    let release = fetch_release(&spec.repo, spec.tag.as_deref(), token)?;
    let version = strip_v(&release.tag).to_string();
    println!("{}: {} ({})", spec.name, version, release.tag);

    let mut verified = Vec::new();
    let mut unverified: Vec<(String, String)> = Vec::new();
    // Canonical key order for deterministic output.
    let mut keys: Vec<&String> = spec.assets.keys().collect();
    keys.sort();
    for key in keys {
        let asset_spec = &spec.assets[key];
        let file = render(&asset_spec.file, &version, &release.tag);
        let names: Vec<String> = release.assets.iter().map(|(n, _)| n.clone()).collect();
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
        let Some((_, url)) = release.assets.iter().find(|(n, _)| n == &file) else {
            unreachable!("find_asset matched above");
        };
        println!("  [{key}] {file}");
        let (archive, sha256) = download_hashed(url, None, &file)?;
        println!("    sha256 {sha256}");
        let override_dir = asset_spec
            .extract_dir
            .as_ref()
            .map(|d| render(d, &version, &release.tag));
        let extract_dir = gate_payload(spec, key, &archive, override_dir, &mut unverified)?;
        if let Some(d) = &extract_dir {
            println!("    extract_dir {d}");
        }
        verified.push(VerifiedSource {
            key: key.clone(),
            url: asset_url_with_fragment(url),
            sha256,
            extract_dir,
        });
    }

    // Merge into the registry.
    let latest = registry_latest(manifests_dir, &spec.name)?;
    let up_to_date = match &latest {
        Some((v, _)) => voli_core::index::cmp_version(&version, v) != std::cmp::Ordering::Greater,
        None => false,
    };
    if up_to_date {
        let (_, manifest) = latest.unwrap();
        let merged = merge_unix_sources(manifest, &verified);
        write_manifest_file(manifests_dir, &merged)?;
        println!("  merged unix blocks into {}", merged.version);
    } else {
        let mut templates = BTreeMap::new();
        for (key, asset_spec) in &spec.assets {
            // Future `bump` support: per-platform url templates.
            templates.insert(key.clone(), asset_spec.file.clone());
        }
        let manifest = new_unix_manifest(spec, &version, &verified, templates);
        write_manifest_file(manifests_dir, &manifest)?;
        println!("  new file for {version} (unix blocks only)");
    }
    if !unverified.is_empty() {
        println!("  UNVERIFIED (hash + static checks only — needs a matching host):");
        for (key, reason) in &unverified {
            println!("    [{key}] {reason}; run --verify-only on matching CI to close");
        }
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
        match gate_binary(&path, bin, &spec.smoke_args, is_mac)? {
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
    /// Static checks passed but execution was impossible on this host (wrong
    /// OS, or an ELF for a foreign arch). The block is emitted with its hash;
    /// a matching CI leg must run `--verify-only` over it.
    StaticOnly(String),
}

/// Static linkage check + live smoke run for one binary.
fn gate_binary(path: &Path, bin: &str, smoke_args: &[String], is_mac: bool) -> Result<GateOutcome> {
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
        let source = manifest
            .source
            .for_platform(host)
            .ok_or_else(|| Error::Config(format!("{name} {version} has no [{key}] block")))?;
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
        let smoke_args = specs
            .iter()
            .find(|s| &s.name == name)
            .map(|s| s.smoke_args.clone())
            .unwrap_or_else(|| vec!["--version".to_string()]);
        for b in &manifest.bin {
            // Same resolution the install engine uses (unix payloads are
            // extensionless where Windows-first manifests name `.exe`).
            let path = voli_core::resolve_bin_target(&root, b.path());
            if !path.is_file() {
                return Err(Error::Config(format!(
                    "bin '{}' resolves to {}, which is not a file",
                    b.path(),
                    path.display()
                )));
            }
            match gate_binary(&path, b.path(), &smoke_args, cfg!(target_os = "macos"))? {
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
