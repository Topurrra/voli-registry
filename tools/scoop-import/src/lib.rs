//! Scoop bucket → voli TOML converter (spec §7).
//!
//! Pure conversion: parse the Scoop JSON into a `voli_core::manifest::Manifest`
//! and serialize it with [`Manifest::to_canonical_toml`] — the ONE canonical
//! emitter, shared with `voli-index-tool bump`. There is deliberately no local
//! TOML writer: two emitters disagreeing on formatting is what produced a 20-file
//! merge conflict in a single week.
//!
//! The driver in `main.rs` handles cloning, file IO, and re-parsing every emitted
//! file through `Manifest::from_toml_str`, which is also what validates it.

use std::collections::BTreeMap;

use serde_json::Value;
use voli_core::manifest::{Bin, Kind, Manifest, Shortcut, Source, SourceKind, Sources};

/// Outcome of converting one Scoop manifest.
pub enum Outcome {
    /// A voli manifest we can emit.
    Ok(Converted),
    /// Rejected; `reason` is a stable, groupable slug for the report.
    Skip(&'static str),
}

pub struct Converted {
    pub name: String,
    pub version: String,
    pub toml: String,
}

/// Script fields that BLOCK import (arbitrary code execution risk).
/// `post_install` and `post_uninstall` are benign (config/persist tweaks
/// that voli's `persist` handles natively) — allowed and silently dropped.
const BLOCKING_SCRIPT_FIELDS: [&str; 5] = [
    "pre_install",
    "installer",
    "uninstaller",
    "psmodule",
    "pre_uninstall",
];

/// Convert one Scoop manifest. `name_stem` is the JSON filename without `.json`.
pub fn convert(name_stem: &str, json: &Value) -> Outcome {
    // 1. name: voli requires lowercase [a-z0-9-].
    let name = name_stem.to_ascii_lowercase();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Outcome::Skip("invalid-name");
    }

    // 2. version.
    let version = match json.get("version").and_then(Value::as_str) {
        Some(v) if !v.is_empty() => v.to_string(),
        _ => return Outcome::Skip("no-version"),
    };

    // 3. script fields — top-level and per-architecture (e.g. 7zip's arm64
    //    pre_install). Declarative-only rule: no scripts allowed.
    if has_script(json) {
        return Outcome::Skip("script-field");
    }
    if let Some(arch) = json.get("architecture").and_then(Value::as_object)
        && arch.values().any(has_script)
    {
        return Outcome::Skip("script-field");
    }

    // 4. sources.
    let (mut x64, mut arm64) = match resolve_sources(json) {
        Ok(pair) => pair,
        Err(reason) => return Outcome::Skip(reason),
    };
    let extract_dir = hoist_extract_dir(&mut x64, &mut arm64);

    // 5. persist (skip nested [src, dst] forms).
    let persist = match parse_persist(json.get("persist")) {
        Ok(p) => p,
        Err(reason) => return Outcome::Skip(reason),
    };

    // 6. env (env_add_path + env_set → [env]).
    let env = match parse_env(json) {
        Ok(e) => e,
        Err(reason) => return Outcome::Skip(reason),
    };

    // 7. bin.
    let bin = match parse_bin(json.get("bin")) {
        Ok(b) => b,
        Err(reason) => return Outcome::Skip(reason),
    };

    // 8. Start Menu shortcuts.
    let shortcuts = match parse_shortcuts(json.get("shortcuts")) {
        Ok(s) => s,
        Err(reason) => return Outcome::Skip(reason),
    };

    let has_installer_archive = [&x64, &arm64].into_iter().any(|s| {
        s.as_ref()
            .is_some_and(|s| s.kind == SourceKind::InstallerArchive)
    });
    if has_installer_archive && bin.is_empty() && shortcuts.is_empty() && !env.contains_key("PATH")
    {
        return Outcome::Skip("no-launch-entry");
    }

    let homepage = json
        .get("homepage")
        .and_then(Value::as_str)
        .map(str::to_string);
    let icon = json
        .get("icon")
        .and_then(Value::as_str)
        .filter(|url| url.starts_with("https://"))
        .or(match name.as_str() {
            "googlechrome" => {
                Some("https://www.google.com/chrome/static/images/chrome-logo-m100.svg")
            }
            _ => None,
        });
    let autoupdate = build_autoupdate(&name, json, homepage.as_deref());

    let manifest = Manifest {
        name: name.clone(),
        version: version.clone(),
        // Scoop has no notion of a former name, and synthesizing one from an
        // import would let an upstream bucket claim names in this registry.
        aliases: Vec::new(),
        description: string_or_joined(json.get("description")),
        homepage,
        icon: icon.map(str::to_string),
        license: parse_license(json.get("license")),
        kind: Kind::App,
        source: Sources {
            x64,
            arm64,
            ..Sources::default()
        },
        extract_dir,
        // Scoop has no equivalent of these: `file_name` needs a binary source
        // (which is `standalone-binary`, i.e. skipped), Scoop cannot express a
        // written-out file, and `gui` is voli-only curation.
        file_name: None,
        write_file: Vec::new(),
        gui: None,
        bin,
        env,
        depends: parse_depends(json.get("depends")),
        autoupdate,
        persist,
        shortcuts,
    };

    Outcome::Ok(Converted {
        name,
        version,
        toml: manifest.to_canonical_toml(),
    })
}

fn has_script(v: &Value) -> bool {
    v.as_object().is_some_and(|m| {
        BLOCKING_SCRIPT_FIELDS
            .iter()
            .any(|k| m.get(*k).is_some_and(|x| !x.is_null()))
    })
}

// --- sources -------------------------------------------------------------

/// voli takes `extract_dir` either once at the top level or per `[source.<arch>]`,
/// where it wins. Collapse to the top-level field whenever every present arch
/// agrees — that is nearly all of them, and emitting a per-source copy instead
/// would rewrite thousands of manifests for no behaviour change.
///
/// When the arches disagree the per-source values stay, which is the fix: vendors
/// put the arch token in the wrapper directory name
/// (`zig-x86_64-windows-0.16.0`), so the x64 value is simply wrong for the arm64
/// archive. "One arch has one and the other has none" counts as disagreement for
/// the same reason.
fn hoist_extract_dir(x64: &mut Option<Source>, arm64: &mut Option<Source>) -> Option<String> {
    let x64_dir = x64.as_ref().and_then(|s| s.extract_dir.clone());
    let arm64_dir = arm64.as_ref().and_then(|s| s.extract_dir.clone());
    if x64.is_some() && arm64.is_some() && x64_dir != arm64_dir {
        return None;
    }
    for s in [x64.as_mut(), arm64.as_mut()].into_iter().flatten() {
        s.extract_dir = None;
    }
    x64_dir.or(arm64_dir)
}

/// Returns (x64, arm64), each carrying its own `extract_dir`
/// ([`hoist_extract_dir`] decides where that lands). Errs with a skip reason if a
/// used entry is bad or no mappable 64-bit/arm64 source exists (voli drops 32-bit).
fn resolve_sources(json: &Value) -> Result<(Option<Source>, Option<Source>), &'static str> {
    let arch = json.get("architecture").and_then(Value::as_object);

    let (x64_entry, arm64_entry): (Option<&Value>, Option<&Value>) = match arch {
        Some(a) => (a.get("64bit"), a.get("arm64")),
        // No `architecture`: a top-level url is treated as the x64 source.
        None => (json.get("url").map(|_| json), None),
    };

    let top_level_innosetup = json
        .get("innosetup")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let top_level_extract_dir = json.get("extract_dir").and_then(first_string);
    let x64 = resolve_entry(
        x64_entry,
        top_level_innosetup,
        top_level_extract_dir.as_deref(),
    )?;
    let arm64 = resolve_entry(
        arm64_entry,
        top_level_innosetup,
        top_level_extract_dir.as_deref(),
    )?;

    if x64.is_none() && arm64.is_none() {
        // Distinguish "only 32-bit offered" from "nothing at all".
        let only32 = arch.is_some_and(|a| a.contains_key("32bit"));
        return Err(if only32 { "no-64bit-source" } else { "no-url" });
    }
    Ok((x64, arm64))
}

/// `None` entry or entry without a url → `Ok(None)` (arch simply absent).
fn resolve_entry(
    entry: Option<&Value>,
    top_level_innosetup: bool,
    top_level_extract_dir: Option<&str>,
) -> Result<Option<Source>, &'static str> {
    let Some(entry) = entry else {
        return Ok(None);
    };
    let url_v = match entry.get("url") {
        Some(v) if !v.is_null() => v,
        _ => return Ok(None),
    };
    let url = one_string(url_v).ok_or("multi-url")?;

    let hash_v = match entry.get("hash") {
        Some(v) if !v.is_null() => v,
        _ => return Err("no-hash"),
    };
    let raw_hash = one_string(hash_v).ok_or("multi-url")?;

    let kind = match effective_ext(&url).as_deref() {
        Some("msi") => SourceKind::InstallerArchive,
        Some("exe")
            if top_level_innosetup
                || entry
                    .get("innosetup")
                    .and_then(Value::as_bool)
                    .unwrap_or(false) =>
        {
            SourceKind::InstallerArchive
        }
        // A bare PE may be the application itself. Passing it to 7-Zip strips
        // it into sections instead of preserving the executable.
        Some("exe") => return Err("standalone-binary"),
        _ => SourceKind::Archive,
    };
    let (sha256, sha512) = normalize_hash(&raw_hash)?;
    let extract_dir = entry
        .get("extract_dir")
        .and_then(first_string)
        .or_else(|| top_level_extract_dir.map(str::to_string));

    Ok(Some(Source {
        url,
        sha256,
        sha512,
        extra: Vec::new(),
        kind,
        extract_dir,
    }))
}

/// A single string, or a one-element string array. Otherwise `None` (multi/empty).
fn one_string(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = v.as_array()
        && arr.len() == 1
    {
        return arr[0].as_str().map(str::to_string);
    }
    None
}

/// A string, or the first element of an array if it is a string.
fn first_string(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    v.as_array()
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .map(str::to_string)
}

type HashPair = (Option<String>, Option<String>);

/// Normalize a Scoop hash into `(sha256, sha512)` — exactly one is `Some`.
/// Handles `sha256:`/`sha512:` prefixes; md5/sha1 (by prefix or by length) are
/// still rejected.
fn normalize_hash(raw: &str) -> Result<HashPair, &'static str> {
    let raw = raw.trim();
    if let Some((prefix, rest)) = raw.split_once(':') {
        return match prefix.to_ascii_lowercase().as_str() {
            "sha256" => Ok((Some(hex_n(rest, 64)?), None)),
            "sha512" => Ok((None, Some(hex_n(rest, 128)?))),
            _ => Err("unsupported-hash"), // md5/sha1/unknown
        };
    }
    // Bare hash: determine by length.
    let s = raw.to_ascii_lowercase();
    if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok((Some(s), None))
    } else if s.len() == 128 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok((None, Some(s)))
    } else {
        Err("unsupported-hash")
    }
}

fn hex_n(s: &str, len: usize) -> Result<String, &'static str> {
    let s = s.trim().to_ascii_lowercase();
    if s.len() == len && s.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(s)
    } else {
        Err("unsupported-hash")
    }
}

fn effective_ext(url: &str) -> Option<String> {
    let candidate: &str = match url.split_once('#') {
        Some((base, frag)) => {
            let frag = frag.strip_prefix('/').unwrap_or(frag);
            if frag.contains('.') { frag } else { base }
        }
        None => url,
    };
    let candidate = candidate.split('?').next().unwrap_or(candidate);
    let seg = candidate.rsplit(['/', '\\']).next().unwrap_or(candidate);
    if !seg.contains('.') {
        return None;
    }
    seg.rsplit('.').next().map(|e| e.to_ascii_lowercase())
}

// --- shortcuts -----------------------------------------------------------

fn parse_shortcuts(v: Option<&Value>) -> Result<Vec<Shortcut>, &'static str> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let entries = v.as_array().ok_or("shortcut-malformed")?;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let parts = entry.as_array().ok_or("shortcut-malformed")?;
        let target = parts
            .first()
            .and_then(Value::as_str)
            .ok_or("shortcut-malformed")?;
        let name = parts
            .get(1)
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or("shortcut-malformed")?;
        if parts
            .get(2)
            .is_some_and(|args| !args.is_null() && args.as_str() != Some(""))
        {
            return Err("shortcut-args");
        }
        if parts.len() > 4 {
            return Err("shortcut-malformed");
        }
        check_bin_path(target)?;
        out.push(Shortcut::Table {
            target: target.to_string(),
            name: name.to_string(),
        });
    }
    Ok(out)
}

// --- bin -----------------------------------------------------------------

fn parse_bin(v: Option<&Value>) -> Result<Vec<Bin>, &'static str> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    match v {
        Value::String(s) => push_bin_path(&mut out, s)?,
        Value::Array(arr) => {
            for elem in arr {
                match elem {
                    Value::String(s) => push_bin_path(&mut out, s)?,
                    Value::Array(nested) => out.push(parse_bin_nested(nested)?),
                    _ => return Err("bin-malformed"),
                }
            }
        }
        _ => return Err("bin-malformed"),
    }
    Ok(out)
}

fn push_bin_path(out: &mut Vec<Bin>, path: &str) -> Result<(), &'static str> {
    check_bin_path(path)?;
    out.push(Bin::Path(path.to_string()));
    Ok(())
}

/// Scoop nested bin: `[path]`, `[path, alias]`, or `[path, alias, args]`.
/// `args` may be a string or an array of strings.
fn parse_bin_nested(nested: &[Value]) -> Result<Bin, &'static str> {
    let path = nested
        .first()
        .and_then(Value::as_str)
        .ok_or("bin-malformed")?;
    check_bin_path(path)?;

    let alias = nested
        .get(1)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let args = match nested.get(2) {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(a)) => {
            let parts: Option<Vec<&str>> = a.iter().map(Value::as_str).collect();
            parts.map(|p| p.join(" ")).filter(|s| !s.is_empty())
        }
        _ => return Err("bin-malformed"),
    };

    match (alias, &args) {
        (None, None) => Ok(Bin::Path(path.to_string())),
        // The default shim name is voli's own stem rule, not a local copy of it.
        _ => Ok(Bin::Table {
            name: alias
                .map(str::to_string)
                .unwrap_or_else(|| Bin::Path(path.to_string()).shim_name()),
            path: path.to_string(),
            args,
        }),
    }
}

/// Mirror of voli_core's bin-path rule: relative, no `..`, no drive/root.
/// Local because a violation has to become a groupable skip reason rather than a
/// round-trip failure at the end of the pipeline.
fn check_bin_path(path: &str) -> Result<(), &'static str> {
    let absolute =
        path.starts_with('/') || path.starts_with('\\') || path.chars().nth(1) == Some(':');
    let has_parent = path.split(['/', '\\']).any(|c| c == "..");
    if absolute || has_parent {
        Err("bin-path-invalid")
    } else {
        Ok(())
    }
}

// --- persist / env / depends / misc -------------------------------------

fn parse_persist(v: Option<&Value>) -> Result<Vec<String>, &'static str> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    match v {
        Value::String(s) => Ok(vec![s.clone()]),
        Value::Array(arr) => {
            let mut out = Vec::new();
            for e in arr {
                match e.as_str() {
                    Some(s) => out.push(s.to_string()),
                    None => return Err("persist-nested"), // [src, dst] or object
                }
            }
            Ok(out)
        }
        _ => Err("persist-nested"),
    }
}

/// Build [env] from Scoop `env_add_path` (→ PATH) and `env_set` (`$dir`→`{dir}`).
fn parse_env(json: &Value) -> Result<BTreeMap<String, String>, &'static str> {
    let mut path_segs: Vec<String> = Vec::new();

    if let Some(v) = json.get("env_add_path").filter(|v| !v.is_null()) {
        let items: Vec<&str> = match v {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => {
                let parts: Option<Vec<&str>> = a.iter().map(Value::as_str).collect();
                parts.ok_or("env-unmappable")?
            }
            _ => return Err("env-unmappable"),
        };
        for p in items {
            path_segs.push(if p == "." || p.is_empty() {
                "{dir}".to_string()
            } else {
                format!("{{dir}}\\{}", p.replace('/', "\\"))
            });
        }
    }

    let mut out: BTreeMap<String, String> = BTreeMap::new();
    if !path_segs.is_empty() {
        out.insert("PATH".to_string(), path_segs.join(";"));
    }

    if let Some(set) = json.get("env_set").filter(|v| !v.is_null()) {
        let obj = set.as_object().ok_or("env-unmappable")?;
        for (k, val) in obj {
            let s = val.as_str().ok_or("env-unmappable")?;
            let mapped = s.replace("$dir", "{dir}");
            // Any remaining Scoop variable ($persist_dir, $version, …) is unmappable.
            if mapped.contains('$') {
                return Err("env-unmappable");
            }
            // Must satisfy voli's {dir}-only template rule.
            if !env_template_ok(&mapped) {
                return Err("env-unmappable");
            }
            // Only PATH can collide (with the env_add_path segments above); append.
            let merged = match out.get(k) {
                Some(existing) => format!("{existing};{mapped}"),
                None => mapped,
            };
            out.insert(k.clone(), merged);
        }
    }

    Ok(out)
}

/// Local mirror of voli_core's env template check: only `{dir}` is allowed.
fn env_template_ok(val: &str) -> bool {
    let mut rest = val;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return false;
        };
        if &after[..close] != "dir" {
            return false;
        }
        rest = &after[close + 1..];
    }
    // A stray closing brace with no opener would also break voli's parser? No —
    // voli only scans for '{'. But a lone '}' is harmless there. Still reject to
    // be safe against odd inputs.
    !rest.contains('}')
}

fn parse_depends(v: Option<&Value>) -> BTreeMap<String, String> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return BTreeMap::new();
    };
    let names: Vec<&str> = match v {
        Value::String(s) => vec![s.as_str()],
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => return BTreeMap::new(),
    };
    let mut out = BTreeMap::new();
    for n in names {
        // Strip a bucket prefix like "extras/foo" → "foo".
        let name = n.rsplit('/').next().unwrap_or(n).to_ascii_lowercase();
        if !name.is_empty() {
            out.insert(name, "*".to_string());
        }
    }
    out
}

fn parse_license(v: Option<&Value>) -> Option<String> {
    let v = v?;
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    // Object form: { identifier, url }.
    v.get("identifier")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn string_or_joined(v: Option<&Value>) -> Option<String> {
    let v = v?;
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    if let Some(a) = v.as_array() {
        let parts: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
        if !parts.is_empty() {
            return Some(parts.join(" "));
        }
    }
    None
}

// --- autoupdate ----------------------------------------------------------

/// A flat `toml` table of string values. Key order is irrelevant: the canonical
/// serializer imposes its own inside `[autoupdate]`.
fn str_table(pairs: &[(&str, &str)]) -> toml::Value {
    toml::Value::Table(
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), toml::Value::String(v.to_string())))
            .collect(),
    )
}

/// Best-effort `[autoupdate]` value (opaque to the client). `None` if no checkver.
fn build_autoupdate(name: &str, json: &Value, homepage: Option<&str>) -> Option<toml::Value> {
    let cv = json.get("checkver").filter(|v| !v.is_null())?;

    // The clean-room vendor resolver derives Chrome's URL itself — no template.
    if name == "googlechrome" {
        return Some(autoupdate(str_table(&[("vendor", "google-chrome")]), None));
    }

    // Prefer an explicit github repo, then a github homepage.
    let repo = cv
        .get("github")
        .and_then(Value::as_str)
        .and_then(github_repo)
        .or_else(|| homepage.and_then(github_repo));

    let checkver = if let Some(repo) = repo {
        str_table(&[("github", &repo)])
    } else if cv.get("script").is_none()
        && let (Some(url), Some(regex)) = (
            cv.get("url").and_then(Value::as_str),
            cv.get("regex").and_then(Value::as_str),
        )
        && url.starts_with("https://")
    {
        str_table(&[("url", url), ("regex", regex)])
    } else {
        // Non-github: store the raw pattern as an opaque string.
        toml::Value::String(match cv {
            Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        })
    };

    Some(autoupdate(checkver, url_template(json)))
}

fn autoupdate(checkver: toml::Value, url_template: Option<toml::Value>) -> toml::Value {
    let mut t = toml::map::Map::new();
    t.insert("checkver".to_string(), checkver);
    if let Some(u) = url_template {
        t.insert("url_template".to_string(), u);
    }
    toml::Value::Table(t)
}

/// Scoop's `autoupdate.url` / `autoupdate.architecture.*.url` as voli's
/// `url_template`, keeping only templates voli can actually substitute into.
fn url_template(json: &Value) -> Option<toml::Value> {
    let autoupdate = json.get("autoupdate")?;
    if let Some(url) = autoupdate.get("url").and_then(supported_url_template) {
        return Some(toml::Value::String(url.to_string()));
    }

    let arches = autoupdate.get("architecture")?;
    let mut t = toml::map::Map::new();
    for (arch, scoop_arch) in [("x64", "64bit"), ("arm64", "arm64")] {
        if let Some(url) = arches
            .get(scoop_arch)
            .and_then(|value| value.get("url"))
            .and_then(supported_url_template)
        {
            t.insert(arch.to_string(), toml::Value::String(url.to_string()));
        }
    }
    if t.is_empty() {
        None
    } else {
        Some(toml::Value::Table(t))
    }
}

fn supported_url_template(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|url| url.starts_with("https://"))
        .filter(|url| url.contains("$version") || url.contains("{version}"))
}

/// "owner/repo" from a github URL or a bare "owner/repo".
fn github_repo(s: &str) -> Option<String> {
    let rest = s
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let rest = if let Some(r) = rest.strip_prefix("github.com/") {
        r
    } else if rest.contains('/') && !rest.contains(' ') && !rest.contains('.') {
        rest // already "owner/repo"
    } else {
        return None;
    };
    let mut parts = rest.trim_matches('/').split('/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim().trim_end_matches(".git");
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

#[cfg(test)]
mod tests;
