//! The ribbon: the whole roll as one registered, flat-fielded (rows, width, 3)
//! 16-bit image (port of `web/decode.py` `_build_ribbon`, visible part).

use std::path::{Path, PathBuf};

use memmap2::Mmap;
use ndarray::{Array2, Array3, ArrayView2};

use crate::decode::{self, Layout, Leads};
use crate::flat::{self, FlatJson};
use crate::ice;

/// A raw scan file (`.raw` or `PKLB` `.pakscan` container), memory-mapped as
/// little-endian u16 samples.
pub struct Raw {
    map: Mmap,
    range: std::ops::Range<usize>,
}

const PKLB_MAGIC: &[u8; 4] = b"PKLB";
const PKLB_HEADER_LEN: usize = 0x54;

fn pklb_raw_range(bytes: &[u8]) -> std::io::Result<std::ops::Range<usize>> {
    let invalid = |msg: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, msg);
    if bytes.len() < PKLB_HEADER_LEN {
        return Err(invalid("truncated PKLB .pakscan header"));
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    if version != 1 {
        return Err(invalid(&format!("unsupported PKLB .pakscan version {version}")));
    }
    let offset = u64::from_le_bytes(bytes[0x3c..0x44].try_into().unwrap()) as usize;
    let len = u64::from_le_bytes(bytes[0x44..0x4c].try_into().unwrap()) as usize;
    let end = offset.checked_add(len).ok_or_else(|| invalid("PKLB raw range overflow"))?;
    if offset % 2 != 0 || len < 2 || end > bytes.len() {
        return Err(invalid("invalid PKLB raw payload range"));
    }
    Ok(offset..end)
}

impl Raw {
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Raw> {
        let path = path.as_ref();
        let f = std::fs::File::open(path)?;
        // SAFETY: the raw is only read; a file truncated underneath us would
        // fault, as numpy's memmap would.
        let map = unsafe { Mmap::map(&f)? };
        let range = if map.starts_with(PKLB_MAGIC) {
            pklb_raw_range(&map)?
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pakscan")) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unsupported .pakscan file (expected PKLB raw container)",
            ));
        } else {
            0..map.len()
        };
        Ok(Raw { map, range })
    }

    /// The samples. Raws are written by our own tools on little-endian hosts;
    /// the map is page-aligned and PKLB raw offsets are even, so the cast is
    /// aligned.
    pub fn samples(&self) -> &[u16] {
        let (pre, mid, _) = unsafe { self.map[self.range.clone()].align_to::<u16>() };
        assert!(pre.is_empty(), "raw slice is 2-byte aligned");
        const { assert!(cfg!(target_endian = "little"), "raws are little-endian") };
        mid
    }
}

/// What the ribbon was built with, for logs and metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct RibbonInfo {
    pub layout: Layout,
    pub marker: Option<usize>,
    pub leads: Leads,
    pub leads_measured: bool,
    pub flat: Option<PathBuf>,
}

pub struct Ribbon {
    pub rgb: Array3<u16>,
    /// The IR plane, row for row with `rgb`, flat-fielded, for dust removal;
    /// only for raws with an IR lane and a flat field with an IR table.
    pub ir: Option<Array2<u16>>,
    /// The flat field used (its IR table feeds `ice::ir_noise_factor`).
    pub flat: Option<FlatJson>,
    pub info: RibbonInfo,
}

/// Options matching the web app's environment overrides.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// PAKON_LINEWIDTH (+ PAKON_IR_LANE): force the row layout.
    pub layout: Option<Layout>,
    /// The EEPROM archive (for the unit's flatfield*.json).
    pub eeprom_dir: Option<PathBuf>,
    /// PAKON_FLAT=0.
    pub no_flat: bool,
}

/// Marker-align, deinterleave, register and flat-field a raw. `progress`
/// gets (step, fraction) as `_build_ribbon` emits them.
pub fn build(raw_path: &Path, opt: &Options, progress: &mut dyn FnMut(&str, f64)) -> Result<Ribbon, String> {
    progress("Loading", 0.0);
    let raw = Raw::open(raw_path).map_err(|e| format!("{}: {e}", raw_path.display()))?;
    let samples = raw.samples();

    progress("Detecting row layout", 0.05);
    let layout = opt
        .layout
        .or_else(|| flat::flat_row_layout(raw_path).map(|(row_samples, ir_lane)| Layout { row_samples, ir_lane }))
        .or_else(|| decode::detect_row_layout(samples))
        .ok_or("could not detect the row layout of this raw (no known stride matched); set PAKON_LINEWIDTH (+ PAKON_IR_LANE=0/1) to force")?;
    let width = layout.width();
    progress(
        &format!("Layout: {} samples/row, {width} px ({})", layout.row_samples, if layout.ir_lane { "RGB+IR" } else { "RGB" }),
        0.08,
    );

    let lw = layout.row_samples;
    let lines = samples.len() / lw;
    let img = ArrayView2::from_shape((lines, lw), &samples[..lines * lw]).map_err(|e| e.to_string())?;

    progress("Aligning rows", 0.10);
    let marker = decode::marker_column(img, 0.6, 3000);
    let m = marker.unwrap_or(0);

    progress("Registering channels", 0.30);
    let (leads, leads_measured) = decode::validated_leads(decode::measure_leads(img, width, m), width);
    let mut rgb = decode::register(img, width, m, leads);

    let flat_path = if opt.no_flat { None } else { flat::find_flat_field(raw_path, opt.eeprom_dir.as_deref(), Some(width)) };
    let mut applied = None;
    let mut flat_json = None;
    if let Some(p) = &flat_path {
        match FlatJson::read(p).and_then(|f| flat::load_flat_field(&f, width).map(|ff| (f, ff))) {
            Ok((f, ff)) => {
                flat::apply_flat_field(&mut rgb, &ff);
                progress(&format!("Flat field: {}", p.file_name().unwrap_or_default().to_string_lossy()), 0.55);
                applied = Some(p.clone());
                flat_json = Some(f);
            }
            Err(e) => eprintln!("flat field not applied: {e}"),
        }
    }

    // IR plane for dust removal: registration keeps red as the row
    // reference, so ribbon row y is raw row y.
    let mut ir = None;
    if let (true, Some(f)) = (layout.ir_lane, &flat_json) {
        match ice::ir_plane(img, m, f, rgb.len_of(ndarray::Axis(0)), 0.9) {
            Ok(p) => ir = Some(ice::ir_to_u16(&p)),
            Err(e) => eprintln!("no IR plane for dust removal: {e}"),
        }
    }
    Ok(Ribbon { rgb, ir, flat: flat_json, info: RibbonInfo { layout, marker, leads, leads_measured, flat: applied } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_open_reads_pklb_pakscan_payload() {
        let dir = std::env::temp_dir().join(format!("pakon-pklb-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("roll.pakscan");

        let offset: u64 = 0x80;
        let samples: [u16; 6] = [100, 200, 300, 400, 500, 600];
        let payload_len = (samples.len() * 2) as u64;
        let mut buf = vec![0u8; offset as usize + payload_len as usize + 16];
        buf[0..4].copy_from_slice(b"PKLB");
        buf[4..8].copy_from_slice(&1u32.to_le_bytes());
        buf[0x3c..0x44].copy_from_slice(&offset.to_le_bytes());
        buf[0x44..0x4c].copy_from_slice(&payload_len.to_le_bytes());
        for (i, &s) in samples.iter().enumerate() {
            let at = offset as usize + i * 2;
            buf[at..at + 2].copy_from_slice(&s.to_le_bytes());
        }
        std::fs::write(&p, &buf).unwrap();

        let raw = Raw::open(&p).unwrap();
        assert_eq!(raw.samples(), &samples);

        let bad = dir.join("not_pklb.pakscan");
        std::fs::write(&bad, b"NOT_PKLB_DATA_HERE").unwrap();
        assert!(Raw::open(&bad).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
