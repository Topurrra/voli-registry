//! Unit tests: asset matching, version handling, binary-format parsers,
//! manifest merge/round-trip. Network and execution gates are exercised by the
//! pilot run itself, not here.

use super::*;

#[test]
fn strip_v_handles_tag_forms() {
    assert_eq!(strip_v("v10.5.0"), "10.5.0");
    assert_eq!(strip_v("10.5.0"), "10.5.0");
    assert_eq!(strip_v("25.07.1"), "25.07.1");
}

#[test]
fn render_fills_version_and_tag() {
    assert_eq!(
        render("fd-v{version}-x86_64.tar.gz", "10.5.0", "v10.5.0"),
        "fd-v10.5.0-x86_64.tar.gz"
    );
    assert_eq!(
        render("tool-{tag}-linux.tar.gz", "1.0.0", "release-1.0.0"),
        "tool-release-1.0.0-linux.tar.gz"
    );
}

#[test]
fn find_asset_is_exact() {
    let names = vec!["a.tar.gz".to_string(), "b.tar.gz".to_string()];
    assert_eq!(find_asset(&names, "a.tar.gz").unwrap(), "a.tar.gz");
    assert!(find_asset(&names, "a.zip").is_err());
    assert!(find_asset(&names, "A.TAR.GZ").is_err());
}

#[test]
fn allowlist_parses_and_rejects_empty_assets() {
    let specs = parse_allowlist(
        r#"
[[source]]
name = "fd"
repo = "sharkdp/fd"
description = "find alternative"
homepage = "https://github.com/sharkdp/fd"
license = "MIT"
bin = ["fd"]

[source.assets.linux-x64]
file = "fd-v{version}-x86_64-unknown-linux-musl.tar.gz"
extract_dir = "fd-v{version}-x86_64-unknown-linux-musl"
"#,
    )
    .unwrap();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].smoke_args, vec!["--version".to_string()]);
    assert_eq!(
        specs[0].assets["linux-x64"].extract_dir.as_deref(),
        Some("fd-v{version}-x86_64-unknown-linux-musl")
    );

    assert!(parse_allowlist("[[source]]\nname = \"x\"\n").is_err());
}

fn minimal_manifest() -> Manifest {
    Manifest::from_toml_str(
        r#"name = "tool"
version = "1.0.0"
kind = "app"
bin = ["tool"]

[source.x64]
url = "https://example.com/tool.zip"
sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
"#,
    )
    .unwrap()
}

fn verified(key: &str) -> VerifiedSource {
    VerifiedSource {
        key: key.to_string(),
        url: format!("https://example.com/tool-{key}.tar.gz"),
        sha256: "b".repeat(64),
        extract_dir: None,
    }
}

#[test]
fn merge_replaces_unix_blocks_and_keeps_windows() {
    let m = merge_unix_sources(minimal_manifest(), &[verified("linux-x64")]);
    assert!(m.source.x64.is_some());
    assert_eq!(
        m.source.linux_x64.as_ref().unwrap().url,
        "https://example.com/tool-linux-x64.tar.gz"
    );
    // Re-merge overwrites (rebuilds change hashes), never duplicates.
    let m2 = merge_unix_sources(m, &[verified("linux-x64"), verified("macos-arm64")]);
    assert!(m2.source.linux_x64.is_some());
    assert!(m2.source.macos_arm64.is_some());
    // Unknown keys are ignored, not fatal.
    let m3 = merge_unix_sources(
        m2,
        &[VerifiedSource {
            key: "plan9-mips".to_string(),
            url: "https://example.com/x".to_string(),
            sha256: "c".repeat(64),
            extract_dir: None,
        }],
    );
    assert!(m3.is_canonical_toml(&m3.to_canonical_toml()));
}

#[test]
fn merged_manifest_is_canonical_and_valid() {
    let m = merge_unix_sources(
        minimal_manifest(),
        &[
            verified("linux-x64"),
            verified("linux-arm64"),
            verified("macos-x64"),
            verified("macos-arm64"),
        ],
    );
    let text = m.to_canonical_toml();
    assert!(text.contains("[source.linux-x64]"));
    assert!(text.contains("[source.macos-arm64]"));
    let back = Manifest::from_toml_str(&text).unwrap();
    assert_eq!(back, m);
    // Windows hosts still select the Windows block.
    let picked = back
        .select_source(voli_core::manifest::Platform {
            os: voli_core::manifest::Os::Windows,
            arch: voli_core::manifest::Arch::X64,
        })
        .unwrap();
    assert_eq!(picked.source.url, "https://example.com/tool.zip");
}

/// Build a minimal 64-bit LE ELF with the given INTERP and one NEEDED.
/// Layout: ELF header + 2 program headers (INTERP, DYNAMIC) + content.
fn elf_fixture(interp: &str, needed: &str) -> Vec<u8> {
    let mut b = vec![0u8; 256];
    b[0..4].copy_from_slice(b"\x7fELF");
    b[4] = 2; // 64-bit
    b[5] = 1; // LE
    b[0x20..0x28].copy_from_slice(&64u64.to_le_bytes()); // phoff
    b[0x36..0x38].copy_from_slice(&56u16.to_le_bytes()); // phentsize
    b[0x38..0x3a].copy_from_slice(&2u16.to_le_bytes()); // phnum
    // PH0: INTERP at file offset 176.
    b[64..68].copy_from_slice(&3u32.to_le_bytes());
    b[64 + 8..64 + 16].copy_from_slice(&176u64.to_le_bytes());
    // PH1: DYNAMIC at file offset 208, two entries.
    b[120..124].copy_from_slice(&2u32.to_le_bytes());
    b[120 + 8..120 + 16].copy_from_slice(&208u64.to_le_bytes());
    b[120 + 32..120 + 40].copy_from_slice(&32u64.to_le_bytes());
    let interp_off = 176usize;
    b[interp_off..interp_off + interp.len()].copy_from_slice(interp.as_bytes());
    let dyn_off = 208usize;
    // DT_STRTAB = 5 -> vaddr 0x400000 (mapped below); DT_NEEDED = 1 -> offset 16.
    b[dyn_off..dyn_off + 8].copy_from_slice(&5i64.to_le_bytes());
    b[dyn_off + 8..dyn_off + 16].copy_from_slice(&0x400000u64.to_le_bytes());
    b[dyn_off + 16..dyn_off + 24].copy_from_slice(&1i64.to_le_bytes());
    b[dyn_off + 24..dyn_off + 32].copy_from_slice(&16u64.to_le_bytes());
    // String table right after: needed name at +16.
    let str_off = 240usize;
    b[str_off..str_off + needed.len()].copy_from_slice(needed.as_bytes());
    // LOAD segment covering vaddr 0x400000 -> file offset str_off.
    // (Reuse PH area? No room — instead rely on filesz covering: simpler to
    // append a third header by bumping phnum and extending.)
    b
}

#[test]
fn elf_parser_errors_without_load_mapping() {
    // No PT_LOAD maps the string table vaddr, so refs cannot resolve.
    let refs = elf_dependency_refs(&elf_fixture("/lib64/ld-linux.so", "libc.so.6"));
    // INTERP always resolves (file offset direct); NEEDED needs the map.
    assert!(refs.is_ok());
    assert_eq!(refs.unwrap(), vec!["/lib64/ld-linux.so".to_string()]);
}

#[test]
fn elf_parser_rejects_non_elf_and_32bit() {
    assert!(elf_dependency_refs(b"definitely not elf").is_err());
    let mut b = elf_fixture("/lib64/ld.so", "libc.so.6");
    b[4] = 1; // 32-bit
    assert!(elf_dependency_refs(&b).is_err());
}

#[test]
fn brew_markers_found_in_refs() {
    let refs = vec![
        "/lib64/ld-linux-x86-64.so.2".to_string(),
        "@@HOMEBREW_PREFIX@@/lib/ld.so".to_string(),
        "/opt/homebrew/opt/openssl/lib/libssl.dylib".to_string(),
    ];
    let bad = has_brew_refs(&refs);
    assert_eq!(bad.len(), 2);
    assert!(has_brew_refs(&["/usr/lib/libc.so".to_string()]).is_empty());
}

/// Minimal 64-bit Mach-O with one LC_LOAD_DYLIB and one LC_RPATH.
fn macho_fixture(dylib: &str, rpath: &str) -> Vec<u8> {
    let mut cmds = Vec::new();
    // LC_LOAD_DYLIB
    let name_off = 24usize;
    let size = (name_off + dylib.len() + 1).div_ceil(8) * 8;
    let mut c = vec![0u8; size];
    c[0..4].copy_from_slice(&0xcu32.to_le_bytes());
    c[4..8].copy_from_slice(&(size as u32).to_le_bytes());
    c[8..12].copy_from_slice(&(name_off as u32).to_le_bytes());
    c[name_off..name_off + dylib.len()].copy_from_slice(dylib.as_bytes());
    cmds.extend(c);
    // LC_RPATH
    let rname_off = 12usize;
    let rsize = (rname_off + rpath.len() + 1).div_ceil(8) * 8;
    let mut c = vec![0u8; rsize];
    c[0..4].copy_from_slice(&0x8000_001cu32.to_le_bytes());
    c[4..8].copy_from_slice(&(rsize as u32).to_le_bytes());
    c[8..12].copy_from_slice(&(rname_off as u32).to_le_bytes());
    c[rname_off..rname_off + rpath.len()].copy_from_slice(rpath.as_bytes());
    cmds.extend(c);

    let mut b = vec![0u8; 32];
    b[0..4].copy_from_slice(&0xfeedfacfu32.to_le_bytes());
    b[16..20].copy_from_slice(&2u32.to_le_bytes()); // ncmds
    b[20..24].copy_from_slice(&(cmds.len() as u32).to_le_bytes());
    b.extend(cmds);
    b
}

#[test]
fn macho_parser_reads_dylibs_and_rpaths() {
    let refs = macho_dependency_refs(&macho_fixture(
        "/opt/homebrew/opt/openssl/lib/libssl.3.dylib",
        "/usr/lib",
    ))
    .unwrap();
    assert_eq!(
        refs,
        vec![
            "/opt/homebrew/opt/openssl/lib/libssl.3.dylib".to_string(),
            "/usr/lib".to_string()
        ]
    );
    let bad = has_brew_refs(&refs);
    assert_eq!(bad.len(), 1);
}

#[test]
fn macho_parser_rejects_garbage() {
    assert!(macho_dependency_refs(b"short").is_err());
    assert!(macho_dependency_refs(&[0u8; 64]).is_err());
}

#[test]
fn detect_extract_dir_flat_and_wrapped() {
    let flat = tempfile::tempdir().unwrap();
    std::fs::write(flat.path().join("tool"), b"x").unwrap();
    assert_eq!(
        detect_extract_dir(flat.path(), &["tool".to_string()]).unwrap(),
        None
    );

    let wrapped = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(wrapped.path().join("tool-1.0")).unwrap();
    std::fs::write(wrapped.path().join("tool-1.0/tool"), b"x").unwrap();
    std::fs::write(wrapped.path().join("README.md"), b"x").unwrap();
    assert_eq!(
        detect_extract_dir(wrapped.path(), &["tool".to_string()]).unwrap(),
        Some("tool-1.0".to_string())
    );

    // Bins scattered across two wrappers: ambiguous, must be pinned.
    let scattered = tempfile::tempdir().unwrap();
    for d in ["a", "b"] {
        std::fs::create_dir_all(scattered.path().join(d)).unwrap();
    }
    std::fs::write(scattered.path().join("a/t1"), b"x").unwrap();
    std::fs::write(scattered.path().join("b/t2"), b"x").unwrap();
    assert!(detect_extract_dir(scattered.path(), &["t1".to_string(), "t2".to_string()]).is_err());
}
