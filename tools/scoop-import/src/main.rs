//! Driver: read a Scoop bucket clone, convert each manifest to voli
//! TOML, round-trip every emitted file through `voli_core::Manifest`, write
//! accepted packages under `manifests/<first-letter>/<name>/<version>.toml`,
//! and emit `manifests/_import-report.md`.
//!
//! Usage: scoop-import <scoop-bucket-dir> <out-manifests-dir>
//!
//! `<scoop-bucket-dir>` may be the repo root (containing `bucket/`) or the
//! `bucket/` dir itself.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use scoop_import::{Outcome, convert};
use voli_core::manifest::Manifest;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: scoop-import <scoop-bucket-dir> <out-manifests-dir>");
        return ExitCode::FAILURE;
    }
    let bucket = locate_bucket(Path::new(&args[1]));
    let out = PathBuf::from(&args[2]);
    import_bucket(&bucket, &out)
}

fn import_bucket(bucket: &Path, out: &Path) -> ExitCode {
    let mut jsons: Vec<PathBuf> = match fs::read_dir(bucket) {
        Ok(rd) => rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect(),
        Err(e) => {
            eprintln!("cannot read {}: {e}", bucket.display());
            return ExitCode::FAILURE;
        }
    };
    jsons.sort();

    let mut converted = 0usize;
    let mut write_failures = 0usize;
    let mut skips: Vec<(String, &'static str)> = Vec::new();
    let mut roundtrip_failures: Vec<(String, String)> = Vec::new();

    for path in &jsons {
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                skips.push((stem, "read-error"));
                eprintln!("read {}: {e}", path.display());
                continue;
            }
        };
        let json: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => {
                skips.push((stem, "json-parse-error"));
                continue;
            }
        };

        match convert(&stem, &json) {
            Outcome::Skip(reason) => skips.push((stem, reason)),
            Outcome::Ok(c) => {
                // Round-trip: a failure here is an emitter bug, not a skip.
                if let Err(e) = Manifest::from_toml_str(&c.toml) {
                    roundtrip_failures.push((c.name.clone(), e.to_string()));
                    eprintln!("ROUND-TRIP FAILED for {}: {e}\n{}", c.name, c.toml);
                    continue;
                }
                if let Err(e) = write_manifest(out, &c.name, &c.version, &c.toml) {
                    eprintln!("write {}: {e}", c.name);
                    write_failures += 1;
                    continue;
                }
                converted += 1;
            }
        }
    }

    let report = build_report(jsons.len(), converted, &skips, &roundtrip_failures);
    let report_path = out.join("_import-report.md");
    if let Err(e) = fs::write(&report_path, &report) {
        eprintln!("cannot write report {}: {e}", report_path.display());
        return ExitCode::FAILURE;
    }

    println!(
        "processed {} manifests: {converted} converted, {} skipped, {} round-trip failures, {write_failures} write failures",
        jsons.len(),
        skips.len(),
        roundtrip_failures.len()
    );
    println!("report: {}", report_path.display());

    // A failed refresh must not be reported as a successful sync.
    if roundtrip_failures.is_empty() && write_failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn locate_bucket(dir: &Path) -> PathBuf {
    let nested = dir.join("bucket");
    if nested.is_dir() {
        nested
    } else {
        dir.to_path_buf()
    }
}

fn write_manifest(out: &Path, name: &str, version: &str, toml: &str) -> std::io::Result<()> {
    let first = name.chars().next().unwrap_or('_').to_string();
    let dir = out.join(&first).join(name);
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.toml", sanitize(version)));
    let text = match fs::read_to_string(&path) {
        Ok(existing) => {
            let invalid = |error| std::io::Error::new(std::io::ErrorKind::InvalidData, error);
            let existing = Manifest::from_toml_str(&existing).map_err(invalid)?;
            let mut update = Manifest::from_toml_str(toml).map_err(invalid)?;
            if existing.name != name
                || existing.version != version
                || update.name != name
                || update.version != version
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "manifest identity does not match refresh destination",
                ));
            }
            // Scoop owns Windows sources only. Preserve independently verified
            // Unix payloads when refreshing this exact package version. Never
            // copy them from another version, whose binaries/hashes may differ.
            update.source.linux_x64 = existing.source.linux_x64;
            update.source.linux_arm64 = existing.source.linux_arm64;
            update.source.macos_x64 = existing.source.macos_x64;
            update.source.macos_arm64 = existing.source.macos_arm64;
            let text = update.to_canonical_toml();
            Manifest::from_toml_str(&text).map_err(invalid)?;
            text
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml.to_string(),
        Err(e) => return Err(e),
    };
    fs::write(path, text)
}

/// Make a version safe as a Windows filename component.
fn sanitize(version: &str) -> String {
    version
        .chars()
        .map(|c| {
            if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect()
}

fn build_report(
    total: usize,
    converted: usize,
    skips: &[(String, &'static str)],
    roundtrip_failures: &[(String, String)],
) -> String {
    let mut by_reason: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (name, reason) in skips {
        by_reason.entry(reason).or_default().push(name);
    }

    let mut o = String::new();
    o.push_str("# Scoop bucket → voli import report\n\n");
    o.push_str("Generated by `tools/scoop-import` from an Unlicense Scoop bucket.\n\n");
    o.push_str("## Summary\n\n");
    o.push_str(&format!("- Total Scoop manifests: **{total}**\n"));
    o.push_str(&format!("- Converted: **{converted}**\n"));
    o.push_str(&format!("- Skipped: **{}**\n", skips.len()));
    if !roundtrip_failures.is_empty() {
        o.push_str(&format!(
            "- **Round-trip failures (emitter bugs): {}**\n",
            roundtrip_failures.len()
        ));
    }
    o.push('\n');

    o.push_str("## Skipped by reason\n\n");
    o.push_str("| Reason | Count |\n|---|---|\n");
    for (reason, names) in &by_reason {
        o.push_str(&format!("| {reason} | {} |\n", names.len()));
    }
    o.push('\n');

    if !roundtrip_failures.is_empty() {
        o.push_str("## Round-trip failures\n\n");
        for (name, err) in roundtrip_failures {
            o.push_str(&format!("- `{name}` — {err}\n"));
        }
        o.push('\n');
    }

    o.push_str("## Full skip list (hand-conversion worklist)\n\n");
    o.push_str("Sorted by reason, then name.\n\n");
    for (reason, names) in &by_reason {
        o.push_str(&format!("### {reason} ({})\n\n", names.len()));
        let mut sorted = names.clone();
        sorted.sort_unstable();
        for name in sorted {
            o.push_str(&format!("- {name}\n"));
        }
        o.push('\n');
    }

    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "scoop-import-test-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture(version: &str) -> Manifest {
        Manifest::from_toml_str(&format!(
            r#"name = "tool"
version = "{version}"
kind = "app"
bin = ["tool.exe"]

[source.x64]
url = "https://example.com/tool.zip"
sha256 = "{}"
"#,
            "a".repeat(64)
        ))
        .unwrap()
    }

    #[test]
    fn refresh_preserves_same_version_unix_sources_and_updates_windows() {
        let dir = TestDir::new();
        let mut existing = fixture("1.0.0");
        let mut unix = existing.source.x64.clone().unwrap();
        unix.url = "https://example.com/tool.tar.gz".to_string();
        unix.extract_dir = Some("tool/bin".to_string());
        existing.source.linux_x64 = Some(unix.clone());
        existing.source.linux_arm64 = Some(unix.clone());
        existing.source.macos_x64 = Some(unix.clone());
        existing.source.macos_arm64 = Some(unix);
        write_manifest(&dir.0, "tool", "1.0.0", &existing.to_canonical_toml()).unwrap();

        let mut update = fixture("1.0.0");
        update.source.x64.as_mut().unwrap().url = "https://example.com/corrected.zip".to_string();
        update.source.x64.as_mut().unwrap().sha256 = Some("b".repeat(64));
        write_manifest(&dir.0, "tool", "1.0.0", &update.to_canonical_toml()).unwrap();
        let text = fs::read_to_string(dir.0.join("t/tool/1.0.0.toml")).unwrap();
        let result = Manifest::from_toml_str(&text).unwrap();
        assert_eq!(result.source.x64, update.source.x64);
        assert_eq!(result.source.linux_x64, existing.source.linux_x64);
        assert_eq!(result.source.linux_arm64, existing.source.linux_arm64);
        assert_eq!(result.source.macos_x64, existing.source.macos_x64);
        assert_eq!(result.source.macos_arm64, existing.source.macos_arm64);
        assert!(result.is_canonical_toml(&text));
    }

    #[test]
    fn new_version_never_inherits_old_version_unix_payloads() {
        let dir = TestDir::new();
        let mut old = fixture("1.0.0");
        old.source.linux_x64 = old.source.x64.clone();
        write_manifest(&dir.0, "tool", "1.0.0", &old.to_canonical_toml()).unwrap();
        let update = fixture("2.0.0");
        write_manifest(&dir.0, "tool", "2.0.0", &update.to_canonical_toml()).unwrap();
        let result =
            Manifest::from_toml_str(&fs::read_to_string(dir.0.join("t/tool/2.0.0.toml")).unwrap())
                .unwrap();
        assert_eq!(result, update);
        let unchanged =
            Manifest::from_toml_str(&fs::read_to_string(dir.0.join("t/tool/1.0.0.toml")).unwrap())
                .unwrap();
        assert_eq!(unchanged, old);
    }

    #[test]
    fn refresh_rejects_invalid_existing_manifest_without_overwriting_it() {
        let dir = TestDir::new();
        let path = dir.0.join("t/tool/1.0.0.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "invalid manifest").unwrap();
        assert!(
            write_manifest(
                &dir.0,
                "tool",
                "1.0.0",
                &fixture("1.0.0").to_canonical_toml()
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "invalid manifest");
    }

    #[test]
    fn importer_fails_if_existing_manifest_cannot_be_refreshed() {
        let dir = TestDir::new();
        let bucket = dir.0.join("bucket");
        let out = dir.0.join("manifests");
        fs::create_dir_all(&bucket).unwrap();
        fs::create_dir_all(out.join("t/tool")).unwrap();
        fs::write(out.join("t/tool/1.0.0.toml"), "invalid manifest").unwrap();
        fs::write(
            bucket.join("tool.json"),
            serde_json::json!({
                "version": "1.0.0",
                "url": "https://example.com/tool.zip",
                "hash": "a".repeat(64),
                "bin": "tool.exe",
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(import_bucket(&bucket, &out), ExitCode::FAILURE);
        assert_eq!(
            fs::read_to_string(out.join("t/tool/1.0.0.toml")).unwrap(),
            "invalid manifest"
        );
    }

    #[test]
    fn refresh_rejects_mismatched_existing_identity_without_overwriting_it() {
        let dir = TestDir::new();
        let path = dir.0.join("t/tool/1.0.0.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let wrong_version = fixture("0.9.0").to_canonical_toml();
        fs::write(&path, &wrong_version).unwrap();
        assert!(
            write_manifest(
                &dir.0,
                "tool",
                "1.0.0",
                &fixture("1.0.0").to_canonical_toml()
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(path).unwrap(), wrong_version);
    }
}
