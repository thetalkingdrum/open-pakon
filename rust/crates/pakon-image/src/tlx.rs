//! Pakon TLXClientDemo planar 16-bit `.raw` loader.
//!
//! TLX saves each already flat-fielded, cropped, ICE-cleaned frame as a
//! standalone `.raw` file with a 16-byte header (`<4I`: `16, width, height, 48`)
//! followed by three little-endian `u16` planes (R, G, B) of `width * height`
//! 14-bit linear transmittance samples (`0..16383`).
//!
//! Opening any TLX `.raw` file gathers all matching sibling `.raw` frames in
//! the same folder (sorted naturally) and packs them into an `open-pakon`
//! ribbon `(N * width, height, 3)` pre-oriented so the standard `rot90(k=1)`
//! preview/export produces upright `(height, width, 3)` frames.

use std::cmp::Ordering;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use ndarray::{Array3, Axis};
use rayon::prelude::*;

use crate::RAW_BLACK;

const TLX_HEADER_LEN: usize = 16;
const TLX_BPP_RGB16: u32 = 48;

/// Header of a Pakon TLX planar 16-bit RGB `.raw` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TlxHeader {
    pub width: usize,
    pub height: usize,
    pub bpp: u32,
}

/// A roll assembled from one or more TLX planar `.raw` files.
pub struct TlxRoll {
    pub rgb: Array3<u16>,
    pub frame_w: usize,
    pub frame_count: usize,
    pub files: Vec<PathBuf>,
}

/// Check if `path` is a Pakon TLX planar 16-bit RGB `.raw` file.
pub fn probe(path: &Path) -> Option<TlxHeader> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() < TLX_HEADER_LEN as u64 {
        return None;
    }
    let mut f = File::open(path).ok()?;
    let mut hdr = [0u8; TLX_HEADER_LEN];
    f.read_exact(&mut hdr).ok()?;
    let hdr_len = u32::from_le_bytes(hdr[0..4].try_into().ok()?) as usize;
    let width = u32::from_le_bytes(hdr[4..8].try_into().ok()?) as usize;
    let height = u32::from_le_bytes(hdr[8..12].try_into().ok()?) as usize;
    let bpp = u32::from_le_bytes(hdr[12..16].try_into().ok()?);
    if hdr_len != TLX_HEADER_LEN || bpp != TLX_BPP_RGB16 || width == 0 || height == 0 {
        return None;
    }
    let expected = (TLX_HEADER_LEN as u64)
        .checked_add((width as u64).checked_mul(height as u64)?.checked_mul(6)?)?;
    if meta.len() != expected {
        return None;
    }
    Some(TlxHeader { width, height, bpp })
}

/// Natural numeric filename comparison (`2.raw` < `10.raw` < `base.raw`).
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0usize, 0usize);
    while i < ab.len() && j < bb.len() {
        if ab[i].is_ascii_digit() && bb[j].is_ascii_digit() {
            let (si, sj) = (i, j);
            while i < ab.len() && ab[i].is_ascii_digit() {
                i += 1;
            }
            while j < bb.len() && bb[j].is_ascii_digit() {
                j += 1;
            }
            let na: u64 = a[si..i].parse().unwrap_or(u64::MAX);
            let nb: u64 = b[sj..j].parse().unwrap_or(u64::MAX);
            match na.cmp(&nb) {
                Ordering::Equal => match (i - si).cmp(&(j - sj)) {
                    Ordering::Equal => continue,
                    o => return o,
                },
                o => return o,
            }
        }
        let ca = ab[i].to_ascii_lowercase();
        let cb = bb[j].to_ascii_lowercase();
        if ca != cb {
            return ca.cmp(&cb);
        }
        i += 1;
        j += 1;
    }
    ab.len().cmp(&bb.len())
}

/// Find all matching TLX `.raw` frames in `selected`'s folder, sorted naturally.
pub fn sibling_files(selected: &Path, expected: &TlxHeader) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = selected.parent() {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("raw"))
                    && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))
                    && probe(&p) == Some(*expected)
                {
                    out.push(p);
                }
            }
        }
    }
    if out.is_empty() {
        out.push(selected.to_path_buf());
    } else {
        out.sort_by(|a, b| {
            let na = a.file_name().map(|s| s.to_string_lossy()).unwrap_or_default();
            let nb = b.file_name().map(|s| s.to_string_lossy()).unwrap_or_default();
            natural_cmp(&na, &nb)
        });
    }
    out
}

fn load_frame_into(
    path: &Path,
    w: usize,
    h: usize,
    scale_14bit: bool,
    mut dst: ndarray::ArrayViewMut3<'_, u16>,
) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let expected = TLX_HEADER_LEN + w * h * 6;
    if bytes.len() != expected {
        return Err(format!("{}: expected {expected} bytes, got {}", path.display(), bytes.len()));
    }
    let body = &bytes[TLX_HEADER_LEN..];
    let plane_samples = w * h;
    let plane_bytes = plane_samples * 2;
    let black = RAW_BLACK as u16;
    for c in 0..3 {
        let plane = &body[c * plane_bytes..(c + 1) * plane_bytes];
        for y in 0..h {
            let col = h - 1 - y;
            let row_off = y * w * 2;
            for x in 0..w {
                let off = row_off + x * 2;
                let v = u16::from_le_bytes([plane[off], plane[off + 1]]);
                let s = if scale_14bit { v.saturating_mul(4).saturating_add(black) } else { v };
                dst[[x, col, c]] = s;
            }
        }
    }
    Ok(())
}

/// Detect whether the first TLX frame stores 14-bit (`0..16383`) or already-scaled
/// 16-bit (`0..65535`) linear transmittance values.
fn is_14bit_tlx(path: &Path) -> Result<bool, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() <= TLX_HEADER_LEN {
        return Ok(true);
    }
    let body = &bytes[TLX_HEADER_LEN..];
    let mut max_v = 0u16;
    for chunk in body.chunks_exact(2).step_by(16) {
        let v = u16::from_le_bytes([chunk[0], chunk[1]]);
        if v > max_v {
            max_v = v;
            if max_v > 16383 {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Build an `open-pakon` ribbon directly from a folder of TLX planar `.raw` files.
pub fn build_ribbon(selected: &Path, progress: &mut dyn FnMut(&str, f64)) -> Result<TlxRoll, String> {
    progress("Detecting TLX planar RAWs", 0.02);
    let hdr = probe(selected).ok_or_else(|| format!("{}: not a TLX planar 16-bit RAW", selected.display()))?;
    let files = sibling_files(selected, &hdr);
    let n = files.len();
    let (w, h) = (hdr.width, hdr.height);
    let scale_14bit = is_14bit_tlx(&files[0])?;

    progress(&format!("Loading {n} TLX frames ({w}×{h})"), 0.08);
    let mut rgb = Array3::<u16>::zeros((n * w, h, 3));
    let done = AtomicUsize::new(0);
    let err: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

    rgb.axis_chunks_iter_mut(Axis(0), w)
        .into_par_iter()
        .enumerate()
        .for_each(|(i, chunk)| {
            if err.lock().unwrap().is_some() {
                return;
            }
            if let Err(e) = load_frame_into(&files[i], w, h, scale_14bit, chunk) {
                let mut g = err.lock().unwrap();
                if g.is_none() {
                    *g = Some(e);
                }
            }
            done.fetch_add(1, AtomicOrdering::Relaxed);
        });

    if let Some(e) = err.into_inner().unwrap() {
        return Err(e);
    }
    progress(&format!("Loaded {n} TLX frames"), 0.58);
    Ok(TlxRoll { rgb, frame_w: w, frame_count: n, files })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tlx(path: &Path, w: usize, h: usize, val: impl Fn(usize, usize, usize) -> u16) {
        let mut buf = Vec::with_capacity(TLX_HEADER_LEN + w * h * 6);
        buf.extend_from_slice(&(TLX_HEADER_LEN as u32).to_le_bytes());
        buf.extend_from_slice(&(w as u32).to_le_bytes());
        buf.extend_from_slice(&(h as u32).to_le_bytes());
        buf.extend_from_slice(&TLX_BPP_RGB16.to_le_bytes());
        for c in 0..3 {
            for y in 0..h {
                for x in 0..w {
                    buf.extend_from_slice(&val(c, y, x).to_le_bytes());
                }
            }
        }
        std::fs::write(path, buf).unwrap();
    }

    #[test]
    fn natural_sort_orders_numeric_filenames() {
        let mut names = ["10.raw", "2.raw", "00.raw", "base.raw", "1.raw"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["00.raw", "1.raw", "2.raw", "10.raw", "base.raw"]);
    }

    #[test]
    fn probe_and_build_ribbon_roundtrip_orientation_and_scale() {
        let dir = std::env::temp_dir().join(format!("pakon_tlx_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let (w, h) = (6, 4);
        let f0 = dir.join("00.raw");
        let f1 = dir.join("01.raw");
        write_tlx(&f0, w, h, |c, y, x| (1000 + c * 100 + y * 10 + x) as u16);
        write_tlx(&f1, w, h, |c, y, x| (2000 + c * 100 + y * 10 + x) as u16);

        let hdr = probe(&f0).expect("probe should succeed");
        assert_eq!(hdr, TlxHeader { width: w, height: h, bpp: 48 });

        let roll = build_ribbon(&f1, &mut |_, _| {}).expect("build_ribbon");
        assert_eq!(roll.frame_count, 2);
        assert_eq!(roll.frame_w, w);
        assert_eq!(roll.rgb.dim(), (2 * w, h, 3));

        // Verify rot90(k=1) of frame 0 recovers upright (h, w, 3) with exact 14-bit -> raw16 scaling.
        let mut slice = roll.rgb.slice(ndarray::s![0..w, .., ..]);
        slice.swap_axes(0, 1);
        slice.invert_axis(Axis(0));
        assert_eq!(slice.dim(), (h, w, 3));
        for c in 0..3 {
            for y in 0..h {
                for x in 0..w {
                    let orig = (1000 + c * 100 + y * 10 + x) as u16;
                    assert_eq!(slice[[y, x, c]], orig * 4 + 300);
                }
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
