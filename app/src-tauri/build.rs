// Embeds the release (release.json + patch.vcdiff) into the app at compile time.
// SORAPATCH_RELEASE_DIR=<dir> picks it; the default is the synthetic test release made by
// tests/run_synthetic.sh (never game data).
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=SORAPATCH_RELEASE_DIR");
    let dir = std::env::var("SORAPATCH_RELEASE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../../testdata/rel-default"));
    let dir = dir.canonicalize().unwrap_or_else(|_| {
        panic!("release dir {} not found: run tests/run_synthetic.sh (installer workspace root) or set SORAPATCH_RELEASE_DIR", dir.display())
    });
    let (m, p) = (dir.join("release.json"), dir.join("patch.vcdiff"));
    for f in [&m, &p] {
        assert!(f.is_file(), "missing {}", f.display());
        println!("cargo:rerun-if-changed={}", f.display());
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("release.rs");
    std::fs::write(
        &out,
        format!(
            "pub const MANIFEST: &str = include_str!({:?});\npub const DELTA: &[u8] = include_bytes!({:?});\n",
            m.display().to_string(),
            p.display().to_string()
        ),
    )
    .unwrap();
    tauri_build::build();
}
