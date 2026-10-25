//! Core of the fan-patch installer: check the player's file, then build the patched copy.
//!
//! Rules (player-facing UX spec): the player's file is only ever READ; the new file is written
//! under a hidden temporary name next to it and renamed only after its SHA-256 matches the
//! release manifest; any failure or cancel removes the temporary file. Every problem has a
//! stable `code` that the UI turns into plain words (app/ui/strings.en.js).

pub mod pspcrypt;
pub mod vcdiff;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------- release manifest

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscInfo {
    /// Game id inside the disc image (UMD_DATA.BIN), e.g. "ULUS-10566".
    pub disc_id: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputInfo {
    pub file_name: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OlderOutput {
    pub version: String,
    pub sha256: String,
    /// Format 3: the fighters that older file has, in plain words ("Sora"); may be empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub names: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub title: String,
    pub version: String,
    pub input: DiscInfo,
    /// Formats 1 and 2: the one result file. Format 3 has one result per fighter set (`sets`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputInfo>,
    #[serde(default)]
    pub older_outputs: Vec<OlderOutput>,
    /// Other releases of the same game: disc id -> region word shown to the player.
    #[serde(default)]
    pub other_regions: BTreeMap<String, String>,
    /// Format 2 (and 3): what the patch's source is, when it is not the player's file byte for byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceInfo>,
    /// Format 3 (P2.11 pack release): the engine pack + the fighter packs of the checklist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packs: Vec<PackInfo>,
    /// Format 3: one entry per fighter mix the player can tick.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sets: Vec<SetInfo>,
    /// Format 3: a developer-only release (holds the engine test fighter); never shared.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub local_test: bool,
}

/// A file of a format-3 release folder (also embedded into the app), checked by size + SHA-256.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DataFile {
    pub file: String,
    pub size: u64,
    pub sha256: String,
}

/// Format 3: one pack. The set patch COPYs from the pack's data ("pack data"): the sectors of a
/// one-fighter result disc that hold the pack's own bytes. `data` is the shipped file: either that
/// pack data itself (no `blob`), or (with `blob`) a VCDIFF that rebuilds it from the player's disc
/// followed by the pack data of `blob_source` (so only the pack's own bytes are shipped).
/// The engine pack (`always`) is part of every set and is not a checkbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackInfo {
    pub id: String,
    /// "engine" or "fighter".
    pub kind: String,
    /// Name the player sees in the checklist ("Sora").
    pub name: String,
    #[serde(default)]
    pub blurb: String,
    #[serde(default)]
    pub always: bool,
    /// The engine test fighter: only ever in a local_test release.
    #[serde(default)]
    pub test: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<u32>,
    pub version: String,
    pub package_sha256: String,
    pub data: DataFile,
    /// Size + SHA-256 of the pack data `data` rebuilds (when `data` is a VCDIFF).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<BlobInfo>,
    /// Packs whose pack data follows the player's disc in the source of `data` (e.g. ["engine"]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blob_source: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlobInfo {
    pub size: u64,
    pub sha256: String,
}

/// Format 3: the result for one fighter mix. Source = player's disc (with `source` applied) followed
/// by the data of `source_packs` in that order; `patch` turns it into the result file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetInfo {
    /// Ticked fighter pack ids (owner order).
    pub packs: Vec<String>,
    /// "engine" first, then `packs`.
    pub source_packs: Vec<String>,
    pub file_name: String,
    pub size: u64,
    pub sha256: String,
    /// Roster id (8 hex digits) the finished disc carries in EXTRA.REG +0x40 and EXTRA.IDX +0x20.
    pub roster_id: String,
    pub patch: DataFile,
}

/// What one build makes: the result file, and for format 3 the fighter set it comes from.
#[derive(Debug, Clone)]
pub struct Target {
    pub output: OutputInfo,
    pub set: Option<SetInfo>,
}

/// Plain-English list: "Sora", "Sora and X", "Sora, X and Y".
pub fn join_names(names: &[String]) -> String {
    match names.len() {
        0 => String::new(),
        1 => names[0].clone(),
        n => format!("{} and {}", names[..n - 1].join(", "), names[n - 1]),
    }
}

/// Format-2 patch source `"kind": "plain_eboot"`: the player's (already checked) clean disc with
/// the bytes of its encrypted EBOOT.BIN replaced, from `offset`, by that EBOOT's decryption
/// (pspcrypt.rs). Same size as the disc; only `plain_size` bytes differ. So the release never has
/// to carry the game's plain code: COPY instructions read it from the player's own decrypted EBOOT.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    pub kind: String,
    /// ISO path of the encrypted file, e.g. "PSP_GAME/SYSDIR/EBOOT.BIN".
    pub file: String,
    /// Byte offset of that file in the disc image (sector * 2048).
    pub offset: u64,
    pub encrypted_size: u64,
    pub encrypted_sha256: String,
    pub plain_size: u64,
    pub plain_sha256: String,
}

pub const SOURCE_PLAIN_EBOOT: &str = "plain_eboot";
pub const EBOOT_PATH: &str = "PSP_GAME/SYSDIR/EBOOT.BIN";

pub struct Release {
    pub manifest: Manifest,
    /// The VCDIFF patch (formats 1 and 2; empty for format 3); `Borrowed` when embedded in the app
    /// binary (no copy in RAM).
    pub delta: Cow<'static, [u8]>,
    /// Format 3: every file the manifest names (packs/*.bin, sets/*.vcdiff), by release path.
    pub files: BTreeMap<String, Cow<'static, [u8]>>,
}

pub const PATCH_FILE: &str = "patch.vcdiff";
/// Largest pack data the app rebuilds in memory (one pack); a release maker keeps far below it.
pub const MAX_PACK_DATA: u64 = 512 << 20;

/// A release path must be a plain relative name under packs/ or sets/ (no "..", no absolute path).
fn release_path_ok(p: &str) -> bool {
    let mut it = p.split('/');
    matches!((it.next(), it.next(), it.next()), (Some("packs" | "sets"), Some(n), None)
        if !n.is_empty() && !n.starts_with('.') && !n.contains('\\'))
}

impl Release {
    /// Formats 1 and 2: release.json + patch.vcdiff.
    pub fn from_parts(manifest_json: &str, delta: impl Into<Cow<'static, [u8]>>) -> Result<Self, String> {
        let mut files = BTreeMap::new();
        files.insert(PATCH_FILE.to_string(), delta.into());
        Self::from_files(manifest_json, files)
    }

    /// Any format: release.json + the files of the release folder by path ("patch.vcdiff",
    /// "packs/sora.bin", "sets/sora.vcdiff"). Extra files are ignored; a missing or wrong-sized one is an error.
    pub fn from_files(manifest_json: &str, mut files: BTreeMap<String, Cow<'static, [u8]>>) -> Result<Self, String> {
        let manifest: Manifest = serde_json::from_str(manifest_json).map_err(|e| e.to_string())?;
        let src_ok = |s: &SourceInfo| s.kind == SOURCE_PLAIN_EBOOT && s.plain_size <= s.encrypted_size;
        match (manifest.format, &manifest.source) {
            (1, None) => {}
            (2, Some(s)) | (3, Some(s)) if src_ok(s) => {}
            (3, None) => {}
            (2 | 3, Some(s)) => return Err(format!("unknown or bad patch source kind {:?}", s.kind)),
            (2, None) => return Err("manifest format 2 needs a \"source\"".into()),
            (f, _) => return Err(format!("unknown manifest format {f}")),
        }
        if manifest.format < 3 {
            if manifest.output.is_none() || !manifest.packs.is_empty() || !manifest.sets.is_empty() {
                return Err("manifest format 1/2 needs \"output\" and no packs/sets".into());
            }
            let delta = files.remove(PATCH_FILE).ok_or("missing patch.vcdiff")?;
            return Ok(Release { manifest, delta, files: BTreeMap::new() });
        }
        Self::check_packs(&manifest)?;
        let mut keep = BTreeMap::new();
        let need = manifest.packs.iter().map(|p| &p.data).chain(manifest.sets.iter().map(|s| &s.patch));
        for d in need {
            let b = files.remove(&d.file).ok_or_else(|| format!("missing release file {}", d.file))?;
            if b.len() as u64 != d.size {
                return Err(format!("release file {} is {} B, manifest says {}", d.file, b.len(), d.size));
            }
            keep.insert(d.file.clone(), b);
        }
        Ok(Release { manifest, delta: Cow::Borrowed(&[]), files: keep })
    }

    /// Format 3 consistency: one engine pack (always on, first); fighter ids unique; every set
    /// names known fighters, its source = engine + its packs, a roster id and a sets/ patch.
    fn check_packs(m: &Manifest) -> Result<(), String> {
        if m.output.is_some() {
            return Err("manifest format 3 has \"sets\", not \"output\"".into());
        }
        let Some(eng) = m.packs.first().filter(|p| p.kind == "engine" && p.always) else {
            return Err("format 3: the first pack must be the engine (kind engine, always)".into());
        };
        let mut seen = std::collections::BTreeSet::new();
        for p in &m.packs {
            if !seen.insert(p.id.as_str()) || !release_path_ok(&p.data.file) || !p.data.file.starts_with("packs/") {
                return Err(format!("format 3: bad or duplicate pack {:?}", p.id));
            }
            if p.id != eng.id && (p.kind != "fighter" || p.always) {
                return Err(format!("format 3: pack {:?} must be a fighter (not always on)", p.id));
            }
            if p.test && !m.local_test {
                return Err(format!("format 3: test pack {:?} in a release that is not local_test", p.id));
            }
            let earlier: Vec<&str> = m.packs.iter().take_while(|q| q.id != p.id).map(|q| q.id.as_str()).collect();
            if p.blob.is_none() && !p.blob_source.is_empty()
                || p.blob_source.iter().any(|b| !earlier.contains(&b.as_str()))
                || p.blob.as_ref().is_some_and(|b| b.size > MAX_PACK_DATA)
            {
                return Err(format!("format 3: pack {:?}: bad blob / blob_source", p.id));
            }
        }
        if m.sets.is_empty() {
            return Err("format 3: no sets".into());
        }
        let mut keys = std::collections::BTreeSet::new();
        for s in &m.sets {
            // every pack a source pack's data is built from must come before it in this set's source
            for (i, id) in s.source_packs.iter().enumerate() {
                if let Some(p) = m.packs.iter().find(|p| &p.id == id) {
                    if p.blob_source.iter().any(|b| !s.source_packs[..i].contains(b)) {
                        return Err(format!("format 3: set {:?}: {id} needs {:?} first", s.packs, p.blob_source));
                    }
                }
            }
            let mut k: Vec<&str> = s.packs.iter().map(String::as_str).collect();
            k.sort_unstable();
            let fighters_ok = !s.packs.is_empty()
                && s.packs.iter().all(|i| *i != eng.id && m.packs.iter().any(|p| &p.id == i));
            let mut want = vec![eng.id.clone()];
            want.extend(s.packs.iter().cloned());
            let rid_ok = s.roster_id.len() == 8 && s.roster_id.bytes().all(|b| b.is_ascii_hexdigit());
            if !fighters_ok || s.source_packs != want || !rid_ok || !release_path_ok(&s.patch.file)
                || !s.patch.file.starts_with("sets/") || !keys.insert(k.join("+"))
            {
                return Err(format!("format 3: bad set {:?}", s.packs));
            }
        }
        Ok(())
    }

    pub fn load_dir(dir: &Path) -> Result<Self, String> {
        let m = fs::read_to_string(dir.join("release.json")).map_err(|e| e.to_string())?;
        let manifest: Manifest = serde_json::from_str(&m).map_err(|e| e.to_string())?;
        let mut files = BTreeMap::new();
        if manifest.format < 3 {
            files.insert(PATCH_FILE.to_string(), Cow::Owned(fs::read(dir.join(PATCH_FILE)).map_err(|e| e.to_string())?));
        } else {
            for d in manifest.packs.iter().map(|p| &p.data).chain(manifest.sets.iter().map(|s| &s.patch)) {
                if !release_path_ok(&d.file) {
                    return Err(format!("bad release path {:?}", d.file));
                }
                let b = fs::read(dir.join(&d.file)).map_err(|e| format!("{}: {e}", d.file))?;
                files.insert(d.file.clone(), Cow::Owned(b));
            }
        }
        Self::from_files(&m, files)
    }

    /// Fighter packs the checklist shows (format 3), in release order; empty for formats 1 and 2.
    pub fn fighter_packs(&self) -> Vec<&PackInfo> {
        self.manifest.packs.iter().filter(|p| !p.always).collect()
    }

    /// The default choice: every fighter that is offered alone or together (all ticked).
    pub fn default_choice(&self) -> Vec<String> {
        self.fighter_packs().iter().map(|p| p.id.clone()).collect()
    }

    fn names_of(&self, ids: &[String]) -> String {
        let n: Vec<String> = ids
            .iter()
            .map(|i| self.manifest.packs.iter().find(|p| &p.id == i).map(|p| p.name.clone()).unwrap_or_else(|| i.clone()))
            .collect();
        join_names(&n)
    }

    /// What to build for the ticked fighters. Formats 1 and 2 ignore `choice` (one result).
    /// Format 3: `None` = default choice; an empty choice = `no_pack`; a mix the release does not
    /// have = `pack_mix`; an unknown id = `pack_mix` too (the app only offers known ids).
    pub fn target(&self, choice: Option<&[String]>) -> Result<Target, Problem> {
        let m = &self.manifest;
        if m.format < 3 {
            let o = m.output.clone().expect("checked in from_files");
            return Ok(Target { output: o, set: None });
        }
        let mut want: Vec<String> = match choice {
            Some(c) => c.to_vec(),
            None => self.default_choice(),
        };
        want.sort();
        want.dedup();
        if want.is_empty() {
            return Err(Problem::new("no_pack"));
        }
        let found = m.sets.iter().find(|s| {
            let mut k = s.packs.clone();
            k.sort();
            k == want
        });
        match found {
            Some(s) => Ok(Target {
                output: OutputInfo { file_name: s.file_name.clone(), size: s.size, sha256: s.sha256.clone() },
                set: Some(s.clone()),
            }),
            None => Err(Problem::new("pack_mix").with("names", self.names_of(&want)).detail(want.join("+"))),
        }
    }

    /// Every result this release can make: (output, fighter names); one entry for formats 1/2.
    fn outputs(&self) -> Vec<(OutputInfo, String)> {
        match &self.manifest.output {
            Some(o) => vec![(o.clone(), String::new())],
            None => self
                .manifest
                .sets
                .iter()
                .map(|s| {
                    (OutputInfo { file_name: s.file_name.clone(), size: s.size, sha256: s.sha256.clone() }, self.names_of(&s.packs))
                })
                .collect(),
        }
    }

    fn file(&self, d: &DataFile) -> Result<&[u8], Problem> {
        let b = self.files.get(&d.file).ok_or_else(|| {
            Problem::new("build_failed").with("code", "E-PATCH").detail(format!("missing {}", d.file))
        })?;
        if b.len() as u64 != d.size || hex(&Sha256::digest(b)) != d.sha256 {
            return Err(Problem::new("build_failed").with("code", "E-SUM").detail(format!("{} is damaged", d.file)));
        }
        Ok(b)
    }
}

// ---------------------------------------------------------------- problems

/// A problem the player can understand. `code` selects the text in strings.en.js;
/// `params` fill its {placeholders}; `detail` is technical (shown under "Details").
#[derive(Debug, Clone, Serialize)]
pub struct Problem {
    pub code: &'static str,
    pub params: BTreeMap<String, String>,
    pub detail: String,
}

impl Problem {
    pub fn new(code: &'static str) -> Self {
        Problem { code, params: BTreeMap::new(), detail: String::new() }
    }
    pub fn with(mut self, k: &str, v: impl ToString) -> Self {
        self.params.insert(k.to_string(), v.to_string());
        self
    }
    pub fn detail(mut self, d: impl ToString) -> Self {
        self.detail = d.to_string();
        self
    }
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {:?} {}", self.code, self.params, self.detail)
    }
}

// ---------------------------------------------------------------- progress

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Checking,
    Building,
    Verifying,
}

pub trait Progress {
    fn update(&mut self, stage: Stage, done: u64, total: u64);
    fn cancelled(&self) -> bool;
}

/// Human size in GB with one decimal, e.g. "1.9 GB" (used in messages).
pub fn gb(bytes: u64) -> String {
    if bytes < 1_000_000_000 {
        // "0.0 GB" for a 10 MB file reads like "empty"; say MB below 1 GB.
        return format!("{} MB", bytes / 1_000_000);
    }
    format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
}

// ---------------------------------------------------------------- checks

/// What the first bytes say the file is, when it is clearly not a plain disc image.
fn sniff(head: &[u8]) -> Option<Problem> {
    let starts = |m: &[u8]| head.len() >= m.len() && &head[..m.len()] == m;
    if starts(b"PK\x03\x04") {
        return Some(Problem::new("archive").with("kind", "ZIP"));
    }
    if starts(b"7z\xBC\xAF\x27\x1C") {
        return Some(Problem::new("archive").with("kind", "7Z"));
    }
    if starts(b"Rar!\x1A\x07") {
        return Some(Problem::new("archive").with("kind", "RAR"));
    }
    if starts(b"CISO") || starts(b"ZISO") || starts(b"DAX\0") || starts(b"JISO") {
        let k = String::from_utf8_lossy(&head[..4]).trim_end_matches('\0').to_string();
        return Some(Problem::new("compressed_image").with("kind", k));
    }
    if starts(b"\0PBP") {
        return Some(Problem::new("eboot"));
    }
    None
}

/// Find a file in a PSP ISO9660 image by path ("PSP_GAME/SYSDIR/EBOOT.BIN"):
/// (byte offset, size). Read only; `None` if the image has no such file.
pub fn iso_find<R: Read + Seek>(f: &mut R, path: &str) -> Option<(u64, u64)> {
    let mut pvd = [0u8; 2048];
    f.seek(SeekFrom::Start(16 * 2048)).ok()?;
    f.read_exact(&mut pvd).ok()?;
    if pvd[0] != 1 || &pvd[1..6] != b"CD001" {
        return None;
    }
    let root = &pvd[156..156 + 34];
    let mut lba = u32::from_le_bytes(root[2..6].try_into().ok()?) as u64;
    let mut len = u32::from_le_bytes(root[10..14].try_into().ok()?) as u64;
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    for (k, want) in parts.iter().enumerate() {
        let last = k + 1 == parts.len();
        let mut dir = vec![0u8; len.min(64 * 2048) as usize];
        f.seek(SeekFrom::Start(lba * 2048)).ok()?;
        f.read_exact(&mut dir).ok()?;
        let mut p = 0usize;
        let mut found = None;
        while p < dir.len() {
            let rl = dir[p] as usize;
            if rl == 0 {
                p = (p / 2048 + 1) * 2048; // records never cross a sector; jump to the next one
                continue;
            }
            if p + rl > dir.len() || rl < 34 {
                break;
            }
            let r = &dir[p..p + rl];
            let nl = r[32] as usize;
            if 33 + nl <= rl {
                let name = &r[33..33 + nl];
                let name = name.split(|&c| c == b';').next().unwrap_or(name);
                let is_dir = r[25] & 2 != 0;
                if name.eq_ignore_ascii_case(want.as_bytes()) && is_dir != last {
                    let flba = u32::from_le_bytes(r[2..6].try_into().ok()?) as u64;
                    let fsize = u32::from_le_bytes(r[10..14].try_into().ok()?) as u64;
                    found = Some((flba, fsize));
                    break;
                }
            }
            p += rl;
        }
        let (flba, fsize) = found?;
        if last {
            return Some((flba * 2048, fsize));
        }
        lba = flba;
        len = fsize;
    }
    None
}

/// Read the game id from /UMD_DATA.BIN of a PSP ISO9660 image ("ULUS-10566"), if present.
pub fn read_disc_id<R: Read + Seek>(f: &mut R) -> Option<String> {
    let (off, _) = iso_find(f, "UMD_DATA.BIN")?;
    let mut id = [0u8; 10];
    f.seek(SeekFrom::Start(off)).ok()?;
    f.read_exact(&mut id).ok()?;
    let s = String::from_utf8_lossy(&id).to_string();
    if s.is_ascii() {
        Some(s)
    } else {
        None
    }
}

// ---------------------------------------------------------------- format-2 source (decrypted EBOOT)

/// The disc's EBOOT.BIN, decrypted in memory (never written next to the player's file).
pub struct PlainEboot {
    pub offset: u64,
    pub encrypted_size: u64,
    pub encrypted_sha256: String,
    pub plain: Vec<u8>,
    pub plain_sha256: String,
}

/// Find, read and decrypt `file` (a ~PSP executable) inside a disc image. Read only.
pub fn decrypt_disc_file(path: &Path, file: &str) -> Result<PlainEboot, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let (offset, size) = iso_find(&mut f, file).ok_or_else(|| format!("{file} not found in the disc image"))?;
    if size > 64 << 20 {
        return Err(format!("{file} is {size} bytes; too large"));
    }
    let mut enc = vec![0u8; size as usize];
    f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    f.read_exact(&mut enc).map_err(|e| e.to_string())?;
    let plain = pspcrypt::decrypt(&enc).map_err(|e| format!("{file}: {e}"))?;
    Ok(PlainEboot {
        offset,
        encrypted_size: size,
        encrypted_sha256: hex(&Sha256::digest(&enc)),
        plain_sha256: hex(&Sha256::digest(&plain)),
        plain,
    })
}

/// The player's file as the patch sees it: `file` with `overlay` bytes shown from `at` on.
/// Read + Seek over the real file; nothing is written anywhere.
pub struct VirtualSource<F: Read + Seek> {
    file: F,
    pos: u64,
    at: u64,
    overlay: Vec<u8>,
}

impl<F: Read + Seek> VirtualSource<F> {
    pub fn new(file: F, at: u64, overlay: Vec<u8>) -> Self {
        VirtualSource { file, pos: 0, at, overlay }
    }
}

impl<F: Read + Seek> Read for VirtualSource<F> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let end = self.at + self.overlay.len() as u64;
        let n = if self.pos >= self.at && self.pos < end {
            let o = (self.pos - self.at) as usize;
            let n = buf.len().min(self.overlay.len() - o);
            buf[..n].copy_from_slice(&self.overlay[o..o + n]);
            n
        } else {
            // Plain file bytes, stopping where the overlay starts.
            let lim = if self.pos < self.at { ((self.at - self.pos) as usize).min(buf.len()) } else { buf.len() };
            self.file.seek(SeekFrom::Start(self.pos))?;
            self.file.read(&mut buf[..lim])?
        };
        self.pos += n as u64;
        Ok(n)
    }
}

impl<F: Read + Seek> Seek for VirtualSource<F> {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let new = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(d) => self.file.seek(SeekFrom::End(0))?.checked_add_signed(d),
        };
        self.pos = new.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before start"))?;
        Ok(self.pos)
    }
}

/// Open the patch source for `input` (already checked by check_input): the file itself
/// (format 1) or the file with its EBOOT decrypted in memory (format 2).
pub fn open_source(rel: &Release, input: &Path) -> Result<VirtualSource<File>, Problem> {
    let f = File::open(input).map_err(|e| Problem::new("cant_read").detail(e))?;
    let Some(s) = &rel.manifest.source else {
        return Ok(VirtualSource::new(f, 0, Vec::new()));
    };
    let fail = |d: String| Problem::new("build_failed").with("code", "E-DECRYPT").detail(d);
    let pe = decrypt_disc_file(input, &s.file).map_err(fail)?;
    if pe.offset != s.offset || pe.encrypted_size != s.encrypted_size || pe.encrypted_sha256 != s.encrypted_sha256 {
        return Err(fail(format!(
            "{} at {} ({} B, sha256 {}) is not the expected file",
            s.file, pe.offset, pe.encrypted_size, pe.encrypted_sha256
        )));
    }
    if pe.plain.len() as u64 != s.plain_size || pe.plain_sha256 != s.plain_sha256 {
        return Err(fail(format!("decrypted {} B, sha256 {}", pe.plain.len(), pe.plain_sha256)));
    }
    Ok(VirtualSource::new(f, s.offset, pe.plain))
}

fn sha256_file<P: Progress>(path: &Path, size: u64, stage: Stage, prog: &mut P) -> Result<String, Problem> {
    let mut f = File::open(path).map_err(|e| Problem::new("cant_read").detail(e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 4 << 20];
    let mut done = 0u64;
    loop {
        if prog.cancelled() {
            return Err(Problem::new("cancelled"));
        }
        let n = f.read(&mut buf).map_err(|e| Problem::new("cant_read").detail(e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        done += n as u64;
        prog.update(stage, done, size);
    }
    Ok(hex(&h.finalize()))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Step 1: is this the player's clean disc image? Never writes anything.
pub fn check_input<P: Progress>(rel: &Release, path: &Path, prog: &mut P) -> Result<(), Problem> {
    let m = &rel.manifest;
    let meta = fs::metadata(path).map_err(|e| Problem::new("not_found").detail(e))?;
    if meta.is_dir() {
        return Err(Problem::new("is_folder"));
    }
    let mut f = File::open(path).map_err(|e| Problem::new("cant_read").detail(e))?;
    let mut head = [0u8; 16];
    let n = f.read(&mut head).map_err(|e| Problem::new("cant_read").detail(e))?;
    if let Some(p) = sniff(&head[..n]) {
        return Err(p);
    }
    let size = meta.len();
    let id = read_disc_id(&mut f);
    drop(f);
    match id.as_deref() {
        None if size < m.input.size / 2 => {
            // Too small to be the game and no disc header: a truncated copy or not a disc image.
            return Err(Problem::new(if size == 0 { "empty" } else { "not_disc" }));
        }
        None => return Err(Problem::new("not_disc")),
        Some(i) if i != m.input.disc_id => {
            return Err(match m.other_regions.get(i) {
                Some(region) => Problem::new("wrong_region")
                    .with("region", region)
                    .with("id", i)
                    .with("want", &m.input.disc_id),
                None => Problem::new("other_game").with("id", i),
            });
        }
        Some(_) => {}
    }
    let outputs = rel.outputs();
    if size < m.input.size && !outputs.iter().any(|(o, _)| o.size == size) {
        return Err(Problem::new("incomplete").with("have", gb(size)).with("want", gb(m.input.size)));
    }
    let sum = sha256_file(path, size, Stage::Checking, prog)?;
    if sum == m.input.sha256 {
        return Ok(());
    }
    let packs = m.format >= 3;
    if let Some((_, names)) = outputs.iter().find(|(o, _)| o.sha256 == sum) {
        return Err(if packs {
            Problem::new("already_has").with("version", &m.version).with("names", names)
        } else {
            Problem::new("already_patched").with("version", &m.version)
        });
    }
    if let Some(o) = m.older_outputs.iter().find(|o| o.sha256 == sum) {
        return Err(if packs {
            Problem::new("older_has").with("version", &o.version).with("names", &o.names)
        } else {
            Problem::new("older_patch").with("version", &o.version)
        });
    }
    Err(Problem::new("modified").detail(format!("sha256 {sum}")))
}

/// Free bytes on the drive holding `dir`.
pub fn free_space(dir: &Path) -> Option<u64> {
    fs4::available_space(dir).ok()
}

/// Step 2a: pick the output path next to the input (or in `out_dir`) without overwriting
/// anything, and check room + write permission. Never writes the player's file.
pub fn plan_output(rel: &Release, input: &Path, out_dir: Option<&Path>) -> Result<PathBuf, Problem> {
    plan_output_for(&rel.target(None)?, input, out_dir)
}

/// plan_output for a chosen target (format 3: the ticked fighter set, from `Release::target`).
pub fn plan_output_for(target: &Target, input: &Path, out_dir: Option<&Path>) -> Result<PathBuf, Problem> {
    let dir = match out_dir {
        Some(d) => d.to_path_buf(),
        None => input.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(".")),
    };
    let need = target.output.size + 64 * 1024 * 1024;
    if let Some(have) = free_space(&dir) {
        if have < need {
            return Err(Problem::new("no_space").with("need", gb(need)).with("have", gb(have)));
        }
    }
    let name = &target.output.file_name;
    let (stem, ext) = match name.rfind('.') {
        Some(i) => (&name[..i], &name[i..]),
        None => (name.as_str(), ""),
    };
    remove_stale_temps(&dir, stem, ext);
    // A name is free when neither it nor its temporary file exists (a temp that is still there
    // after remove_stale_temps belongs to a build that is running right now).
    let mut out = dir.join(name);
    let mut k = 2;
    while out.exists() || temp_path(&out).exists() {
        out = dir.join(format!("{stem} ({k}){ext}"));
        k += 1;
    }
    // Same file as the input can never happen (it already exists), but be explicit.
    if same_file(&out, input) {
        return Err(Problem::new("cant_write").detail("output == input"));
    }
    let probe = temp_path(&out);
    match OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
        }
        Err(e) => return Err(Problem::new("cant_write").with("folder", dir.display()).detail(e)),
    }
    Ok(out)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// A temporary file untouched this long is a leftover of a build that was killed (window closed
/// mid-build, Ctrl+C, crash, power cut): a running build writes to it every few milliseconds, and
/// even the final flush to a slow USB stick finishes well within this.
const STALE_TEMP_SECS: u64 = 10 * 60;

/// Deletes our own leftover temporary files in `dir`: only hidden regular files named exactly
/// `.<stem><ext>.part` or `.<stem> (<n>)<ext>.part` (the names `temp_path` gives this release's
/// output) that have not been written for STALE_TEMP_SECS. Nothing else is ever touched.
fn remove_stale_temps(dir: &Path, stem: &str, ext: &str) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !is_our_temp_name(&name, stem, ext) {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        let idle = meta.modified().ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs());
        if meta.is_file() && idle.is_some_and(|s| s >= STALE_TEMP_SECS) {
            let _ = fs::remove_file(e.path());
        }
    }
}

fn is_our_temp_name(name: &str, stem: &str, ext: &str) -> bool {
    let Some(mid) = name
        .strip_prefix('.')
        .and_then(|n| n.strip_prefix(stem))
        .and_then(|n| n.strip_suffix(".part"))
        .and_then(|n| n.strip_suffix(ext))
    else {
        return false;
    };
    mid.is_empty()
        || mid
            .strip_prefix(" (")
            .and_then(|m| m.strip_suffix(')'))
            .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

fn temp_path(out: &Path) -> PathBuf {
    let name = out.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    out.with_file_name(format!(".{name}.part"))
}

/// Removes the temporary file unless `keep()` was called (failure / cancel / panic safe).
struct TempGuard {
    path: PathBuf,
    armed: bool,
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct HashingWriter<W: Write> {
    inner: W,
    hash: Sha256,
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

struct DecodeSink<'a, P: Progress> {
    prog: &'a mut P,
    total: u64,
}

impl<P: Progress> vcdiff::Sink for DecodeSink<'_, P> {
    fn progress(&mut self, written: u64) {
        self.prog.update(Stage::Building, written, self.total);
    }
    fn cancelled(&self) -> bool {
        self.prog.cancelled()
    }
}

fn io_problem(e: &std::io::Error) -> Problem {
    // "No space" can still happen if something else fills the drive while we write.
    if e.raw_os_error() == Some(28) || e.raw_os_error() == Some(112) {
        Problem::new("no_space_late")
    } else {
        Problem::new("build_failed").with("code", "E-IO").detail(e)
    }
}

/// The format-3 patch source: the player's disc (as `open_source` shows it) followed by the
/// data files of the set's packs, end to end. Read + Seek; nothing is written anywhere.
pub struct ChainSource<'a, H: Read + Seek> {
    head: H,
    head_len: u64,
    parts: Vec<&'a [u8]>,
    pos: u64,
}

impl<'a, H: Read + Seek> ChainSource<'a, H> {
    pub fn new(head: H, head_len: u64, parts: Vec<&'a [u8]>) -> Self {
        ChainSource { head, head_len, parts, pos: 0 }
    }
    pub fn len(&self) -> u64 {
        self.head_len + self.parts.iter().map(|p| p.len() as u64).sum::<u64>()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<H: Read + Seek> Read for ChainSource<'_, H> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.pos < self.head_len {
            let lim = ((self.head_len - self.pos) as usize).min(buf.len());
            self.head.seek(SeekFrom::Start(self.pos))?;
            let n = self.head.read(&mut buf[..lim])?;
            self.pos += n as u64;
            return Ok(n);
        }
        let mut at = self.head_len;
        for p in &self.parts {
            let end = at + p.len() as u64;
            if self.pos < end {
                let o = (self.pos - at) as usize;
                let n = buf.len().min(p.len() - o);
                buf[..n].copy_from_slice(&p[o..o + n]);
                self.pos += n as u64;
                return Ok(n);
            }
            at = end;
        }
        Ok(0)
    }
}

impl<H: Read + Seek> Seek for ChainSource<'_, H> {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let new = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(d) => self.len().checked_add_signed(d),
        };
        self.pos = new.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before start"))?;
        Ok(self.pos)
    }
}

pub const REG_PATH: &str = "PSP_GAME/USRDIR/DATA/EXTRA.REG";
pub const IDX_PATH: &str = "PSP_GAME/USRDIR/DATA/EXTRA.IDX";

/// Roster ids of a finished disc (P2.9b / E7): EXTRA.REG +0x40 (header >= 0x48, magic XREG) and
/// EXTRA.IDX +0x20 (header >= 0x24, magic XIDX); `None` where a file or its id is missing.
pub fn read_roster_ids<R: Read + Seek>(f: &mut R) -> (Option<u32>, Option<u32>) {
    let mut one = |path: &str, magic: &[u8; 4], need: u16, at: usize| -> Option<u32> {
        let (off, size) = iso_find(f, path)?;
        if size < at as u64 + 4 {
            return None;
        }
        let mut h = vec![0u8; at + 4];
        f.seek(SeekFrom::Start(off)).ok()?;
        f.read_exact(&mut h).ok()?;
        let hsz = u16::from_le_bytes([h[6], h[7]]);
        (&h[..4] == magic && hsz >= need).then(|| u32::from_le_bytes(h[at..at + 4].try_into().unwrap()))
    };
    let reg = one(REG_PATH, b"XREG", 0x48, 0x40);
    let idx = one(IDX_PATH, b"XIDX", 0x24, 0x20);
    (reg, idx)
}

struct QuietSink<'a, P: Progress> {
    prog: &'a mut P,
}

impl<P: Progress> vcdiff::Sink for QuietSink<'_, P> {
    fn progress(&mut self, _written: u64) {
        self.prog.update(Stage::Building, 0, 1);
    }
    fn cancelled(&self) -> bool {
        self.prog.cancelled()
    }
}

/// Rebuild one pack's data in memory: its VCDIFF against the player's disc followed by the pack
/// data of `blob_source` (already rebuilt, in `done`). Size + SHA-256 must match the manifest.
fn pack_data<S: Read + Seek, P: Progress>(
    rel: &Release,
    p: &PackInfo,
    b: &BlobInfo,
    delta: &[u8],
    done: &[(String, Cow<'_, [u8]>)],
    src: &mut S,
    prog: &mut P,
) -> Result<Vec<u8>, Problem> {
    let mut parts = Vec::new();
    for k in &p.blob_source {
        let d = done.iter().find(|(id, _)| id == k).ok_or_else(|| {
            Problem::new("build_failed").with("code", "E-PATCH").detail(format!("{}: needs {k} first", p.id))
        })?;
        parts.push(d.1.as_ref());
    }
    let mut chain = ChainSource::new(src, rel.manifest.input.size, parts);
    let mut out = Vec::with_capacity(b.size.min(MAX_PACK_DATA) as usize);
    let mut sink = QuietSink { prog };
    match vcdiff::decode(delta, &mut chain, &mut out, &mut sink) {
        Ok(_) => {}
        Err(vcdiff::DecodeError::Cancelled) => return Err(Problem::new("cancelled")),
        Err(vcdiff::DecodeError::Io(e)) => return Err(io_problem(&e)),
        Err(vcdiff::DecodeError::Checksum { window }) => {
            return Err(Problem::new("build_failed").with("code", "E-SUM").detail(format!("{} window {window}", p.id)))
        }
        Err(e @ vcdiff::DecodeError::BadPatch(_)) => {
            return Err(Problem::new("build_failed").with("code", "E-PATCH").detail(format!("{}: {e}", p.id)))
        }
    }
    if out.len() as u64 != b.size || hex(&Sha256::digest(&out)) != b.sha256 {
        return Err(Problem::new("build_failed").with("code", "E-SUM").detail(format!("{}: pack data does not match", p.id)));
    }
    Ok(out)
}

/// Step 2b + 3: build the patched copy at `out` (from plan_output) and check it.
/// Formats 1 and 2 (one result); for format 3 this builds the default choice (all fighters).
pub fn build<P: Progress>(rel: &Release, input: &Path, out: &Path, prog: &mut P) -> Result<PathBuf, Problem> {
    build_for(rel, &rel.target(None)?, input, out, prog)
}

/// Step 2b + 3 for a chosen target (`Release::target`).
pub fn build_for<P: Progress>(rel: &Release, target: &Target, input: &Path, out: &Path, prog: &mut P) -> Result<PathBuf, Problem> {
    let want = &target.output;
    let tmp = temp_path(out);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| Problem::new("cant_write").detail(e))?;
    let mut guard = TempGuard { path: tmp.clone(), armed: true };
    let mut src = open_source(rel, input)?;
    // Format 3: the set's patch + the pack data of its packs, each checked before the result is written.
    let mut done: Vec<(String, Cow<'_, [u8]>)> = Vec::new();
    let patch: &[u8] = match &target.set {
        None => &rel.delta,
        Some(s) => {
            for id in &s.source_packs {
                let p = rel.manifest.packs.iter().find(|p| &p.id == id).ok_or_else(|| {
                    Problem::new("build_failed").with("code", "E-PATCH").detail(format!("no pack {id}"))
                })?;
                let data = rel.file(&p.data)?;
                let blob = match &p.blob {
                    None => Cow::Borrowed(data),
                    Some(b) => Cow::Owned(pack_data(rel, p, b, data, &done, &mut src, prog)?),
                };
                done.push((id.clone(), blob));
            }
            rel.file(&s.patch)?
        }
    };
    let mut w = HashingWriter { inner: BufWriter::with_capacity(1 << 20, file), hash: Sha256::new() };
    let mut sink = DecodeSink { prog, total: want.size };
    let res = if target.set.is_some() {
        let parts: Vec<&[u8]> = done.iter().map(|(_, b)| b.as_ref()).collect();
        let mut chain = ChainSource::new(&mut src, rel.manifest.input.size, parts);
        vcdiff::decode(patch, &mut chain, &mut w, &mut sink)
    } else {
        vcdiff::decode(patch, &mut src, &mut w, &mut sink)
    };
    let written = match res {
        Ok(n) => n,
        Err(vcdiff::DecodeError::Cancelled) => return Err(Problem::new("cancelled")),
        Err(vcdiff::DecodeError::Io(e)) => return Err(io_problem(&e)),
        Err(vcdiff::DecodeError::Checksum { window }) => {
            return Err(Problem::new("build_failed").with("code", "E-SUM").detail(format!("window {window}")))
        }
        Err(e @ vcdiff::DecodeError::BadPatch(_)) => {
            return Err(Problem::new("build_failed").with("code", "E-PATCH").detail(e))
        }
    };
    let prog = sink.prog;
    prog.update(Stage::Verifying, 0, 1);
    let HashingWriter { inner, hash } = w;
    let file = inner.into_inner().map_err(|e| io_problem(e.error()))?;
    file.sync_all().map_err(|e| io_problem(&e))?;
    drop(file);
    let sum = hex(&hash.finalize());
    if written != want.size || sum != want.sha256 {
        return Err(Problem::new("build_failed")
            .with("code", "E-HASH")
            .detail(format!("size {written} sha256 {sum}")));
    }
    // Format 3: the finished disc must carry the ticked set's roster id in both engine files
    // (the SHA-256 above already binds every byte; this names the fighter list explicitly).
    if let Some(s) = &target.set {
        let mut f = File::open(&tmp).map_err(|e| io_problem(&e))?;
        let ids = read_roster_ids(&mut f);
        let want_id = u32::from_str_radix(&s.roster_id, 16).ok();
        if ids != (want_id, want_id) {
            return Err(Problem::new("build_failed")
                .with("code", "E-ROSTER")
                .detail(format!("roster ids {:08x?} want {}", ids, s.roster_id)));
        }
    }
    if out.exists() {
        return Err(Problem::new("cant_write").detail("output appeared while building"));
    }
    fs::rename(&tmp, out).map_err(|e| Problem::new("cant_write").detail(e))?;
    guard.armed = false;
    prog.update(Stage::Verifying, 1, 1);
    Ok(out.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Minimal ISO9660 image with /UMD_DATA.BIN, built in memory (no file written).
    fn fake_disc(id: &str) -> Vec<u8> {
        let mut img = vec![0u8; 20 * 2048];
        let pvd = &mut img[16 * 2048..17 * 2048];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        let root = &mut pvd[156..190];
        root[0] = 34;
        root[2..6].copy_from_slice(&18u32.to_le_bytes());
        root[10..14].copy_from_slice(&2048u32.to_le_bytes());
        let name = b"UMD_DATA.BIN;1";
        let dir = &mut img[18 * 2048..19 * 2048];
        // "." and ".." entries first, like a real disc.
        for (i, n) in [(0usize, 0u8), (34, 1)] {
            dir[i] = 34;
            dir[i + 32] = 1;
            dir[i + 33] = n;
        }
        let r = 68;
        let rl = 33 + name.len() + 1;
        dir[r] = rl as u8;
        dir[r + 2..r + 6].copy_from_slice(&19u32.to_le_bytes());
        dir[r + 10..r + 14].copy_from_slice(&32u32.to_le_bytes());
        dir[r + 32] = name.len() as u8;
        dir[r + 33..r + 33 + name.len()].copy_from_slice(name);
        img[19 * 2048..19 * 2048 + id.len()].copy_from_slice(id.as_bytes());
        img
    }

    #[test]
    fn disc_id_found() {
        let img = fake_disc("ULES-01505|0123456789ABCDEF|0001|G");
        assert_eq!(read_disc_id(&mut Cursor::new(img)).as_deref(), Some("ULES-01505"));
    }

    #[test]
    fn disc_id_absent_on_random_data() {
        assert_eq!(read_disc_id(&mut Cursor::new(vec![7u8; 100_000])), None);
    }

    #[test]
    fn virtual_source_overlays_exactly() {
        let base: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let ov = vec![0xEEu8; 1234];
        let at = 4000u64;
        let mut want = base.clone();
        want[at as usize..at as usize + ov.len()].copy_from_slice(&ov);
        let mut v = VirtualSource::new(Cursor::new(base), at, ov);
        let mut got = Vec::new();
        v.read_to_end(&mut got).unwrap();
        assert_eq!(got, want);
        for (pos, n) in [(3990u64, 30usize), (5230, 10), (0, 10_000), (9_995, 5), (4500, 7)] {
            let mut b = vec![0u8; n];
            v.seek(SeekFrom::Start(pos)).unwrap();
            v.read_exact(&mut b).unwrap();
            assert_eq!(b, want[pos as usize..pos as usize + n], "at {pos}");
        }
        assert_eq!(v.seek(SeekFrom::End(-1)).unwrap(), 9_999);
    }

    #[test]
    fn iso_find_nested_and_missing() {
        let img = fake_disc("ULUS-10566|0123456789ABCDEF|0001|G");
        let mut c = Cursor::new(img);
        assert_eq!(iso_find(&mut c, "UMD_DATA.BIN"), Some((19 * 2048, 32)));
        assert_eq!(iso_find(&mut c, "PSP_GAME/SYSDIR/EBOOT.BIN"), None);
        assert_eq!(iso_find(&mut c, "UMD_DATA.BIN/X"), None);
    }

    #[test]
    fn manifest_formats() {
        let base = r#""title":"t","version":"1","input":{"disc_id":"X","size":1,"sha256":"a"},
            "output":{"file_name":"o","size":1,"sha256":"b"}"#;
        let src = r#","source":{"kind":"plain_eboot","file":"PSP_GAME/SYSDIR/EBOOT.BIN","offset":2048,
            "encrypted_size":10,"encrypted_sha256":"c","plain_size":8,"plain_sha256":"d"}"#;
        let m = |f: u32, s: &str| Release::from_parts(&format!("{{\"format\":{f},{base}{s}}}"), Vec::new());
        assert!(m(1, "").is_ok());
        assert!(m(2, src).is_ok());
        assert!(m(2, "").is_err());
        assert!(m(1, src).is_err());
        assert!(m(3, "").is_err());
        assert!(m(2, &src.replace("plain_eboot", "other")).is_err());
    }

    /// Hash-only check against a real clean disc, when one is given (never committed):
    ///   SORAPATCH_CLEAN_ISO=/path/ULUS10566.iso cargo test -- --ignored real_eboot
    #[test]
    #[ignore]
    fn real_eboot_decrypts_to_known_plain_elf() {
        let Ok(p) = std::env::var("SORAPATCH_CLEAN_ISO") else {
            eprintln!("SORAPATCH_CLEAN_ISO not set; skipped");
            return;
        };
        let pe = decrypt_disc_file(Path::new(&p), EBOOT_PATH).expect("decrypt");
        assert_eq!(pe.offset, 560 * 2048);
        assert_eq!(pe.encrypted_size, 4_641_296);
        assert_eq!(pe.encrypted_sha256, "8fbf3541ab121aff85aae72795760736dfe37706996b2b0f9b81d36fc98f3394");
        assert_eq!(pe.plain.len(), 4_640_948);
        assert_eq!(pe.plain_sha256, "0bc8e779aad2814a3d1bd77346f69ca5e16665845ed053cd0ad44a9cbfaefe6d");
    }

    #[test]
    fn temp_name_matching() {
        let (st, ext) = ("Dissidia 012 + Sora (fan patch 1.0)", ".iso");
        assert!(is_our_temp_name(".Dissidia 012 + Sora (fan patch 1.0).iso.part", st, ext));
        assert!(is_our_temp_name(".Dissidia 012 + Sora (fan patch 1.0) (12).iso.part", st, ext));
        assert!(!is_our_temp_name("Dissidia 012 + Sora (fan patch 1.0).iso.part", st, ext));
        assert!(!is_our_temp_name(".Dissidia 012 + Sora (fan patch 1.0) (x).iso.part", st, ext));
        assert!(!is_our_temp_name(".Dissidia 012 + Sora (fan patch 1.0) ().iso.part", st, ext));
        assert!(!is_our_temp_name(".Dissidia 012 + Sora (fan patch 1.0).iso", st, ext));
        assert!(!is_our_temp_name(".Dissidia 012 + Sora (fan patch 1.0) copy.iso.part", st, ext));
        assert!(!is_our_temp_name(".other.iso.part", st, ext));
    }

    /// A leftover temp from a killed build must neither block the folder (it used to give
    /// cant_write "File exists") nor be deleted while it could still belong to a running build.
    #[test]
    fn plan_output_handles_leftover_temps() {
        let dir = std::env::temp_dir().join(format!("sorapatch-stale-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let rel = Release::from_parts(
            r#"{"format":1,"title":"t","version":"1","input":{"disc_id":"X","size":1,"sha256":"a"},
               "output":{"file_name":"Out.iso","size":1,"sha256":"b"}}"#,
            Vec::new(),
        )
        .unwrap();
        let input = dir.join("in.iso");
        fs::write(&input, b"x").unwrap();
        let fresh = dir.join(".Out.iso.part");
        let stale = dir.join(".Out (2).iso.part");
        let foreign = dir.join(".Out.iso.part.keep");
        for f in [&fresh, &stale, &foreign] {
            fs::write(f, b"partial").unwrap();
        }
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(STALE_TEMP_SECS + 5);
        for f in [&stale, &foreign] {
            File::options().write(true).open(f).unwrap().set_modified(old).unwrap();
        }
        let out = plan_output(&rel, &input, None).expect("leftover temp must not block the folder");
        assert!(fresh.exists(), "a recent temp (maybe a running build) is kept");
        assert!(!stale.exists(), "an idle leftover temp is removed");
        assert!(foreign.exists(), "other files are never touched");
        // ".Out.iso.part" is busy, the stale "(2)" temp is gone, so "(2)" is free again.
        assert_eq!(out, dir.join("Out (2).iso"));
        fs::remove_dir_all(&dir).unwrap();
    }

    /// ISO9660 image with nested directories (sector 18 = root), built in memory: `files` are
    /// (path, bytes). Enough for iso_find (no path tables, one sector per directory).
    fn fake_tree(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut dirs: Vec<String> = vec![String::new()];
        for (p, _) in files {
            let parts: Vec<&str> = p.split('/').collect();
            for k in 1..parts.len() {
                let d = parts[..k].join("/");
                if !dirs.contains(&d) {
                    dirs.push(d);
                }
            }
        }
        let dir_lba = |d: &str, dirs: &Vec<String>| 18 + dirs.iter().position(|x| x == d).unwrap() as u32;
        let mut next = 18 + dirs.len() as u32;
        let mut file_lba = Vec::new();
        for (_, b) in files {
            file_lba.push(next);
            next += (b.len() as u32).div_ceil(2048).max(1);
        }
        let mut img = vec![0u8; next as usize * 2048];
        let pvd = 16 * 2048;
        img[pvd] = 1;
        img[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        img[pvd + 156] = 34;
        img[pvd + 158..pvd + 162].copy_from_slice(&18u32.to_le_bytes());
        img[pvd + 166..pvd + 170].copy_from_slice(&2048u32.to_le_bytes());
        let rec = |img: &mut Vec<u8>, at: usize, lba: u32, size: u32, name: &[u8], dir: bool| -> usize {
            let rl = 33 + name.len() + (1 - name.len() % 2);
            img[at] = rl as u8;
            img[at + 2..at + 6].copy_from_slice(&lba.to_le_bytes());
            img[at + 10..at + 14].copy_from_slice(&size.to_le_bytes());
            img[at + 25] = if dir { 2 } else { 0 };
            img[at + 32] = name.len() as u8;
            img[at + 33..at + 33 + name.len()].copy_from_slice(name);
            at + rl
        };
        for d in &dirs {
            let base = dir_lba(d, &dirs) as usize * 2048;
            let mut at = rec(&mut img, base, dir_lba(d, &dirs), 2048, &[0], true);
            at = rec(&mut img, at, 18, 2048, &[1], true);
            for c in &dirs {
                if !c.is_empty() && c.rsplit_once('/').map(|x| x.0).unwrap_or("") == d.as_str() {
                    let n = c.rsplit('/').next().unwrap();
                    at = rec(&mut img, at, dir_lba(c, &dirs), 2048, n.as_bytes(), true);
                }
            }
            for (i, (p, b)) in files.iter().enumerate() {
                if p.rsplit_once('/').map(|x| x.0).unwrap_or("") == d.as_str() {
                    let n = format!("{};1", p.rsplit('/').next().unwrap());
                    at = rec(&mut img, at, file_lba[i], b.len() as u32, n.as_bytes(), false);
                }
            }
        }
        for (i, (_, b)) in files.iter().enumerate() {
            let o = file_lba[i] as usize * 2048;
            img[o..o + b.len()].copy_from_slice(b);
        }
        img
    }

    fn reg_idx(reg_id: Option<u32>, idx_id: Option<u32>) -> Vec<(&'static str, Vec<u8>)> {
        let mut reg = b"XREG".to_vec();
        reg.extend_from_slice(&1u16.to_le_bytes());
        reg.extend_from_slice(&(if reg_id.is_some() { 0x48u16 } else { 0x40 }).to_le_bytes());
        reg.resize(0x40, 0);
        if let Some(r) = reg_id {
            reg.extend_from_slice(&r.to_le_bytes());
            reg.extend_from_slice(&0x2F2EAE84u32.to_le_bytes());
        }
        let mut idx = b"XIDX".to_vec();
        idx.extend_from_slice(&1u16.to_le_bytes());
        idx.extend_from_slice(&(if idx_id.is_some() { 0x24u16 } else { 0x20 }).to_le_bytes());
        idx.resize(0x20, 0);
        if let Some(r) = idx_id {
            idx.extend_from_slice(&r.to_le_bytes());
        }
        vec![("UMD_DATA.BIN", b"ULUS-10566|0123456789ABCDEF|0001|G".to_vec()), (REG_PATH, reg), (IDX_PATH, idx)]
    }

    #[test]
    fn roster_ids_read_from_a_disc() {
        let mut c = Cursor::new(fake_tree(&reg_idx(Some(0x9bf4196a), Some(0x9bf4196a))));
        assert_eq!(read_disc_id(&mut c).as_deref(), Some("ULUS-10566"));
        assert_eq!(read_roster_ids(&mut c), (Some(0x9bf4196a), Some(0x9bf4196a)));
        let mut c = Cursor::new(fake_tree(&reg_idx(None, Some(7))));
        assert_eq!(read_roster_ids(&mut c), (None, Some(7)));
        let mut c = Cursor::new(fake_disc("ULUS-10566|0123456789ABCDEF|0001|G"));
        assert_eq!(read_roster_ids(&mut c), (None, None));
    }

    #[test]
    fn chain_source_reads_across_parts() {
        let head: Vec<u8> = (0..5000u32).map(|i| (i % 253) as u8).collect();
        let (a, b) = (vec![0xAAu8; 300], (0..700u32).map(|i| (i % 7) as u8).collect::<Vec<u8>>());
        let mut want = head.clone();
        want[100..110].copy_from_slice(&[9; 10]); // overlay inside the head
        want.extend_from_slice(&a);
        want.extend_from_slice(&b);
        let v = VirtualSource::new(Cursor::new(head), 100, vec![9; 10]);
        let mut ch = ChainSource::new(v, 5000, vec![&a, &b]);
        assert_eq!(ch.len(), 6000);
        let mut got = Vec::new();
        ch.read_to_end(&mut got).unwrap();
        assert_eq!(got, want);
        for (pos, n) in [(4990u64, 20usize), (5290, 20), (0, 6000), (95, 20), (5999, 1), (5300, 700)] {
            let mut buf = vec![0u8; n];
            ch.seek(SeekFrom::Start(pos)).unwrap();
            ch.read_exact(&mut buf).unwrap();
            assert_eq!(buf, want[pos as usize..pos as usize + n], "at {pos}");
        }
        assert_eq!(ch.seek(SeekFrom::End(-1)).unwrap(), 5999);
        ch.seek(SeekFrom::Start(6000)).unwrap();
        assert_eq!(ch.read(&mut [0u8; 4]).unwrap(), 0);
    }

    fn pack_manifest(extra: &str, sets: &str) -> String {
        let d = |f: &str, n: u64| format!(r#"{{"file":"{f}","size":{n},"sha256":"x"}}"#);
        format!(
            r#"{{"format":3,"title":"t","version":"1.1","input":{{"disc_id":"X","size":1,"sha256":"a"}},{extra}
            "packs":[{{"id":"engine","kind":"engine","name":"Fighter engine","always":true,"version":"2.6.0","package_sha256":"e","data":{}}},
                     {{"id":"sora","kind":"fighter","name":"Sora","owner":33,"version":"1.0.0","package_sha256":"s","data":{}}},
                     {{"id":"two","kind":"fighter","name":"Two","owner":34,"version":"0.1.0","package_sha256":"t","data":{}}}],
            "sets":[{sets}],
            "older_outputs":[{{"version":"1.0","sha256":"old","names":"Sora"}}]}}"#,
            d("packs/engine.bin", 1),
            d("packs/sora.bin", 2),
            d("packs/two.bin", 3)
        )
    }

    fn set_json(packs: &[&str], key: &str, rid: &str) -> String {
        let p: Vec<String> = packs.iter().map(|x| format!("\"{x}\"")).collect();
        format!(
            r#"{{"packs":[{0}],"source_packs":["engine",{0}],"file_name":"{key}.iso","size":10,"sha256":"o-{key}",
               "roster_id":"{rid}","patch":{{"file":"sets/{key}.vcdiff","size":4,"sha256":"y"}}}}"#,
            p.join(",")
        )
    }

    fn pack_files(keys: &[&str]) -> BTreeMap<String, Cow<'static, [u8]>> {
        let mut f: BTreeMap<String, Cow<'static, [u8]>> = BTreeMap::new();
        f.insert("packs/engine.bin".into(), Cow::Owned(vec![1]));
        f.insert("packs/sora.bin".into(), Cow::Owned(vec![1, 2]));
        f.insert("packs/two.bin".into(), Cow::Owned(vec![1, 2, 3]));
        for k in keys {
            f.insert(format!("sets/{k}.vcdiff"), Cow::Owned(vec![0; 4]));
        }
        f
    }

    #[test]
    fn pack_release_manifest_and_choice() {
        let sets = [set_json(&["sora"], "sora", "cdd2d2b6"), set_json(&["sora", "two"], "sora+two", "9bf4196a")].join(",");
        let rel = Release::from_files(&pack_manifest("", &sets), pack_files(&["sora", "sora+two"])).expect("valid");
        assert_eq!(rel.default_choice(), vec!["sora".to_string(), "two".to_string()]);
        assert_eq!(rel.fighter_packs().len(), 2);
        let t = rel.target(None).unwrap();
        assert_eq!(t.output.file_name, "sora+two.iso");
        let t = rel.target(Some(&["two".into(), "sora".into(), "sora".into()])).unwrap();
        assert_eq!(t.set.unwrap().roster_id, "9bf4196a");
        assert_eq!(rel.target(Some(&["sora".into()])).unwrap().output.sha256, "o-sora");
        assert_eq!(rel.target(Some(&[])).unwrap_err().code, "no_pack");
        let e = rel.target(Some(&["two".into()])).unwrap_err();
        assert_eq!((e.code, e.params["names"].as_str()), ("pack_mix", "Two"));
        assert_eq!(rel.target(Some(&["nobody".into()])).unwrap_err().code, "pack_mix");
        assert_eq!(rel.names_of(&["sora".into(), "two".into()]), "Sora and Two");
        assert_eq!(join_names(&["A".into(), "B".into(), "C".into()]), "A, B and C");
        // plan_output for a target uses that set's name and size
        let dir = std::env::temp_dir().join(format!("sorapatch-packs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let input = dir.join("in.iso");
        fs::write(&input, b"x").unwrap();
        let t = rel.target(Some(&["sora".into()])).unwrap();
        assert_eq!(plan_output_for(&t, &input, None).unwrap(), dir.join("sora.iso"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pack_release_manifest_refusals() {
        let ok = set_json(&["sora"], "sora", "cdd2d2b6");
        let try_ = |extra: &str, sets: &str, keys: &[&str]| Release::from_files(&pack_manifest(extra, sets), pack_files(keys));
        assert!(try_("", &ok, &["sora"]).is_ok());
        assert!(try_("", &ok, &[]).is_err(), "missing set patch");
        assert!(try_("", "", &[]).is_err(), "no sets");
        assert!(try_(r#""output":{"file_name":"o","size":1,"sha256":"b"},"#, &ok, &["sora"]).is_err(), "output + sets");
        assert!(try_("", &set_json(&["sora"], "sora", "xyz"), &["sora"]).is_err(), "bad roster id");
        assert!(try_("", &set_json(&["ghost"], "ghost", "00000000"), &["ghost"]).is_err(), "unknown pack");
        assert!(try_("", &[ok.clone(), ok.clone()].join(","), &["sora"]).is_err(), "duplicate set");
        let bad_src = ok.replace(r#""source_packs":["engine","sora"]"#, r#""source_packs":["sora"]"#);
        assert!(try_("", &bad_src, &["sora"]).is_err(), "engine not first in the source");
        let up = ok.replace("sets/sora.vcdiff", "../sora.vcdiff");
        assert!(try_("", &up, &["sora"]).is_err(), "path outside the release");
        // a test pack only in a local_test release
        let m = pack_manifest("", &ok).replace(r#""name":"Two","#, r#""name":"Two","test":true,"#);
        assert!(Release::from_files(&m, pack_files(&["sora"])).is_err());
        let m = pack_manifest(r#""local_test":true,"#, &ok).replace(r#""name":"Two","#, r#""name":"Two","test":true,"#);
        assert!(Release::from_files(&m, pack_files(&["sora"])).is_ok());
        // wrong size of a pack file
        let mut f = pack_files(&["sora"]);
        f.insert("packs/sora.bin".into(), Cow::Owned(vec![1]));
        assert!(Release::from_files(&pack_manifest("", &ok), f).is_err());
        // format 1/2 still need output and refuse packs
        assert!(Release::from_parts(r#"{"format":1,"title":"t","version":"1","input":{"disc_id":"X","size":1,"sha256":"a"}}"#, Vec::new()).is_err());
    }

    #[test]
    fn sniff_kinds() {
        assert_eq!(sniff(b"PK\x03\x04....").unwrap().code, "archive");
        assert_eq!(sniff(b"CISO\0\0").unwrap().params["kind"], "CISO");
        assert_eq!(sniff(b"\0PBP\0\0").unwrap().code, "eboot");
        assert!(sniff(&[0u8; 16]).is_none());
    }
}
