//! Decrypts the player's own `~PSP`-encrypted EBOOT.BIN to the plain ELF the patch was made against.
//!
//! SPDX-License-Identifier: GPL-3.0-or-later
//!
//! This is a Rust port of the PSP executable decryption in pspdecrypt
//! (<https://github.com/John-K/pspdecrypt>, GPL-3.0), whose PRX code comes from PPSSPP
//! (Core/ELF/PrxDecrypter.cpp, GPL-2.0-or-later) and whose KIRK engine is libkirk by Draan and
//! the PSP scene (GPL-3.0-or-later). Only what one game needs is ported: the "type 2" ~PSP
//! format with tag 0xD91613F0 (the tag of the US Dissidia 012 EBOOT), KIRK command 7
//! (AES-128-CBC with a key-vault key), KIRK command 1 (AES-128-CBC with a per-file key wrapped by
//! the KIRK-1 key) and the command-10 AES-CMAC checks. The key bytes below are the publicly known
//! PSP tag/KIRK keys from those projects; nothing here is derived from the game.
//!
//! Byte-exact with pspdecrypt: the output is the first `0xB0` (decrypt size) bytes of the
//! CBC-decrypted payload. The caller still compares the result with the release's SHA-256.

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;
use sha1::{Digest, Sha1};

/// KIRK command 1 master key (wraps the per-file AES + CMAC keys).
const KIRK1_KEY: [u8; 16] = [
    0x98, 0xC9, 0x40, 0x97, 0x5C, 0x1D, 0x10, 0xE8, 0x7F, 0xE6, 0x0E, 0xA3, 0xFD, 0x03, 0xA8, 0xBA,
];

struct TagInfo {
    tag: u32,
    /// Per-tag seed (pspdecrypt `key_D91613F0`).
    seed: [u8; 16],
    /// KIRK key-vault slot used by command 7 for this tag, and that slot's key.
    code: u8,
    kirk7_key: [u8; 16],
}

/// Tags we can decrypt. ULUS-10566 EBOOT.BIN = 0xD91613F0 (type 2, KIRK key 0x5D).
const TAGS: &[TagInfo] = &[TagInfo {
    tag: 0xD916_13F0,
    seed: [0xEB, 0xFF, 0x40, 0xD8, 0xB4, 0x1A, 0xE1, 0x66, 0x91, 0x3B, 0x8F, 0x64, 0xB6, 0xFC, 0xB7, 0x12],
    code: 0x5D,
    kirk7_key: [0x11, 0x5A, 0x5D, 0x20, 0xD5, 0x3A, 0x8D, 0xD3, 0x9C, 0xC5, 0xAF, 0x41, 0x0F, 0x0F, 0x18, 0x6F],
}];

#[derive(Debug, PartialEq, Eq)]
pub enum DecryptError {
    /// Not a ~PSP file (or too short to hold its 0x150-byte header).
    NotPsp,
    /// A ~PSP file with a tag we do not have keys for.
    UnknownTag(u32),
    /// Header sizes do not fit the file.
    BadSize,
    /// Type-2 header SHA-1 mismatch (wrong keys or damaged header).
    HeaderHash,
    /// KIRK command-10 CMAC of the KIRK header / of the data did not match (damaged file).
    KirkHeaderCmac,
    KirkDataCmac,
}

impl std::fmt::Display for DecryptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecryptError::NotPsp => write!(f, "not a ~PSP encrypted executable"),
            DecryptError::UnknownTag(t) => write!(f, "unsupported ~PSP tag {t:08X}"),
            DecryptError::BadSize => write!(f, "~PSP header sizes do not fit the file"),
            DecryptError::HeaderHash => write!(f, "~PSP header SHA-1 mismatch"),
            DecryptError::KirkHeaderCmac => write!(f, "KIRK header CMAC mismatch"),
            DecryptError::KirkDataCmac => write!(f, "KIRK data CMAC mismatch"),
        }
    }
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// AES-128-CBC decrypt in place, IV = 0, whole 16-byte blocks (KIRK command 7 style).
fn cbc_decrypt(key: &[u8; 16], data: &mut [u8]) {
    debug_assert_eq!(data.len() % 16, 0);
    let aes = Aes128::new(GenericArray::from_slice(key));
    let mut prev = [0u8; 16];
    for blk in data.chunks_exact_mut(16) {
        let mut cur = [0u8; 16];
        cur.copy_from_slice(blk);
        let b = GenericArray::from_mut_slice(blk);
        aes.decrypt_block(b);
        for (x, p) in blk.iter_mut().zip(prev.iter()) {
            *x ^= p;
        }
        prev = cur;
    }
}

/// AES-CMAC (RFC 4493), as libkirk's AES_CMAC.
fn cmac(key: &[u8; 16], msg: &[u8]) -> [u8; 16] {
    let aes = Aes128::new(GenericArray::from_slice(key));
    let enc = |b: &mut [u8; 16]| aes.encrypt_block(GenericArray::from_mut_slice(b));
    let dbl = |x: &[u8; 16]| {
        let mut o = [0u8; 16];
        for i in 0..16 {
            o[i] = (x[i] << 1) | if i < 15 { x[i + 1] >> 7 } else { 0 };
        }
        if x[0] & 0x80 != 0 {
            o[15] ^= 0x87;
        }
        o
    };
    let mut l = [0u8; 16];
    enc(&mut l);
    let k1 = dbl(&l);
    let k2 = dbl(&k1);
    let n = msg.len().div_ceil(16).max(1);
    let complete = !msg.is_empty() && msg.len() % 16 == 0;
    let mut x = [0u8; 16];
    for i in 0..n {
        let mut blk = [0u8; 16];
        let chunk = &msg[i * 16..msg.len().min(i * 16 + 16)];
        blk[..chunk.len()].copy_from_slice(chunk);
        if i == n - 1 {
            if complete {
                blk.iter_mut().zip(k1.iter()).for_each(|(b, k)| *b ^= k);
            } else {
                blk[chunk.len()] = 0x80;
                blk.iter_mut().zip(k2.iter()).for_each(|(b, k)| *b ^= k);
            }
        }
        x.iter_mut().zip(blk.iter()).for_each(|(a, b)| *a ^= b);
        enc(&mut x);
    }
    x
}

/// Is this a ~PSP file whose tag we can decrypt? (No crypto, header only.)
pub fn supported(enc: &[u8]) -> Result<u32, DecryptError> {
    if enc.len() < 0x150 || &enc[..4] != b"~PSP" {
        return Err(DecryptError::NotPsp);
    }
    let tag = le32(enc, 0xD0);
    if TAGS.iter().any(|t| t.tag == tag) {
        Ok(tag)
    } else {
        Err(DecryptError::UnknownTag(tag))
    }
}

/// Decrypt a type-2 ~PSP executable (EBOOT.BIN) to the plain ELF, byte-exact with pspdecrypt.
pub fn decrypt(enc: &[u8]) -> Result<Vec<u8>, DecryptError> {
    let tag = supported(enc)?;
    let ti = TAGS.iter().find(|t| t.tag == tag).expect("checked");
    debug_assert_eq!(ti.code, 0x5D);
    let k7 = &ti.kirk7_key;
    let out_size = i32::from_le_bytes(enc[0xB0..0xB4].try_into().unwrap());
    if out_size <= 0 {
        return Err(DecryptError::BadSize);
    }

    // Seed expansion: 9 blocks of the tag seed, block i starts with byte i, KIRK-7 decrypted.
    let mut xorbuf = [0u8; 0x90];
    for (i, blk) in xorbuf.chunks_exact_mut(16).enumerate() {
        blk.copy_from_slice(&ti.seed);
        blk[0] = i as u8;
    }
    cbc_decrypt(k7, &mut xorbuf);

    // Type-2 header image (PPSSPP's PRXType2 struct, 0x150 bytes):
    // 0x00 tag | 0x04 zeros[0x58] | 0x5C id[0x10] | 0x6C sha1[0x14] | 0x80 kirk header[0x40]
    // | 0xC0 kirk metadata[0x10] | 0xD0 first 0x80 bytes of the file.
    let mut h = [0u8; 0x150];
    h[0..4].copy_from_slice(&enc[0xD0..0xD4]);
    h[0x5C..0x6C].copy_from_slice(&enc[0x140..0x150]);
    h[0x6C..0x80].copy_from_slice(&enc[0x12C..0x140]);
    h[0x80..0xB0].copy_from_slice(&enc[0x80..0xB0]);
    h[0xB0..0xC0].copy_from_slice(&enc[0xC0..0xD0]);
    h[0xC0..0xD0].copy_from_slice(&enc[0xB0..0xC0]);
    h[0xD0..0x150].copy_from_slice(&enc[0..0x80]);
    cbc_decrypt(k7, &mut h[0x5C..0xBC]);

    let mut sha = Sha1::new();
    sha.update(&h[0..4]);
    sha.update(&xorbuf[..0x10]);
    sha.update(&h[4..0x6C]);
    sha.update(&h[0x80..0x150]);
    if sha.finalize()[..] != h[0x6C..0x80] {
        return Err(DecryptError::HeaderHash);
    }

    // KIRK command-1 buffer: 0x90 header | 0x80 file header | encrypted payload (file 0x150..).
    let mut k = vec![0u8; enc.len() - 0x40];
    k[0x70..0x80].copy_from_slice(&h[0xC0..0xD0]);
    k[0x90..0x110].copy_from_slice(&enc[0..0x80]);
    k[0x110..].copy_from_slice(&enc[0x150..]);
    for i in 0..0x40 {
        k[i] = h[0x80 + i] ^ xorbuf[0x10 + i];
    }
    cbc_decrypt(k7, &mut k[0..0x40]);
    for i in 0..0x40 {
        k[i] ^= xorbuf[0x50 + i];
    }
    k[0x60..0x64].copy_from_slice(&1u32.to_le_bytes()); // mode = CMD1

    let data_size = le32(&k, 0x70) as usize;
    let data_offset = le32(&k, 0x74) as usize;
    let padded = data_size.div_ceil(16) * 16;
    let start = 0x90 + data_offset;
    if data_size == 0 || (out_size as usize) > padded || start.checked_add(padded).map_or(true, |e| e > k.len()) {
        return Err(DecryptError::BadSize);
    }

    let mut keys = [0u8; 32];
    keys.copy_from_slice(&k[0..0x20]);
    cbc_decrypt(&KIRK1_KEY, &mut keys);
    let aes_key: [u8; 16] = keys[0..16].try_into().unwrap();
    let cmac_key: [u8; 16] = keys[16..32].try_into().unwrap();
    if k[0x64] != 1 {
        // Not ECDSA-signed: KIRK command 10 checks both CMACs before decrypting.
        if cmac(&cmac_key, &k[0x60..0x90]) != k[0x20..0x30] {
            return Err(DecryptError::KirkHeaderCmac);
        }
        if cmac(&cmac_key, &k[0x60..0x90 + data_offset + padded]) != k[0x30..0x40] {
            return Err(DecryptError::KirkDataCmac);
        }
    }

    let mut plain = k[start..start + padded].to_vec();
    cbc_decrypt(&aes_key, &mut plain);
    plain.truncate(out_size as usize);
    Ok(plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cbc_encrypt(key: &[u8; 16], data: &mut [u8]) {
        let aes = Aes128::new(GenericArray::from_slice(key));
        let mut prev = [0u8; 16];
        for blk in data.chunks_exact_mut(16) {
            blk.iter_mut().zip(prev.iter()).for_each(|(b, p)| *b ^= p);
            aes.encrypt_block(GenericArray::from_mut_slice(blk));
            prev.copy_from_slice(blk);
        }
    }

    /// Small deterministic byte stream (no game bytes anywhere in the tests).
    fn bytes(seed: u32, n: usize) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2_654_435_761).max(1);
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x >> 24) as u8
            })
            .collect()
    }

    /// Build a synthetic type-2 ~PSP file for `plain` with the D91613F0 tag: the exact inverse of
    /// `decrypt`, so the test proves the format handling without any real game file.
    fn encrypt(plain: &[u8]) -> Vec<u8> {
        let ti = &TAGS[0];
        let k7 = &ti.kirk7_key;
        let data_size = plain.len();
        let padded = data_size.div_ceil(16) * 16;
        let mut enc = vec![0u8; 0x150 + padded];
        // File header (first 0x80 bytes, kept as-is by the format): magic + sizes + filler.
        enc[0..0x80].copy_from_slice(&bytes(1, 0x80));
        enc[0..4].copy_from_slice(b"~PSP");
        enc[0x28..0x2C].copy_from_slice(&(data_size as u32).to_le_bytes());
        let total = enc.len() as u32;
        enc[0x2C..0x30].copy_from_slice(&total.to_le_bytes());
        enc[0xD0..0xD4].copy_from_slice(&ti.tag.to_le_bytes());
        let mut meta = [0u8; 16];
        meta[0..4].copy_from_slice(&(data_size as u32).to_le_bytes());
        meta[4..8].copy_from_slice(&0x80u32.to_le_bytes());
        enc[0xB0..0xC0].copy_from_slice(&meta);

        let aes_key: [u8; 16] = bytes(2, 16).try_into().unwrap();
        let cmac_key: [u8; 16] = bytes(3, 16).try_into().unwrap();
        let mut payload = plain.to_vec();
        payload.resize(padded, 0);
        cbc_encrypt(&aes_key, &mut payload);
        enc[0x150..].copy_from_slice(&payload);

        // KIRK-1 buffer as decrypt() will rebuild it; compute both CMACs over it.
        let mut k = vec![0u8; enc.len() - 0x40];
        let mut wrapped = [0u8; 32];
        wrapped[..16].copy_from_slice(&aes_key);
        wrapped[16..].copy_from_slice(&cmac_key);
        cbc_encrypt(&KIRK1_KEY, &mut wrapped);
        k[0..0x20].copy_from_slice(&wrapped);
        k[0x60..0x64].copy_from_slice(&1u32.to_le_bytes());
        k[0x70..0x80].copy_from_slice(&meta);
        k[0x90..0x110].copy_from_slice(&enc[0..0x80]);
        k[0x110..].copy_from_slice(&enc[0x150..]);
        let hh = cmac(&cmac_key, &k[0x60..0x90]);
        let dh = cmac(&cmac_key, &k[0x60..0x90 + 0x80 + padded]);
        k[0x20..0x30].copy_from_slice(&hh);
        k[0x30..0x40].copy_from_slice(&dh);

        let mut xorbuf = [0u8; 0x90];
        for (i, blk) in xorbuf.chunks_exact_mut(16).enumerate() {
            blk.copy_from_slice(&ti.seed);
            blk[0] = i as u8;
        }
        cbc_decrypt(k7, &mut xorbuf);
        // Inverse of the KIRK-header unwrap: kh = E(k ^ x2) ^ x1.
        let mut kh = [0u8; 0x40];
        for i in 0..0x40 {
            kh[i] = k[i] ^ xorbuf[0x50 + i];
        }
        cbc_encrypt(k7, &mut kh);
        for i in 0..0x40 {
            kh[i] ^= xorbuf[0x10 + i];
        }

        // Decrypted header image, then its SHA-1, then encrypt [0x5C..0xBC] back.
        let mut h = [0u8; 0x150];
        h[0..4].copy_from_slice(&enc[0xD0..0xD4]);
        h[0x5C..0x6C].copy_from_slice(&bytes(4, 16));
        h[0x80..0xC0].copy_from_slice(&kh);
        h[0xC0..0xD0].copy_from_slice(&meta);
        h[0xD0..0x150].copy_from_slice(&enc[0..0x80]);
        let mut sha = Sha1::new();
        sha.update(&h[0..4]);
        sha.update(&xorbuf[..0x10]);
        sha.update(&h[4..0x6C]);
        sha.update(&h[0x80..0x150]);
        h[0x6C..0x80].copy_from_slice(&sha.finalize());
        cbc_encrypt(k7, &mut h[0x5C..0xBC]);
        enc[0x140..0x150].copy_from_slice(&h[0x5C..0x6C]);
        enc[0x12C..0x140].copy_from_slice(&h[0x6C..0x80]);
        enc[0x80..0xB0].copy_from_slice(&h[0x80..0xB0]);
        enc[0xC0..0xD0].copy_from_slice(&h[0xB0..0xC0]);
        enc
    }

    #[test]
    fn cmac_rfc4493_vectors() {
        let key = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf, 0x4f, 0x3c,
        ];
        let msg = [
            0x6b, 0xc1, 0xbe, 0xe2, 0x2e, 0x40, 0x9f, 0x96, 0xe9, 0x3d, 0x7e, 0x11, 0x73, 0x93, 0x17, 0x2a, 0xae, 0x2d,
            0x8a, 0x57, 0x1e, 0x03, 0xac, 0x9c, 0x9e, 0xb7, 0x6f, 0xac, 0x45, 0xaf, 0x8e, 0x51, 0x30, 0xc8, 0x1c, 0x46,
            0xa3, 0x5c, 0xe4, 0x11,
        ];
        let hex = |b: [u8; 16]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        assert_eq!(hex(cmac(&key, &[])), "bb1d6929e95937287fa37d129b756746");
        assert_eq!(hex(cmac(&key, &msg[..16])), "070a16b46b4d4144f79bdd9dd04a287c");
        assert_eq!(hex(cmac(&key, &msg[..40])), "dfa66747de9ae63030ca32611497c827");
    }

    #[test]
    fn synthetic_type2_round_trip() {
        for len in [4usize, 16, 1000, 4097] {
            let mut plain = bytes(9 + len as u32, len);
            plain[..4].copy_from_slice(b"\x7fELF");
            let enc = encrypt(&plain);
            assert_eq!(supported(&enc), Ok(0xD916_13F0));
            assert_eq!(decrypt(&enc).unwrap(), plain, "len {len}");
        }
    }

    #[test]
    fn damage_is_detected() {
        let plain = bytes(5, 3000);
        let enc = encrypt(&plain);
        let mut bad = enc.clone();
        bad[0x150 + 100] ^= 1; // payload byte -> data CMAC
        assert_eq!(decrypt(&bad), Err(DecryptError::KirkDataCmac));
        let mut bad = enc.clone();
        bad[0x130] ^= 1; // header SHA-1 field
        assert_eq!(decrypt(&bad), Err(DecryptError::HeaderHash));
        let mut bad = enc.clone();
        bad[0xD0] ^= 1;
        assert!(matches!(decrypt(&bad), Err(DecryptError::UnknownTag(_))));
        assert_eq!(decrypt(b"\x7fELF not encrypted"), Err(DecryptError::NotPsp));
    }
}
