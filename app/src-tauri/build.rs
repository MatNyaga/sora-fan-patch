// Embeds the release into the app at compile time: release.json + patch.vcdiff (formats 1 and 2)
// or release.json + packs/*.bin + sets/*.vcdiff (format 3, pack release with the fighter checklist).
// SORAPATCH_RELEASE_DIR=<dir> picks it; the default is the synthetic test release made by
// tests/run_synthetic.sh (never game data). A release marked "local_test" (it holds the engine test
// fighter) is refused unless SORAPATCH_ALLOW_LOCAL_TEST=1 (local proofs only, never shared).
use std::path::{Path, PathBuf};

fn files_under(dir: &Path, sub: &str, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir.join(sub)) else { return };
    let mut v: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    v.sort();
    for p in v {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if !name.starts_with('.') {
            out.push((format!("{sub}/{name}"), p));
        }
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=SORAPATCH_RELEASE_DIR");
    println!("cargo:rerun-if-env-changed=SORAPATCH_ALLOW_LOCAL_TEST");
    let dir = std::env::var("SORAPATCH_RELEASE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../../testdata/rel-default"));
    let dir = dir.canonicalize().unwrap_or_else(|_| {
        panic!("release dir {} not found: run tests/run_synthetic.sh (installer workspace root) or set SORAPATCH_RELEASE_DIR", dir.display())
    });
    let m = dir.join("release.json");
    assert!(m.is_file(), "missing {}", m.display());
    println!("cargo:rerun-if-changed={}", m.display());
    let text = std::fs::read_to_string(&m).unwrap();
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains("\"local_test\":true") && std::env::var("SORAPATCH_ALLOW_LOCAL_TEST").as_deref() != Ok("1") {
        panic!("{} is a LOCAL TEST release (engine test fighter): never build a shareable app from it", m.display());
    }
    let mut files = Vec::new();
    let p = dir.join("patch.vcdiff");
    if p.is_file() {
        files.push(("patch.vcdiff".to_string(), p));
    }
    files_under(&dir, "packs", &mut files);
    files_under(&dir, "sets", &mut files);
    assert!(!files.is_empty(), "no patch.vcdiff and no packs/ sets/ in {}", dir.display());
    for (_, f) in &files {
        println!("cargo:rerun-if-changed={}", f.display());
    }
    let list: String = files
        .iter()
        .map(|(k, f)| format!("    ({:?}, include_bytes!({:?})),\n", k, f.display().to_string()))
        .collect();
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("release.rs");
    std::fs::write(
        &out,
        format!(
            "pub const MANIFEST: &str = include_str!({:?});\npub const FILES: &[(&str, &[u8])] = &[\n{list}];\n",
            m.display().to_string()
        ),
    )
    .unwrap();
    tauri_build::build();
}
