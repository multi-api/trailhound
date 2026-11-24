use crate::{Error, Result};
use roaring::RoaringBitmap;
use std::io::Write;
use std::path::Path;

pub fn write_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub fn read_varint(buf: &[u8], pos: &mut usize) -> Result<u64> {
    let mut v = 0u64;
    let mut shift = 0u32;
    loop {
        let b = *buf.get(*pos).ok_or_else(|| Error::corrupt("truncated varint"))?;
        *pos += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
        shift += 7;
        if shift > 63 {
            return Err(Error::corrupt("varint too long"));
        }
    }
}

/// Small sets are stored as delta-encoded varints (1-2 bytes per doc),
/// larger ones as a roaring bitmap.
pub fn write_docset(out: &mut Vec<u8>, bm: &RoaringBitmap) {
    if bm.len() <= 32 {
        out.push(0);
        write_varint(out, bm.len());
        let mut prev = 0u32;
        for d in bm {
            write_varint(out, (d - prev) as u64);
            prev = d;
        }
    } else {
        out.push(1);
        write_varint(out, bm.serialized_size() as u64);
        bm.serialize_into(&mut *out).expect("write to vec");
    }
}

pub fn read_docset(buf: &[u8], pos: &mut usize) -> Result<RoaringBitmap> {
    let tag = *buf.get(*pos).ok_or_else(|| Error::corrupt("truncated docset"))?;
    *pos += 1;
    match tag {
        0 => {
            let n = read_varint(buf, pos)?;
            let mut bm = RoaringBitmap::new();
            let mut prev = 0u32;
            for _ in 0..n {
                prev = prev.wrapping_add(read_varint(buf, pos)? as u32);
                bm.insert(prev);
            }
            Ok(bm)
        }
        1 => {
            let len = read_varint(buf, pos)? as usize;
            let end = pos.checked_add(len).filter(|e| *e <= buf.len()).ok_or_else(|| Error::corrupt("truncated bitmap"))?;
            let bm = RoaringBitmap::deserialize_from(&buf[*pos..end]).map_err(|e| Error::corrupt(format!("bad bitmap: {e}")))?;
            *pos = end;
            Ok(bm)
        }
        _ => Err(Error::corrupt("unknown docset tag")),
    }
}

/// Order-preserving encoding of an f64 as 8 big-endian bytes.
pub fn encode_f64(f: f64) -> [u8; 8] {
    let bits = f.to_bits();
    let ordered = if bits >> 63 == 0 { bits ^ (1 << 63) } else { !bits };
    ordered.to_be_bytes()
}

pub fn decode_f64(b: &[u8]) -> f64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[..8]);
    let ordered = u64::from_be_bytes(a);
    let bits = if ordered >> 63 == 1 { ordered ^ (1 << 63) } else { !ordered };
    f64::from_bits(bits)
}

pub fn sync_dir(_dir: &Path) {
    #[cfg(unix)]
    if let Ok(f) = std::fs::File::open(_dir) {
        let _ = f.sync_all();
    }
}

/// Write `bytes` to `path` so that after a crash the file holds either the
/// old content or the new content, never a partial write.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        sync_dir(dir);
    }
    Ok(())
}

pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}
