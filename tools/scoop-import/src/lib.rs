//! Scoop `main` bucket → voli TOML converter (spec §7).
//!
//! Pure conversion + manual TOML emission. The driver in `main.rs` handles
//! cloning, file IO, and the round-trip check against `voli_core::Manifest`.
//!
//! Manual emission (not the `toml` crate) is used on purpose: voli's schema
//! requires every top-level scalar key to appear BEFORE any `[table]` header
//! (spec §4 — otherwise TOML absorbs the scalar into the last table). Emitting
//! sections in a fixed sequence makes that ordering guaranteed by construction.

use serde_json::Value;

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

struct SourceParts {
    url: String,
    hash: Hash,
    extract_dir: Option<String>,
}

/// Hash algorithm used by the source.
enum Hash {
    Sha256(String),
    Sha512(String),
}

enum BinOut {
    Path(String),
    Table {
        name: String,
        path: String,
        args: Option<String>,
    },
}

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
    let (x64, arm64) = match resolve_sources(json) {
        Ok(pair) => pair,
        Err(reason) => return Outcome::Skip(reason),
    };
    // voli has a single extract_dir; prefer x64's, else arm64's.
    let extract_dir = x64
        .as_ref()
        .and_then(|s| s.extract_dir.clone())
        .or_else(|| arm64.as_ref().and_then(|s| s.extract_dir.clone()));

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
    let bins = match parse_bin(json.get("bin")) {
        Ok(b) => b,
        Err(reason) => return Outcome::Skip(reason),
    };

    let depends = parse_depends(json.get("depends"));
    let description = string_or_joined(json.get("description"));
    let homepage = json
        .get("homepage")
        .and_then(Value::as_str)
        .map(str::to_string);
    let license = parse_license(json.get("license"));
    let autoupdate = build_autoupdate(json, homepage.as_deref());

    let toml = emit_toml(
        &name,
        &version,
        description.as_deref(),
        homepage.as_deref(),
        license.as_deref(),
        extract_dir.as_deref(),
        &bins,
        &persist,
        x64.as_ref(),
        arm64.as_ref(),
        &env,
        &depends,
        autoupdate.as_deref(),
    );

    Outcome::Ok(Converted {
        name,
        version,
        toml,
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

/// Returns (x64, arm64). Errs with a skip reason if a used entry is bad or no
/// mappable 64-bit/arm64 source exists (voli drops 32-bit).
fn resolve_sources(
    json: &Value,
) -> Result<(Option<SourceParts>, Option<SourceParts>), &'static str> {
    let arch = json.get("architecture").and_then(Value::as_object);

    let (x64_entry, arm64_entry): (Option<&Value>, Option<&Value>) = match arch {
        Some(a) => (a.get("64bit"), a.get("arm64")),
        // No `architecture`: a top-level url is treated as the x64 source.
        None => (json.get("url").map(|_| json), None),
    };

    let x64 = resolve_entry(x64_entry)?;
    let arm64 = resolve_entry(arm64_entry)?;

    if x64.is_none() && arm64.is_none() {
        // Distinguish "only 32-bit offered" from "nothing at all".
        let only32 = arch.is_some_and(|a| a.contains_key("32bit"));
        return Err(if only32 { "no-64bit-source" } else { "no-url" });
    }
    Ok((x64, arm64))
}

/// `None` entry or entry without a url → `Ok(None)` (arch simply absent).
fn resolve_entry(entry: Option<&Value>) -> Result<Option<SourceParts>, &'static str> {
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

    if is_installer_ext(&url) {
        return Err("installer-binary");
    }
    let hash = normalize_hash(&raw_hash)?;
    let extract_dir = entry.get("extract_dir").and_then(first_string);

    Ok(Some(SourceParts {
        url,
        hash,
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

/// Normalize a Scoop hash to a [`Hash`], handling `sha256:`/`sha512:` prefixes.
/// md5/sha1 (by prefix or by length) are still rejected.
fn normalize_hash(raw: &str) -> Result<Hash, &'static str> {
    let raw = raw.trim();
    if let Some((prefix, rest)) = raw.split_once(':') {
        return match prefix.to_ascii_lowercase().as_str() {
            "sha256" => Ok(Hash::Sha256(hex_n(rest, 64)?)),
            "sha512" => Ok(Hash::Sha512(hex_n(rest, 128)?)),
            _ => Err("unsupported-hash"), // md5/sha1/unknown
        };
    }
    // Bare hash: determine by length.
    let s = raw.trim().to_ascii_lowercase();
    if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(Hash::Sha256(s))
    } else if s.len() == 128 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(Hash::Sha512(s))
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

/// True if the URL's effective download extension is `.exe`/`.msi`.
/// Scoop uses a `#/name.ext` fragment to set the real filename; honor it.
fn is_installer_ext(url: &str) -> bool {
    matches!(effective_ext(url).as_deref(), Some("exe") | Some("msi"))
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

// --- bin -----------------------------------------------------------------

fn parse_bin(v: Option<&Value>) -> Result<Vec<BinOut>, &'static str> {
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

fn push_bin_path(out: &mut Vec<BinOut>, path: &str) -> Result<(), &'static str> {
    check_bin_path(path)?;
    out.push(BinOut::Path(path.to_string()));
    Ok(())
}

/// Scoop nested bin: `[path]`, `[path, alias]`, or `[path, alias, args]`.
/// `args` may be a string or an array of strings.
fn parse_bin_nested(nested: &[Value]) -> Result<BinOut, &'static str> {
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
        (None, None) => Ok(BinOut::Path(path.to_string())),
        _ => Ok(BinOut::Table {
            name: alias.map(str::to_string).unwrap_or_else(|| stem(path)),
            path: path.to_string(),
            args,
        }),
    }
}

fn stem(path: &str) -> String {
    let seg = path.rsplit(['/', '\\']).next().unwrap_or(path);
    seg.rsplit_once('.')
        .map(|(a, _)| a)
        .unwrap_or(seg)
        .to_string()
}

/// Mirror of voli_core's bin-path rule: relative, no `..`, no drive/root.
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
/// Ordered so PATH (if any) comes first, then env_set keys sorted.
fn parse_env(json: &Value) -> Result<Vec<(String, String)>, &'static str> {
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

    let mut out: Vec<(String, String)> = Vec::new();
    if !path_segs.is_empty() {
        out.push(("PATH".to_string(), path_segs.join(";")));
    }

    if let Some(set) = json.get("env_set").filter(|v| !v.is_null()) {
        let obj = set.as_object().ok_or("env-unmappable")?;
        let mut entries: Vec<(String, String)> = Vec::new();
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
            entries.push((k.clone(), mapped));
        }
        entries.sort();
        for (k, v) in entries {
            if k == "PATH" {
                // Merge into the PATH we already built.
                if let Some((_, existing)) = out.iter_mut().find(|(kk, _)| kk == "PATH") {
                    *existing = format!("{existing};{v}");
                } else {
                    out.push((k, v));
                }
            } else {
                out.push((k, v));
            }
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

fn parse_depends(v: Option<&Value>) -> Vec<(String, String)> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Vec::new();
    };
    let names: Vec<&str> = match v {
        Value::String(s) => vec![s.as_str()],
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => return Vec::new(),
    };
    let mut out: Vec<(String, String)> = Vec::new();
    for n in names {
        // Strip a bucket prefix like "extras/foo" → "foo".
        let name = n.rsplit('/').next().unwrap_or(n).to_ascii_lowercase();
        if !name.is_empty() && !out.iter().any(|(k, _)| *k == name) {
            out.push((name, "*".to_string()));
        }
    }
    out.sort();
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

/// Best-effort [autoupdate] contents (opaque to the client).
/// Returns the body to place under `[autoupdate]`, or None if no checkver.
fn build_autoupdate(json: &Value, homepage: Option<&str>) -> Option<String> {
    let cv = json.get("checkver").filter(|v| !v.is_null())?;

    // Prefer an explicit github repo, then a github homepage.
    let repo = cv
        .get("github")
        .and_then(Value::as_str)
        .and_then(github_repo)
        .or_else(|| homepage.and_then(github_repo));

    if let Some(repo) = repo {
        return Some(format!("checkver = {{ github = {} }}", esc(&repo)));
    }
    // Non-github: store the raw pattern as an opaque string.
    let raw = match cv {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    Some(format!("checkver = {}", esc(&raw)))
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

// --- TOML emission -------------------------------------------------------

/// TOML basic-string with correct escaping.
fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04X}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// A bare key if safe, otherwise a quoted key.
fn key(k: &str) -> String {
    if !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        k.to_string()
    } else {
        esc(k)
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_toml(
    name: &str,
    version: &str,
    description: Option<&str>,
    homepage: Option<&str>,
    license: Option<&str>,
    extract_dir: Option<&str>,
    bins: &[BinOut],
    persist: &[String],
    x64: Option<&SourceParts>,
    arm64: Option<&SourceParts>,
    env: &[(String, String)],
    depends: &[(String, String)],
    autoupdate: Option<&str>,
) -> String {
    let mut o = String::new();

    // --- top-level scalars FIRST (before any [table]) ---
    o.push_str(&format!("name = {}\n", esc(name)));
    o.push_str(&format!("version = {}\n", esc(version)));
    if let Some(d) = description {
        o.push_str(&format!("description = {}\n", esc(d)));
    }
    if let Some(h) = homepage {
        o.push_str(&format!("homepage = {}\n", esc(h)));
    }
    if let Some(l) = license {
        o.push_str(&format!("license = {}\n", esc(l)));
    }
    o.push_str("kind = \"app\"\n");
    if let Some(e) = extract_dir {
        o.push_str(&format!("extract_dir = {}\n", esc(e)));
    }
    if !bins.is_empty() {
        o.push_str(&format!("bin = {}\n", emit_bins(bins)));
    }
    if !persist.is_empty() {
        let items: Vec<String> = persist.iter().map(|p| esc(p)).collect();
        o.push_str(&format!("persist = [{}]\n", items.join(", ")));
    }

    // --- tables ---
    if let Some(s) = x64 {
        o.push_str("\n[source.x64]\n");
        o.push_str(&format!("url = {}\n", esc(&s.url)));
        match &s.hash {
            Hash::Sha256(h) => o.push_str(&format!("sha256 = {}\n", esc(h))),
            Hash::Sha512(h) => o.push_str(&format!("sha512 = {}\n", esc(h))),
        }
    }
    if let Some(s) = arm64 {
        o.push_str("\n[source.arm64]\n");
        o.push_str(&format!("url = {}\n", esc(&s.url)));
        match &s.hash {
            Hash::Sha256(h) => o.push_str(&format!("sha256 = {}\n", esc(h))),
            Hash::Sha512(h) => o.push_str(&format!("sha512 = {}\n", esc(h))),
        }
    }
    if !env.is_empty() {
        o.push_str("\n[env]\n");
        for (k, v) in env {
            o.push_str(&format!("{} = {}\n", key(k), esc(v)));
        }
    }
    if !depends.is_empty() {
        o.push_str("\n[depends]\n");
        for (k, v) in depends {
            o.push_str(&format!("{} = {}\n", key(k), esc(v)));
        }
    }
    if let Some(au) = autoupdate {
        o.push_str("\n[autoupdate]\n");
        o.push_str(au);
        o.push('\n');
    }

    o
}

fn emit_bins(bins: &[BinOut]) -> String {
    let items: Vec<String> = bins
        .iter()
        .map(|b| match b {
            BinOut::Path(p) => esc(p),
            BinOut::Table { name, path, args } => {
                let mut t = format!("{{ name = {}, path = {}", esc(name), esc(path));
                if let Some(a) = args {
                    t.push_str(&format!(", args = {}", esc(a)));
                }
                t.push_str(" }");
                t
            }
        })
        .collect();
    format!("[{}]", items.join(", "))
}

#[cfg(test)]
mod tests;
