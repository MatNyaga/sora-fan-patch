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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub title: String,
    pub version: String,
    pub input: DiscInfo,
    pub output: OutputInfo,
    #[serde(default)]
    pub older_outputs: Vec<OlderOutput>,
    /// Other releases of the same game: disc id -> region word shown to the player.
    #[serde(default)]
    pub other_regions: BTreeMap<String, String>,
    /// Format 2: what the patch's source is, when it is not the player's file byte for byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceInfo>,
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
    /// The VCDIFF patch; `Borrowed` when embedded in the app binary (no copy in RAM).
    pub delta: Cow<'static, [u8]>,
}

impl Release {
    pub fn from_parts(manifest_json: &str, delta: impl Into<Cow<'static, [u8]>>) -> Result<Self, String> {
        let delta = delta.into();
        let manifest: Manifest = serde_json::from_str(manifest_json).map_err(|e| e.to_string())?;
        match (manifest.format, &manifest.source) {
            (1, None) => {}
            (2, Some(s)) if s.kind == SOURCE_PLAIN_EBOOT && s.plain_size <= s.encrypted_size => {}
            (2, Some(s)) => return Err(format!("unknown or bad patch source kind {:?}", s.kind)),
            (2, None) => return Err("manifest format 2 needs a \"source\"".into()),
            (f, _) => return Err(format!("unknown manifest format {f}")),
        }
        Ok(Release { manifest, delta })
    }
    pub fn load_dir(dir: &Path) -> Result<Self, String> {
        let m = fs::read_to_string(dir.join("release.json")).map_err(|e| e.to_string())?;
        let d = fs::read(dir.join("patch.vcdiff")).map_err(|e| e.to_string())?;
        Self::from_parts(&m, d)
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
    if size < m.input.size && size != m.output.size {
        return Err(Problem::new("incomplete").with("have", gb(size)).with("want", gb(m.input.size)));
    }
    let sum = sha256_file(path, size, Stage::Checking, prog)?;
    if sum == m.input.sha256 {
        return Ok(());
    }
    if sum == m.output.sha256 {
        return Err(Problem::new("already_patched").with("version", &m.version));
    }
    if let Some(o) = m.older_outputs.iter().find(|o| o.sha256 == sum) {
        return Err(Problem::new("older_patch").with("version", &o.version));
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
    let dir = match out_dir {
        Some(d) => d.to_path_buf(),
        None => input.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(".")),
    };
    let need = rel.manifest.output.size + 64 * 1024 * 1024;
    if let Some(have) = free_space(&dir) {
        if have < need {
            return Err(Problem::new("no_space").with("need", gb(need)).with("have", gb(have)));
        }
    }
    let name = &rel.manifest.output.file_name;
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

/// Step 2b + 3: build the patched copy at `out` (from plan_output) and check it.
pub fn build<P: Progress>(rel: &Release, input: &Path, out: &Path, prog: &mut P) -> Result<PathBuf, Problem> {
    let m = &rel.manifest;
    let tmp = temp_path(out);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| Problem::new("cant_write").detail(e))?;
    let mut guard = TempGuard { path: tmp.clone(), armed: true };
    let mut src = open_source(rel, input)?;
    let mut w = HashingWriter { inner: BufWriter::with_capacity(1 << 20, file), hash: Sha256::new() };
    let mut sink = DecodeSink { prog, total: m.output.size };
    let written = match vcdiff::decode(&rel.delta, &mut src, &mut w, &mut sink) {
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
    if written != m.output.size || sum != m.output.sha256 {
        return Err(Problem::new("build_failed")
            .with("code", "E-HASH")
            .detail(format!("size {written} sha256 {sum}")));
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

    #[test]
    fn sniff_kinds() {
        assert_eq!(sniff(b"PK\x03\x04....").unwrap().code, "archive");
        assert_eq!(sniff(b"CISO\0\0").unwrap().params["kind"], "CISO");
        assert_eq!(sniff(b"\0PBP\0\0").unwrap().code, "eboot");
        assert!(sniff(&[0u8; 16]).is_none());
    }
}
