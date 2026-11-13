//! Payload compression for overlay manifest v4.
//!
//! Each payload file is stored with one of three codecs, chosen per file
//! at pack time; a file is only stored compressed when that actually
//! shrinks it:
//!
//! - [`CODEC_STORED`] (0) — raw bytes.
//! - [`CODEC_LZ`] (1) — the hand-rolled LZ codec in this module. It runs
//!   everywhere (including the Linux host, where the whole pack →
//!   install → uninstall lifecycle is smoke-tested), so it is the
//!   fallback whenever the Windows Compression API is unavailable.
//! - [`CODEC_XPRESS_HUFF`] (2) — the Windows Compression API
//!   (`cabinet.dll`, `COMPRESS_ALGORITHM_XPRESS_HUFF`) via windows-sys.
//!   Preferred on Windows; decoding a 2-flagged payload requires Windows.
//!
//! Codec 1 format: a token stream. Each group starts with one flag byte;
//! bit *i* (LSB first) describes item *i* of the group — 1 means a
//! literal byte follows, 0 means a two-byte back-reference follows:
//! byte A = low 8 bits of (offset − 1), byte B = high 4 bits of
//! (offset − 1) in its low nibble and (length − 3) in its high nibble,
//! where offset is 1..=4096 bytes back and length is 3..=18.

pub const CODEC_STORED: u8 = 0;
pub const CODEC_LZ: u8 = 1;
pub const CODEC_XPRESS_HUFF: u8 = 2;

const WINDOW: usize = 4096;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 18;

/// Compresses `data` with the best available codec and returns
/// `(codec, stored_bytes)`. Stored (codec 0) is returned whenever no
/// codec shrinks the input.
pub fn compress_best(data: &[u8]) -> (u8, Vec<u8>) {
    let mut best: Option<(u8, Vec<u8>)> = None;
    let consider = |candidate: (u8, Vec<u8>), best: &mut Option<(u8, Vec<u8>)>| {
        if candidate.1.len() < data.len()
            && best
                .as_ref()
                .map(|(_, b)| candidate.1.len() < b.len())
                .unwrap_or(true)
        {
            *best = Some(candidate);
        }
    };
    #[cfg(windows)]
    {
        if let Ok(bytes) = win::compress_xpress_huff(data) {
            consider((CODEC_XPRESS_HUFF, bytes), &mut best);
        }
    }
    if data.len() >= MIN_MATCH {
        consider((CODEC_LZ, lz_compress(data)), &mut best);
    }
    match best {
        Some(found) => found,
        None => (CODEC_STORED, data.to_vec()),
    }
}

/// Decompresses `stored` (previously produced for `codec`) and checks the
/// result is exactly `expected_len` bytes.
pub fn decompress(codec: u8, stored: &[u8], expected_len: usize) -> std::io::Result<Vec<u8>> {
    let out = match codec {
        CODEC_STORED => stored.to_vec(),
        CODEC_LZ => lz_decompress(stored, expected_len)?,
        CODEC_XPRESS_HUFF => {
            #[cfg(windows)]
            {
                win::decompress_xpress_huff(stored, expected_len)?
            }
            #[cfg(not(windows))]
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "this payload uses the Windows Compression API (XPRESS Huffman) \
                     and can only be unpacked on Windows",
                ));
            }
        }
        other => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown payload compression flag {other}"),
            ))
        }
    };
    if out.len() != expected_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "decompressed payload is {} bytes, manifest says {expected_len}",
                out.len()
            ),
        ));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// The LZ codec (codec 1).

fn hash3(data: &[u8], pos: usize) -> usize {
    let v = (data[pos] as u32) | ((data[pos + 1] as u32) << 8) | ((data[pos + 2] as u32) << 16);
    (v.wrapping_mul(0x9E37_79B1) >> 17) as usize & 0x7FFF
}

pub fn lz_compress(data: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(data.len() / 2 + 16);
    // Last seen position for each 3-byte hash, -1 = none.
    let mut table: Vec<i32> = vec![-1; 0x8000];
    let mut pos = 0usize;
    while pos < data.len() {
        let flag_index = out.len();
        out.push(0u8); // patched as items are emitted
        let mut flags: u8 = 0;
        for bit in 0..8 {
            if pos >= data.len() {
                break;
            }
            let mut matched = 0usize;
            let mut match_len = 0usize;
            if pos + MIN_MATCH <= data.len() {
                let candidate = table[hash3(data, pos)];
                if candidate >= 0 {
                    let cand = candidate as usize;
                    if cand < pos && pos - cand <= WINDOW {
                        let max_len = MAX_MATCH.min(data.len() - pos);
                        let mut len = 0usize;
                        while len < max_len && data[cand + len] == data[pos + len] {
                            len += 1;
                        }
                        if len >= MIN_MATCH {
                            matched = pos - cand;
                            match_len = len;
                        }
                    }
                }
            }
            if matched > 0 {
                let offset_minus_1 = (matched - 1) as u32;
                out.push((offset_minus_1 & 0xFF) as u8);
                out.push(
                    (((offset_minus_1 >> 8) & 0x0F) as u8) | (((match_len - MIN_MATCH) as u8) << 4),
                );
                // Index every position the match spans so later matches
                // can reference into it, then skip past it.
                let end = pos + match_len;
                while pos < end {
                    if pos + MIN_MATCH <= data.len() {
                        table[hash3(data, pos)] = pos as i32;
                    }
                    pos += 1;
                }
            } else {
                flags |= 1 << bit;
                out.push(data[pos]);
                if pos + MIN_MATCH <= data.len() {
                    table[hash3(data, pos)] = pos as i32;
                }
                pos += 1;
            }
        }
        out[flag_index] = flags;
    }
    out
}

pub fn lz_decompress(stored: &[u8], expected_len: usize) -> std::io::Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(expected_len);
    let mut i = 0usize;
    while i < stored.len() && out.len() < expected_len {
        let flags = stored[i];
        i += 1;
        for bit in 0..8 {
            if out.len() >= expected_len {
                break;
            }
            if i >= stored.len() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "compressed payload ends mid-group",
                ));
            }
            if flags & (1 << bit) != 0 {
                out.push(stored[i]);
                i += 1;
            } else {
                if i + 1 >= stored.len() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "compressed payload ends mid-reference",
                    ));
                }
                let a = stored[i] as usize;
                let b = stored[i + 1] as usize;
                i += 2;
                let offset = (((b & 0x0F) << 8) | a) + 1;
                let len = (b >> 4) + MIN_MATCH;
                if offset > out.len() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "back-reference points before the start of the payload",
                    ));
                }
                let start = out.len() - offset;
                for k in 0..len {
                    let byte = out[start + k];
                    out.push(byte);
                }
            }
        }
    }
    if out.len() != expected_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "decompressed payload is {} bytes, manifest says {expected_len}",
                out.len()
            ),
        ));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Windows Compression API (codec 2).

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Storage::Compression::*;

    pub fn compress_xpress_huff(data: &[u8]) -> std::io::Result<Vec<u8>> {
        unsafe {
            let mut handle: COMPRESSOR_HANDLE = std::ptr::null_mut();
            if CreateCompressor(
                COMPRESS_ALGORITHM_XPRESS_HUFF,
                std::ptr::null(),
                &mut handle,
            ) == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            let mut capacity = data.len() + data.len() / 8 + 1024;
            let result = loop {
                let mut buffer = vec![0u8; capacity];
                let mut written: usize = 0;
                let ok = Compress(
                    handle,
                    data.as_ptr() as *const std::ffi::c_void,
                    data.len(),
                    buffer.as_mut_ptr() as *mut std::ffi::c_void,
                    buffer.len(),
                    &mut written,
                );
                if ok != 0 {
                    buffer.truncate(written);
                    break Ok(buffer);
                }
                let err = std::io::Error::last_os_error();
                // ERROR_BUFFER_TOO_SMALL (122): grow and retry, bounded.
                if err.raw_os_error() == Some(122) && capacity < data.len() * 2 + 4096 {
                    capacity = capacity * 2 + 1024;
                    continue;
                }
                break Err(err);
            };
            CloseCompressor(handle);
            result
        }
    }

    pub fn decompress_xpress_huff(stored: &[u8], expected_len: usize) -> std::io::Result<Vec<u8>> {
        unsafe {
            let mut handle: DECOMPRESSOR_HANDLE = std::ptr::null_mut();
            if CreateDecompressor(
                COMPRESS_ALGORITHM_XPRESS_HUFF,
                std::ptr::null(),
                &mut handle,
            ) == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            let mut buffer = vec![0u8; expected_len];
            let mut written: usize = 0;
            let ok = Decompress(
                handle,
                stored.as_ptr() as *const std::ffi::c_void,
                stored.len(),
                buffer.as_mut_ptr() as *mut std::ffi::c_void,
                buffer.len(),
                &mut written,
            );
            CloseDecompressor(handle);
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            buffer.truncate(written);
            Ok(buffer)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(kind: u8, len: usize) -> Vec<u8> {
        match kind {
            0 => (0..len).map(|i| (i * 31 + 7) as u8).collect(), // incompressible-ish
            1 => (0..len).map(|i| b"abcd"[(i / 3) % 4]).collect(), // very repetitive
            _ => {
                let mut v = Vec::with_capacity(len);
                let mut state = 0x1234_5678u32;
                while v.len() < len {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    v.push((state >> 24) as u8);
                }
                v
            }
        }
    }

    #[test]
    fn lz_round_trip_all_kinds() {
        for kind in 0..3 {
            for len in [0usize, 1, 2, 3, 17, 100, 5000, 70_000] {
                let data = sample(kind, len);
                let packed = lz_compress(&data);
                let back = lz_decompress(&packed, data.len()).unwrap();
                assert_eq!(back, data, "kind {kind} len {len}");
            }
        }
    }

    #[test]
    fn lz_shrinks_repetitive_and_rejects_corruption() {
        let data = sample(1, 10_000);
        let packed = lz_compress(&data);
        assert!(
            packed.len() < data.len() / 4,
            "{} vs {}",
            packed.len(),
            data.len()
        );
        let mut corrupt = packed.clone();
        let mid = corrupt.len() / 2;
        corrupt[mid] = corrupt[mid].wrapping_add(1);
        // Corruption may still decode (LZ has no checksum); it must never
        // panic, and a truncated stream must error.
        let _ = lz_decompress(&corrupt, data.len());
        assert!(lz_decompress(&packed[..packed.len() / 2], data.len()).is_err());
    }

    #[test]
    fn compress_best_picks_shrinking_codec_and_round_trips() {
        let repetitive = sample(1, 20_000);
        let (codec, stored) = compress_best(&repetitive);
        assert_ne!(codec, CODEC_STORED);
        assert!(stored.len() < repetitive.len());
        assert_eq!(
            decompress(codec, &stored, repetitive.len()).unwrap(),
            repetitive
        );

        let random = sample(2, 4096);
        let (codec, stored) = compress_best(&random);
        assert_eq!(codec, CODEC_STORED);
        assert_eq!(decompress(codec, &stored, random.len()).unwrap(), random);

        let empty: Vec<u8> = Vec::new();
        let (codec, stored) = compress_best(&empty);
        assert_eq!(codec, CODEC_STORED);
        assert_eq!(decompress(codec, &stored, 0).unwrap(), empty);
    }

    #[test]
    fn decompress_rejects_unknown_codec_and_wrong_length() {
        assert!(decompress(9, b"abc", 3).is_err());
        assert!(decompress(CODEC_STORED, b"abc", 4).is_err());
    }
}
