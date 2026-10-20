//! Streaming VCDIFF (RFC 3284) decoder for patches made by `xdelta3 -e -S none`.
//!
//! Pure Rust, no external binary. Memory use = one target window (xdelta3 default 8 MiB) + a
//! 4 MiB read cache: COPY-from-source bytes are read from the player's file on demand, never as a
//! whole source segment. So patches made with a whole-disc source window (`-B 2147483648`, which
//! lets xdelta3 COPY every vanilla byte instead of storing it) decode with the same small memory.
//!
//! Supported: default code table, VCD_SOURCE windows, xdelta3's per-window Adler-32
//! (checked), xdelta3's application header (skipped).
//! Refused with a clear developer error: secondary compression (`-S djw` / `-S lzma`,
//! re-encode with `-S none`), custom code tables, VCD_TARGET windows.

use std::io::{Read, Seek, SeekFrom, Write};

#[derive(Debug)]
pub enum DecodeError {
    /// The patch bytes are not a valid / supported VCDIFF stream (developer problem).
    BadPatch(String),
    /// Reading the source (player's file) or writing the output failed.
    Io(std::io::Error),
    /// A window's Adler-32 did not match (patch does not fit this source, or corruption).
    Checksum { window: u64 },
    /// The cancel callback asked us to stop.
    Cancelled,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::BadPatch(s) => write!(f, "bad patch: {s}"),
            DecodeError::Io(e) => write!(f, "i/o: {e}"),
            DecodeError::Checksum { window } => write!(f, "checksum mismatch in window {window}"),
            DecodeError::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl From<std::io::Error> for DecodeError {
    fn from(e: std::io::Error) -> Self {
        DecodeError::Io(e)
    }
}

fn bad<T>(msg: &str) -> Result<T, DecodeError> {
    Err(DecodeError::BadPatch(msg.to_string()))
}

const VCD_DECOMPRESS: u8 = 0x01;
const VCD_CODETABLE: u8 = 0x02;
const VCD_APPHEADER: u8 = 0x04; // xdelta3 extension
const VCD_SOURCE: u8 = 0x01;
const VCD_TARGET: u8 = 0x02;
const VCD_ADLER32: u8 = 0x04; // xdelta3 extension

const NOOP: u8 = 0;
const ADD: u8 = 1;
const RUN: u8 = 2;
const COPY: u8 = 3;

const S_NEAR: usize = 4;
const S_SAME: usize = 3;

#[derive(Clone, Copy, Default)]
struct Inst {
    kind: u8,
    size: u8,
    mode: u8,
}

/// RFC 3284 section 5.6 default code table.
fn default_code_table() -> [(Inst, Inst); 256] {
    let mut t = [(Inst::default(), Inst::default()); 256];
    let mut i = 0usize;
    let n = |kind, size, mode| Inst { kind, size, mode };
    t[i] = (n(RUN, 0, 0), n(NOOP, 0, 0));
    i += 1;
    for size in 0..=17u8 {
        t[i] = (n(ADD, size, 0), n(NOOP, 0, 0));
        i += 1;
    }
    for mode in 0..9u8 {
        t[i] = (n(COPY, 0, mode), n(NOOP, 0, 0));
        i += 1;
        for size in 4..=18u8 {
            t[i] = (n(COPY, size, mode), n(NOOP, 0, 0));
            i += 1;
        }
    }
    for mode in 0..6u8 {
        for add in 1..=4u8 {
            for copy in 4..=6u8 {
                t[i] = (n(ADD, add, 0), n(COPY, copy, mode));
                i += 1;
            }
        }
    }
    for mode in 6..9u8 {
        for add in 1..=4u8 {
            t[i] = (n(ADD, add, 0), n(COPY, 4, mode));
            i += 1;
        }
    }
    for mode in 0..9u8 {
        t[i] = (n(COPY, 4, mode), n(ADD, 1, 0));
        i += 1;
    }
    debug_assert_eq!(i, 256);
    t
}

/// Cursor over a byte slice with VCDIFF integer reading.
struct Cur<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn new(b: &'a [u8]) -> Self {
        Cur { b, p: 0 }
    }
    fn done(&self) -> bool {
        self.p >= self.b.len()
    }
    fn byte(&mut self) -> Result<u8, DecodeError> {
        match self.b.get(self.p) {
            Some(&v) => {
                self.p += 1;
                Ok(v)
            }
            None => bad("unexpected end of data"),
        }
    }
    fn int(&mut self) -> Result<u64, DecodeError> {
        let mut v: u64 = 0;
        for _ in 0..10 {
            let c = self.byte()?;
            if v > (u64::MAX >> 7) {
                return bad("integer overflow");
            }
            v = (v << 7) | u64::from(c & 0x7f);
            if c & 0x80 == 0 {
                return Ok(v);
            }
        }
        bad("integer too long")
    }
    fn usize(&mut self) -> Result<usize, DecodeError> {
        usize::try_from(self.int()?).or_else(|_| bad("value too large"))
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.b.len() - self.p < n {
            return bad("section runs past end of patch");
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
}

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += u32::from(x);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

/// On-demand reader for the source segment: one aligned chunk cached (COPYs are mostly sequential).
struct SrcCache {
    start: u64,
    buf: Vec<u8>,
}

const SRC_CHUNK: u64 = 4 << 20;

impl SrcCache {
    /// Append `n` source bytes at absolute file offset `pos` to `dst`.
    fn copy<S: Read + Seek>(&mut self, source: &mut S, mut pos: u64, mut n: usize, dst: &mut Vec<u8>) -> Result<(), DecodeError> {
        while n > 0 {
            let end = self.start + self.buf.len() as u64;
            if pos < self.start || pos >= end {
                self.start = pos - pos % SRC_CHUNK;
                self.buf.resize(SRC_CHUNK as usize, 0);
                source.seek(SeekFrom::Start(self.start))?;
                let mut got = 0usize;
                while got < self.buf.len() {
                    match source.read(&mut self.buf[got..]) {
                        Ok(0) => break,
                        Ok(k) => got += k,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                self.buf.truncate(got);
                if pos >= self.start + got as u64 {
                    return Err(DecodeError::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "source file shorter than the patch expects",
                    )));
                }
            }
            let off = (pos - self.start) as usize;
            let k = n.min(self.buf.len() - off);
            dst.extend_from_slice(&self.buf[off..off + k]);
            pos += k as u64;
            n -= k;
        }
        Ok(())
    }
}

/// Progress report: bytes of output written so far.
pub trait Sink {
    fn progress(&mut self, written: u64);
    fn cancelled(&self) -> bool;
}

/// Decode `patch` against `source`, writing the result to `out`.
/// Returns the number of bytes written.
pub fn decode<S: Read + Seek, W: Write, P: Sink>(
    patch: &[u8],
    source: &mut S,
    out: &mut W,
    sink: &mut P,
) -> Result<u64, DecodeError> {
    let table = default_code_table();
    let mut c = Cur::new(patch);
    if c.take(4).ok() != Some(&[0xD6, 0xC3, 0xC4, 0x00][..]) {
        return bad("not a VCDIFF/xdelta3 file (magic)");
    }
    let hdr = c.byte()?;
    if hdr & !(VCD_DECOMPRESS | VCD_CODETABLE | VCD_APPHEADER) != 0 {
        return bad("unknown header bits");
    }
    if hdr & VCD_DECOMPRESS != 0 {
        let _secondary_id = c.byte()?; // only an error if a window actually uses it
    }
    if hdr & VCD_CODETABLE != 0 {
        return bad("custom code tables are not supported");
    }
    if hdr & VCD_APPHEADER != 0 {
        let n = c.usize()?;
        c.take(n)?;
    }

    let mut src = SrcCache { start: 0, buf: Vec::new() };
    let mut tgt: Vec<u8> = Vec::new();
    let mut written: u64 = 0;
    let mut window: u64 = 0;

    while !c.done() {
        if sink.cancelled() {
            return Err(DecodeError::Cancelled);
        }
        let win = c.byte()?;
        if win & !(VCD_SOURCE | VCD_TARGET | VCD_ADLER32) != 0 {
            return bad("unknown window bits");
        }
        if win & VCD_TARGET != 0 {
            return bad("VCD_TARGET windows are not supported");
        }
        let (sseg_len, sseg_pos) = if win & VCD_SOURCE != 0 {
            (c.usize()?, c.int()?)
        } else {
            (0, 0)
        };
        let enc_len = c.usize()?;
        let enc_start = c.p;
        let tgt_len = c.usize()?;
        let delta_ind = c.byte()?;
        if delta_ind != 0 {
            return bad("secondary compression is used; re-encode the patch with `xdelta3 -S none`");
        }
        let data_len = c.usize()?;
        let inst_len = c.usize()?;
        let addr_len = c.usize()?;
        let cksum = if win & VCD_ADLER32 != 0 {
            let b = c.take(4)?;
            Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        } else {
            None
        };
        let mut data = Cur::new(c.take(data_len)?);
        let mut inst = Cur::new(c.take(inst_len)?);
        let mut addr = Cur::new(c.take(addr_len)?);
        if c.p - enc_start != enc_len {
            return bad("window length mismatch");
        }

        // Source segment = player's file [sseg_pos, sseg_pos + sseg_len), read on demand by COPY.
        if sseg_pos.checked_add(sseg_len as u64).is_none() {
            return bad("source segment out of range");
        }

        tgt.clear();
        tgt.reserve(tgt_len);
        let mut near = [0usize; S_NEAR];
        let mut next_near = 0usize;
        let mut same = [0usize; S_SAME * 256];

        while !inst.done() {
            let code = inst.byte()? as usize;
            let (i1, i2) = table[code];
            for ins in [i1, i2] {
                if ins.kind == NOOP {
                    continue;
                }
                let size = if ins.size == 0 { inst.usize()? } else { ins.size as usize };
                if tgt.len() + size > tgt_len {
                    return bad("instruction overflows target window");
                }
                match ins.kind {
                    ADD => tgt.extend_from_slice(data.take(size)?),
                    RUN => {
                        let b = data.byte()?;
                        tgt.resize(tgt.len() + size, b);
                    }
                    _ => {
                        // COPY: decode address in the combined [source segment | target] space.
                        let here = sseg_len + tgt.len();
                        let mode = ins.mode as usize;
                        let a = match mode {
                            0 => addr.usize()?,
                            1 => here.checked_sub(addr.usize()?).ok_or_else(|| {
                                DecodeError::BadPatch("bad HERE address".into())
                            })?,
                            m if m < 2 + S_NEAR => near[m - 2] + addr.usize()?,
                            m => same[(m - 2 - S_NEAR) * 256 + addr.byte()? as usize],
                        };
                        near[next_near] = a;
                        next_near = (next_near + 1) % S_NEAR;
                        same[a % (S_SAME * 256)] = a;
                        if a >= here {
                            return bad("COPY address points forward");
                        }
                        let mut a = a;
                        let mut left = size;
                        if a < sseg_len {
                            let n = left.min(sseg_len - a);
                            src.copy(source, sseg_pos + a as u64, n, &mut tgt)?;
                            a += n;
                            left -= n;
                        }
                        // Target part; may overlap what we are writing (run-like copies).
                        let mut t = a - sseg_len;
                        while left > 0 {
                            let avail = tgt.len() - t;
                            let n = left.min(avail);
                            tgt.extend_from_within(t..t + n);
                            t += n;
                            left -= n;
                        }
                    }
                }
            }
        }
        if tgt.len() != tgt_len {
            return bad("target window shorter than announced");
        }
        if let Some(want) = cksum {
            if adler32(&tgt) != want {
                return Err(DecodeError::Checksum { window });
            }
        }
        out.write_all(&tgt)?;
        written += tgt_len as u64;
        window += 1;
        sink.progress(written);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_table_matches_rfc_spot_checks() {
        let t = default_code_table();
        assert_eq!(t[0].0.kind, RUN);
        assert_eq!((t[1].0.kind, t[1].0.size), (ADD, 0));
        assert_eq!((t[18].0.kind, t[18].0.size), (ADD, 17));
        assert_eq!((t[19].0.kind, t[19].0.size, t[19].0.mode), (COPY, 0, 0));
        assert_eq!((t[162].0.kind, t[162].0.size, t[162].0.mode), (COPY, 18, 8));
        assert_eq!((t[163].0.size, t[163].1.kind, t[163].1.size), (1, COPY, 4));
        assert_eq!((t[255].0.kind, t[255].0.mode, t[255].1.kind), (COPY, 8, ADD));
    }

    #[test]
    fn adler_known_value() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }
}
