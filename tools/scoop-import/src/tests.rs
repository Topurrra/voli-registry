use super::*;
use serde_json::json;
use voli_core::manifest::{Bin, Kind, Manifest, Shortcut, SourceKind};

/// Convert, assert it succeeded, and return the round-tripped voli Manifest.
fn ok(stem: &str, v: Value) -> Manifest {
    match convert(stem, &v) {
        Outcome::Ok(c) => Manifest::from_toml_str(&c.toml)
            .unwrap_or_else(|e| panic!("round-trip failed for {stem}: {e}\n---\n{}", c.toml)),
        Outcome::Skip(r) => panic!("expected Ok for {stem}, got Skip({r})"),
    }
}

fn skip_reason(stem: &str, v: Value) -> &'static str {
    match convert(stem, &v) {
        Outcome::Skip(r) => r,
        Outcome::Ok(_) => panic!("expected Skip for {stem}, got Ok"),
    }
}

#[test]
fn simple_single_url_top_level() {
    let m = ok(
        "tool",
        json!({
            "version": "1.0.0",
            "description": "A tool",
            "homepage": "https://example.com",
            "icon": "https://example.com/tool.svg",
            "license": "MIT",
            "url": "https://example.com/tool-1.0.0.zip",
            "hash": "a".repeat(64),
            "bin": "tool.exe"
        }),
    );
    assert_eq!(m.name, "tool");
    assert_eq!(m.version, "1.0.0");
    assert_eq!(m.kind, Kind::App);
    assert_eq!(m.icon.as_deref(), Some("https://example.com/tool.svg"));
    assert!(m.source.x64.is_some());
    assert!(m.source.arm64.is_none());
    assert_eq!(m.source.x64.unwrap().hash(), "a".repeat(64));
    assert_eq!(m.bin, vec![Bin::Path("tool.exe".into())]);
}

#[test]
fn curated_icon_survives_future_imports() {
    let m = ok(
        "googlechrome",
        json!({
            "version": "1.0.0",
            "url": "https://example.com/chrome.zip",
            "hash": "a".repeat(64),
            "bin": "chrome.exe"
        }),
    );
    assert_eq!(
        m.icon.as_deref(),
        Some("https://www.google.com/chrome/static/images/chrome-logo-m100.svg")
    );
}

#[test]
fn per_arch_sources() {
    let m = ok(
        "ripgrep",
        json!({
            "version": "15.2.0",
            "architecture": {
                "64bit": { "url": "https://x/rg-x64.zip", "hash": "1".repeat(64), "extract_dir": "rg-x64" },
                "32bit": { "url": "https://x/rg-32.zip", "hash": "2".repeat(64) },
                "arm64": { "url": "https://x/rg-arm64.zip", "hash": "3".repeat(64), "extract_dir": "rg-arm64" }
            },
            "bin": "rg.exe"
        }),
    );
    assert_eq!(m.source.x64.unwrap().url, "https://x/rg-x64.zip");
    assert_eq!(m.source.arm64.unwrap().hash(), "3".repeat(64));
    // extract_dir prefers x64.
    assert_eq!(m.extract_dir.as_deref(), Some("rg-x64"));
}

#[test]
fn top_level_extract_dir_applies_to_arch_sources() {
    let m = ok(
        "app",
        json!({
            "version": "1.0",
            "architecture": {
                "64bit": { "url": "https://x/app.msi", "hash": "1".repeat(64) }
            },
            "extract_dir": "PFiles64/App",
            "shortcuts": [["app.exe", "App"]]
        }),
    );
    assert_eq!(m.extract_dir.as_deref(), Some("PFiles64/App"));
}

#[test]
fn bin_string_array_and_nested_alias() {
    let m = ok(
        "multi",
        json!({
            "version": "1.0",
            "url": "https://x/a.zip",
            "hash": "b".repeat(64),
            "bin": [
                "a.exe",
                ["b.exe", "bee"],
                ["c.exe", "cee", "--flag --x"],
                ["d.exe"]
            ]
        }),
    );
    assert_eq!(m.bin.len(), 4);
    assert_eq!(m.bin[0], Bin::Path("a.exe".into()));
    assert_eq!(
        m.bin[1],
        Bin::Table {
            name: "bee".into(),
            path: "b.exe".into(),
            args: None
        }
    );
    assert_eq!(
        m.bin[2],
        Bin::Table {
            name: "cee".into(),
            path: "c.exe".into(),
            args: Some("--flag --x".into())
        }
    );
    // Nested single-element degrades to a plain path.
    assert_eq!(m.bin[3], Bin::Path("d.exe".into()));
}

#[test]
fn bin_nested_args_array_joined() {
    let m = ok(
        "alist",
        json!({
            "version": "1.0",
            "url": "https://x/a.zip",
            "hash": "b".repeat(64),
            "bin": [["alist.exe", "alist", ["--force", "--bin-dir"]]]
        }),
    );
    assert_eq!(
        m.bin[0],
        Bin::Table {
            name: "alist".into(),
            path: "alist.exe".into(),
            args: Some("--force --bin-dir".into())
        }
    );
}

#[test]
fn bin_backslash_subpath_ok() {
    let m = ok(
        "apngasm",
        json!({
            "version": "1.0",
            "url": "https://x/a.zip",
            "hash": "b".repeat(64),
            "bin": [["bin\\apngasm.exe", "apngasm-cli"]]
        }),
    );
    assert_eq!(m.bin[0].path(), "bin\\apngasm.exe");
}

#[test]
fn extract_dir_mapped() {
    let m = ok(
        "fd",
        json!({
            "version": "1.0",
            "url": "https://x/a.zip",
            "hash": "b".repeat(64),
            "extract_dir": "fd-1.0"
        }),
    );
    assert_eq!(m.extract_dir.as_deref(), Some("fd-1.0"));
}

#[test]
fn env_add_path_mapping() {
    let m = ok(
        "node",
        json!({
            "version": "1.0",
            "url": "https://x/a.7z",
            "hash": "b".repeat(64),
            "env_add_path": ["bin", "."]
        }),
    );
    // "." → {dir}, "bin" → {dir}\bin, joined with ';'.
    assert_eq!(m.env.get("PATH").unwrap(), "{dir}\\bin;{dir}");
}

#[test]
fn env_set_dir_template() {
    let m = ok(
        "temurin",
        json!({
            "version": "1.0",
            "url": "https://x/a.zip",
            "hash": "b".repeat(64),
            "env_set": { "JAVA_HOME": "$dir", "JAVA_OPTS": "$dir\\lib" }
        }),
    );
    assert_eq!(m.env.get("JAVA_HOME").unwrap(), "{dir}");
    assert_eq!(m.env.get("JAVA_OPTS").unwrap(), "{dir}\\lib");
}

#[test]
fn persist_string_and_array() {
    let m = ok(
        "app",
        json!({
            "version": "1.0",
            "url": "https://x/a.zip",
            "hash": "b".repeat(64),
            "persist": ["config", "data"]
        }),
    );
    assert_eq!(m.persist, vec!["config", "data"]);

    let m2 = ok(
        "app",
        json!({ "version": "1.0", "url": "https://x/a.zip", "hash": "b".repeat(64), "persist": "config" }),
    );
    assert_eq!(m2.persist, vec!["config"]);
}

#[test]
fn depends_strips_bucket_prefix() {
    let m = ok(
        "app",
        json!({
            "version": "1.0",
            "url": "https://x/a.zip",
            "hash": "b".repeat(64),
            "depends": ["extras/vcredist2022", "7zip"]
        }),
    );
    assert_eq!(m.depends.get("vcredist2022").map(String::as_str), Some("*"));
    assert_eq!(m.depends.get("7zip").map(String::as_str), Some("*"));
}

#[test]
fn hash_prefix_sha256_handled() {
    let m = ok(
        "app",
        json!({
            "version": "1.0",
            "url": "https://x/a.zip",
            "hash": format!("sha256:{}", "F".repeat(64))
        }),
    );
    // Prefix stripped and lowercased.
    assert_eq!(m.source.x64.unwrap().hash(), "f".repeat(64));
}

#[test]
fn license_object_identifier() {
    let m = ok(
        "app",
        json!({
            "version": "1.0",
            "url": "https://x/a.zip",
            "hash": "b".repeat(64),
            "license": { "identifier": "BSD-2-Clause", "url": "https://x/l" }
        }),
    );
    assert_eq!(m.license.as_deref(), Some("BSD-2-Clause"));
}

#[test]
fn autoupdate_github_from_checkver_object() {
    match convert(
        "jq",
        &json!({
            "version": "1.0",
            "url": "https://x/a.zip",
            "hash": "b".repeat(64),
            "checkver": { "github": "https://github.com/jqlang/jq", "regex": "x" }
        }),
    ) {
        Outcome::Ok(c) => {
            assert!(c.toml.contains("[autoupdate]"));
            assert!(c.toml.contains(r#"github = "jqlang/jq""#), "{}", c.toml);
            Manifest::from_toml_str(&c.toml).unwrap();
        }
        Outcome::Skip(r) => panic!("skip {r}"),
    }
}

#[test]
fn autoupdate_github_from_homepage() {
    match convert(
        "fzf",
        &json!({
            "version": "1.0",
            "homepage": "https://github.com/junegunn/fzf",
            "url": "https://x/a.zip",
            "hash": "b".repeat(64),
            "checkver": "github"
        }),
    ) {
        Outcome::Ok(c) => assert!(c.toml.contains(r#"github = "junegunn/fzf""#), "{}", c.toml),
        Outcome::Skip(r) => panic!("skip {r}"),
    }
}

#[test]
fn autoupdate_vendor_url_regex_is_declarative() {
    match convert(
        "brave",
        &json!({
            "version": "1.0",
            "url": "https://x/brave-1.0.zip",
            "hash": "b".repeat(64),
            "checkver": {
                "url": "https://example.com/latest.version",
                "regex": "([\\d.]+)"
            }
        }),
    ) {
        Outcome::Ok(c) => assert!(
            c.toml.contains(
                r#"checkver = { url = "https://example.com/latest.version", regex = "([\\d.]+)" }"#
            ),
            "{}",
            c.toml
        ),
        Outcome::Skip(r) => panic!("skip {r}"),
    }
}

#[test]
fn autoupdate_preserves_architecture_url_templates() {
    match convert(
        "tool",
        &json!({
            "version": "1.0",
            "homepage": "https://github.com/example/tool",
            "architecture": {
                "64bit": {
                    "url": "https://example.com/tool-1.0-x64.zip",
                    "hash": "b".repeat(64)
                },
                "arm64": {
                    "url": "https://example.com/tool-1.0-arm64.zip",
                    "hash": "c".repeat(64)
                }
            },
            "checkver": "github",
            "autoupdate": {
                "architecture": {
                    "64bit": { "url": "https://example.com/tool-$version-x64.zip" },
                    "arm64": { "url": "https://example.com/tool-$version-arm64.zip" }
                }
            }
        }),
    ) {
        Outcome::Ok(c) => assert!(
            c.toml.contains(
                r#"url_template = { x64 = "https://example.com/tool-$version-x64.zip", arm64 = "https://example.com/tool-$version-arm64.zip" }"#
            ),
            "{}",
            c.toml
        ),
        Outcome::Skip(r) => panic!("skip {r}"),
    }
}

#[test]
fn autoupdate_googlechrome_uses_clean_room_vendor_resolver() {
    match convert(
        "googlechrome",
        &json!({
            "version": "1.0",
            "url": "https://x/chrome-1.0.zip",
            "hash": "b".repeat(64),
            "checkver": { "script": ["do not import"], "regex": "(.+)" }
        }),
    ) {
        Outcome::Ok(c) => assert!(
            c.toml
                .contains(r#"checkver = { vendor = "google-chrome" }"#),
            "{}",
            c.toml
        ),
        Outcome::Skip(r) => panic!("skip {r}"),
    }
}

// --- skip reasons --------------------------------------------------------

#[test]
fn skip_script_fields() {
    for field in [
        "pre_install",
        "installer",
        "uninstaller",
        "psmodule",
        "pre_uninstall",
    ] {
        let v = json!({
            "version": "1.0",
            "url": "https://x/a.zip",
            "hash": "b".repeat(64),
            field: ["do something"]
        });
        assert_eq!(skip_reason("s", v), "script-field", "field {field}");
    }
}

#[test]
fn benign_post_install_allowed() {
    // post_install and post_uninstall are benign — dropped, not skipped.
    let v = json!({
        "version": "1.0",
        "url": "https://x/a.zip",
        "hash": "b".repeat(64),
        "post_install": ["Write-Host 'config tweak'"],
        "post_uninstall": ["Remove-Item 'config.ini'"]
    });
    let m = ok("s", v);
    assert_eq!(m.version, "1.0");
}

#[test]
fn skip_nested_arch_script() {
    // 7zip pattern: arm64 has pre_install.
    let v = json!({
        "version": "1.0",
        "architecture": {
            "64bit": { "url": "https://x/a.zip", "hash": "b".repeat(64) },
            "arm64": { "url": "https://x/b.zip", "hash": "c".repeat(64), "pre_install": ["x"] }
        }
    });
    assert_eq!(skip_reason("s", v), "script-field");
}

#[test]
fn explicit_installer_archives_are_accepted() {
    let exe = ok(
        "s",
        json!({
            "version": "1.0",
            "url": "https://x/setup.exe",
            "hash": "b".repeat(64),
            "innosetup": true,
            "bin": "app.exe"
        }),
    );
    assert_eq!(
        exe.source.x64.as_ref().unwrap().kind,
        SourceKind::InstallerArchive
    );

    let msi = ok(
        "s",
        json!({
            "version": "1.0",
            "url": "https://x/app.msi",
            "hash": "b".repeat(64),
            "shortcuts": [["PFiles/App/app.exe", "App"]]
        }),
    );
    assert_eq!(
        msi.source.x64.as_ref().unwrap().kind,
        SourceKind::InstallerArchive
    );
    assert_eq!(
        msi.shortcuts,
        vec![Shortcut::Table {
            target: "PFiles/App/app.exe".into(),
            name: "App".into()
        }]
    );
}

#[test]
fn standalone_exe_is_not_treated_as_an_installer() {
    for url in ["https://x/tool.exe", "https://x/tool-amd64.exe#/tool.exe"] {
        let v = json!({
            "version": "1.0",
            "url": url,
            "hash": "b".repeat(64),
            "bin": "tool.exe"
        });
        assert_eq!(skip_reason("s", v), "standalone-binary");
    }
}

#[test]
fn installer_without_launch_entry_is_skipped() {
    let v = json!({
        "version": "1.0",
        "url": "https://x/app.msi",
        "hash": "b".repeat(64)
    });
    assert_eq!(skip_reason("s", v), "no-launch-entry");
}

#[test]
fn shortcut_arguments_are_not_silently_dropped() {
    let v = json!({
        "version": "1.0",
        "url": "https://x/app.msi",
        "hash": "b".repeat(64),
        "shortcuts": [["app.exe", "App", "--portable"]]
    });
    assert_eq!(skip_reason("s", v), "shortcut-args");
}

#[test]
fn installer_key_still_blocked() {
    // Manifests with an explicit `installer` key are still blocked
    // (they need to be RUN, not extracted).
    let v = json!({
        "version": "1.0",
        "url": "https://x/setup.exe",
        "hash": "b".repeat(64),
        "installer": { "script": ["Start-Process"] }
    });
    assert_eq!(skip_reason("s", v), "script-field");
}

#[test]
fn archive_with_fragment_not_skipped() {
    // #/name.7z fragment marks an archive → keep.
    let m = ok(
        "app",
        json!({ "version": "1.0", "url": "https://x/download?id=5#/app.7z", "hash": "b".repeat(64) }),
    );
    assert_eq!(m.source.x64.unwrap().url, "https://x/download?id=5#/app.7z");
}

#[test]
fn skip_no_url() {
    let v = json!({ "version": "1.0", "description": "x" });
    assert_eq!(skip_reason("s", v), "no-url");
}

#[test]
fn skip_only_32bit() {
    let v = json!({
        "version": "1.0",
        "architecture": { "32bit": { "url": "https://x/a.zip", "hash": "b".repeat(64) } }
    });
    assert_eq!(skip_reason("s", v), "no-64bit-source");
}

#[test]
fn skip_no_hash() {
    let v = json!({ "version": "1.0", "url": "https://x/a.zip" });
    assert_eq!(skip_reason("s", v), "no-hash");
}

#[test]
fn skip_unsupported_hash() {
    // md5 (32 hex) by length — still rejected.
    let md5 = json!({ "version": "1.0", "url": "https://x/a.zip", "hash": "b".repeat(32) });
    assert_eq!(skip_reason("s", md5), "unsupported-hash");
    // sha1 by prefix — still rejected.
    let sha1 = json!({ "version": "1.0", "url": "https://x/a.zip", "hash": format!("sha1:{}", "b".repeat(40)) });
    assert_eq!(skip_reason("s", sha1), "unsupported-hash");
}

#[test]
fn sha512_now_accepted() {
    // sha512 by prefix — now accepted (round 2).
    let m = ok(
        "s",
        json!({ "version": "1.0", "url": "https://x/a.zip", "hash": format!("sha512:{}", "b".repeat(128)) }),
    );
    assert!(m.source.x64.as_ref().unwrap().is_sha512());
    assert_eq!(m.source.x64.unwrap().hash(), "b".repeat(128));
    // bare 128-hex — also accepted.
    let m2 = ok(
        "s",
        json!({ "version": "1.0", "url": "https://x/a.zip", "hash": "b".repeat(128) }),
    );
    assert!(m2.source.x64.as_ref().unwrap().is_sha512());
}

#[test]
fn skip_multi_url() {
    let v = json!({
        "version": "1.0",
        "url": ["https://x/a.zip", "https://x/b.zip"],
        "hash": ["b".repeat(64), "c".repeat(64)]
    });
    assert_eq!(skip_reason("s", v), "multi-url");
}

#[test]
fn skip_persist_nested() {
    let v = json!({
        "version": "1.0",
        "url": "https://x/a.zip",
        "hash": "b".repeat(64),
        "persist": [["src", "dst"]]
    });
    assert_eq!(skip_reason("s", v), "persist-nested");
}

#[test]
fn skip_env_unmappable_scoop_var() {
    let v = json!({
        "version": "1.0",
        "url": "https://x/a.zip",
        "hash": "b".repeat(64),
        "env_set": { "PREFIX": "$persist_dir\\bin" }
    });
    assert_eq!(skip_reason("s", v), "env-unmappable");
}

#[test]
fn skip_invalid_name() {
    // Underscore is not allowed by voli's name rule.
    let v = json!({ "version": "1.0", "url": "https://x/a.zip", "hash": "b".repeat(64) });
    assert_eq!(skip_reason("bad_name", v), "invalid-name");
}

#[test]
fn escaping_backslashes_round_trips() {
    // Windows paths in env values must be escaped correctly.
    let c = match convert(
        "app",
        &json!({
            "version": "1.0",
            "url": "https://x/a.zip",
            "hash": "b".repeat(64),
            "env_set": { "HOME": "$dir\\a\\b\\c" }
        }),
    ) {
        Outcome::Ok(c) => c,
        Outcome::Skip(r) => panic!("skip {r}"),
    };
    assert!(c.toml.contains(r#"HOME = "{dir}\\a\\b\\c""#), "{}", c.toml);
    let m = Manifest::from_toml_str(&c.toml).unwrap();
    assert_eq!(m.env.get("HOME").unwrap(), "{dir}\\a\\b\\c");
}
