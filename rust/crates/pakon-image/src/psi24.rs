//! The PSI 2.4 C-41 render after the scene balance: 12-bit RPD (SCP LUT + shifts applied, see
//! `colour::rpd_field`) -> the Kodak CMM (`rpd.pf` combined with `srgb.pf`, sampled on a 65^3 grid) -> 8-bit
//! sRGB -> EnhancedBlackPrinting (Gain and Offset, Gamma, Noise Filter, Black Print; see `ebp`). The real
//! DLL does exactly this; without the EBP stages the renders have ~1.3x too much contrast and a shadow toe.

use std::sync::OnceLock;

use ndarray::Array3;
use rayon::prelude::*;

use crate::colour::Table;
use crate::ebp::{self, FPlanes, Gamma, HqOptions, Planes};

/// The real Kodak CMM, `SpCombineXforms(rpd.pf, srgb.pf)` sampled with `SpEvaluate` (output type 5,
/// u16 full range) on a 65^3 grid of 12-bit inputs: cmm_in (SCP LUT + shifts) -> sRGB, R-major
/// (`assets/make_psi24_tables.py`, Windows `psi24-cmm`). Used for the float planes of the HQ path.
pub const CMM65_U16: &[u8] = include_bytes!("../assets/cmm65.u16");

/// The same grid with `SpEvaluate` output type 1 (u8, what the DLL's render uses). Type 5 lifts the
/// shadows by +0.5..+0.9 u8 levels against it, so the exact 8-bit path and the 8-bit decisions of the
/// HQ path use this one.
pub const CMM65_U8: &[u8] = include_bytes!("../assets/cmm65_u8.u8");

pub struct Tables {
    /// type 5 (u16), float output
    pub cmm: Table,
    /// type 1 (u8), the DLL's 8-bit output
    pub cmm8: Table,
}

pub fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| Tables {
        cmm: Table::from_u16_le(65, CMM65_U16).expect("cmm65.u16"),
        cmm8: Table::from_u8(65, CMM65_U8).expect("cmm65_u8.u8"),
    })
}

/// Tetrahedral lookup of a 12-bit RPD triple (`x / 4095` is the table coordinate; warping the input to
/// the exact node positions `round(i*4095/64)` was tried and was worse on both held-out rolls).
#[inline]
fn cmm_at(tab: &Table, row: &[f32], x: usize) -> [f32; 3] {
    tab.lookup_tetra([row[3 * x] / 4095.0, row[3 * x + 1] / 4095.0, row[3 * x + 2] / 4095.0])
}

/// Gamma exponent ceiling passed to the automatic estimator (`Gamma::Auto { cap }`) by the roll
/// renderer and the reference test: the estimate is clamped to at most 1.0. The edge statistic the
/// estimator uses is resolution dependent (frame 1 estimates 1.19 on a 1/4 subsample), so on
/// downscaled input it can overshoot; at full resolution it is within ~0.03 of the best fit on the
/// reference frames. Tests that exercise the estimator itself pass 9.0 (effectively uncapped).
pub const GAMMA_CAP: f64 = 1.0;

/// `v` 0..1 -> the 8-bit level on the 12-bit plane scale (`level << 4`), rounded like the DLL's u8 output.
#[inline]
fn q8(v: f32) -> u16 {
    ((v * 255.0 + 0.5).clamp(0.0, 255.0) as u16) << 4
}

/// The 8-bit sRGB image entering EBP (planar, `level << 4`): RPD field (rows of interleaved f32, 0..4095)
/// through the sampled Kodak CMM, rounded to 8 bit. `psi24_stage`/analysis code compares this with the
/// real `s0` dumps.
pub fn pre_ebp(rpd: &[Vec<f32>]) -> Planes {
    pre_ebp_planes(rpd, false).0
}

/// [`pre_ebp`] plus, when `float` is set, the same image from the type-5 table without rounding
/// (0..255 scale), for the HQ path.
fn pre_ebp_planes(rpd: &[Vec<f32>], float: bool) -> (Planes, Option<FPlanes>) {
    let t = tables();
    let h = rpd.len();
    let w = if h > 0 { rpd[0].len() / 3 } else { 0 };
    let mut q = Planes::new(w, h);
    let mut f = float.then(|| FPlanes { w, h, p: std::array::from_fn(|_| vec![0f32; w * h]) });
    if w == 0 {
        return (q, f);
    }
    let [qr, qg, qb] = &mut q.p;
    let rows = qr.par_chunks_mut(w).zip(qg.par_chunks_mut(w)).zip(qb.par_chunks_mut(w));
    match f.as_mut() {
        None => rows.enumerate().for_each(|(y, ((qr, qg), qb))| {
            for x in 0..w {
                let s8 = cmm_at(&t.cmm8, &rpd[y], x);
                (qr[x], qg[x], qb[x]) = (q8(s8[0]), q8(s8[1]), q8(s8[2]));
            }
        }),
        Some(f) => {
            let [fr, fg, fb] = &mut f.p;
            rows.zip(fr.par_chunks_mut(w).zip(fg.par_chunks_mut(w)).zip(fb.par_chunks_mut(w))).enumerate().for_each(|(y, (((qr, qg), qb), ((fr, fg), fb)))| {
                for x in 0..w {
                    let s = cmm_at(&t.cmm, &rpd[y], x);
                    let s8 = cmm_at(&t.cmm8, &rpd[y], x);
                    (qr[x], qg[x], qb[x]) = (q8(s8[0]), q8(s8[1]), q8(s8[2]));
                    (fr[x], fg[x], fb[x]) = ((s[0] * 255.0).clamp(0.0, 255.0), (s[1] * 255.0).clamp(0.0, 255.0), (s[2] * 255.0).clamp(0.0, 255.0));
                }
            });
        }
    }
    (q, f)
}

/// RPD field (rows of interleaved f32, 0..4095) -> 8-bit-equivalent 16-bit sRGB (H, W, 3), like
/// `colour::render_oem_scp`'s output. Returns the Gamma exponent used.
pub fn render(rpd: &[Vec<f32>], gamma: Gamma) -> (Array3<u16>, f64) {
    let h = rpd.len();
    let w = if h > 0 { rpd[0].len() / 3 } else { 0 };
    let mut pl = pre_ebp(rpd);
    let used = if w > 0 { ebp::enhanced_black_printing(&mut pl, gamma).1 } else { 1.0 };
    let mut out = Array3::<u16>::zeros((h, w, 3));
    out.axis_iter_mut(ndarray::Axis(0)).into_par_iter().enumerate().for_each(|(y, mut orow)| {
        for x in 0..w {
            for c in 0..3 {
                orow[(x, c)] = (pl.p[c][y * w + x] >> 4).min(255) * 257;
            }
        }
    });
    (out, used)
}

/// High-bit-depth render: same pipeline and decisions as [`render`], but the 8-bit EBP stages run
/// continuously on float planes ([`ebp::hq_apply`]) and nothing is quantised between the CMM and the
/// output. Returns sRGB on a 0..255 float scale plus the Gamma exponent. All parameter decisions come
/// from the exact 8-bit chain (run on the rounded planes).
pub fn render_hq_f32(rpd: &[Vec<f32>], gamma: Gamma) -> (Vec<Vec<f32>>, usize, usize, f64) {
    render_hq_f32_with(rpd, gamma, HqOptions::default())
}

pub fn render_hq_f32_with(rpd: &[Vec<f32>], gamma: Gamma, opts: HqOptions) -> (Vec<Vec<f32>>, usize, usize, f64) {
    let h = rpd.len();
    let w = if h > 0 { rpd[0].len() / 3 } else { 0 };
    if w == 0 {
        return (vec![], 0, h, 1.0);
    }
    let (mut q, f) = pre_ebp_planes(rpd, true);
    let mut f = f.unwrap();
    let params = ebp::hq_params(&mut q, gamma);
    drop(q);
    ebp::hq_apply(&mut f, &params, opts);
    let rows: Vec<Vec<f32>> = (0..h)
        .map(|y| {
            let mut row = Vec::with_capacity(3 * w);
            for x in 0..w {
                for c in 0..3 {
                    row.push(f.p[c][y * w + x]);
                }
            }
            row
        })
        .collect();
    (rows, w, h, params.gamma)
}

/// 16-bit output of [`render_hq_f32`] (`v * 257`, so 8-bit levels map to the same codes as [`render`]).
pub fn render_hq(rpd: &[Vec<f32>], gamma: Gamma) -> (Array3<u16>, f64) {
    let (rows, w, h, g) = render_hq_f32(rpd, gamma);
    let mut out = Array3::<u16>::zeros((h, w, 3));
    out.axis_iter_mut(ndarray::Axis(0)).into_par_iter().enumerate().for_each(|(y, mut orow)| {
        for x in 0..w {
            for c in 0..3 {
                orow[(x, c)] = (rows[y][3 * x + c] * 257.0 + 0.5).clamp(0.0, 65535.0) as u16;
            }
        }
    });
    (out, g)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmm_tables_load_and_agree_on_the_neutral_ramp() {
        let t = tables();
        for i in 0..=60 {
            let x = i as f32 / 60.0 * 3000.0 / 4095.0;
            let (a, b) = (t.cmm8.lookup([x; 3]), t.cmm.lookup([x; 3]));
            for c in 0..3 {
                // type 5 is the same transform at higher precision: within ~3 u8 levels (measured -0.9..+2.5) of the u8 output
                assert!((a[c] - b[c]).abs() * 255.0 < 3.0, "ch {c} u8 {} vs t5 {} at {x}", a[c], b[c]);
            }
        }
        assert!(t.cmm8.lookup([0.0; 3]).iter().all(|v| *v * 255.0 < 3.0));
    }

    #[test]
    fn hq_stays_close_to_the_8bit_path_and_has_no_banding() {
        // smooth gradient with some texture, wide enough to give the stages real work
        let (w, h) = (600usize, 300usize);
        let rpd: Vec<Vec<f32>> = (0..h)
            .map(|y| {
                (0..3 * w)
                    .map(|i| {
                        let (x, c) = (i / 3, i % 3);
                        let t = x as f32 / (w - 1) as f32;
                        let tex = (((x * 7 + y * 13 + c * 5) % 17) as f32 - 8.0) * 3.0;
                        (700.0 + (2900.0 - c as f32 * 150.0) * t + tex).clamp(0.0, 4095.0)
                    })
                    .collect()
            })
            .collect();
        let (a, ga) = render(&rpd, Gamma::Auto { cap: GAMMA_CAP });
        let (b, gb) = render_hq(&rpd, Gamma::Auto { cap: GAMMA_CAP });
        assert!((ga - gb).abs() < 1e-9);
        let (mut mae, mut bias) = (0f64, 0f64);
        for (p, q) in a.iter().zip(b.iter()) {
            let d = (*q as f64 - *p as f64) / 257.0;
            mae += d.abs();
            bias += d;
        }
        let n = a.len() as f64;
        assert!(mae / n < 1.6, "MAE {}", mae / n);
        assert!((bias / n).abs() < 1.2, "bias {}", bias / n);
        // a smooth row must have many more distinct 16-bit values than the 256 of the 8-bit path
        let mut d8: Vec<u16> = (0..w).map(|x| a[(150, x, 1)]).collect();
        let mut d16: Vec<u16> = (0..w).map(|x| b[(150, x, 1)]).collect();
        d8.sort();
        d8.dedup();
        d16.sort();
        d16.dedup();
        assert!(d16.len() > d8.len() + d8.len() / 2, "hq {} vs 8-bit {}", d16.len(), d8.len());
    }

    #[test]
    fn render_of_a_flat_field_is_flat_and_sized() {
        let rpd: Vec<Vec<f32>> = (0..30).map(|_| vec![1500.0f32; 3 * 40]).collect();
        let (o, g) = render(&rpd, Gamma::Fixed(1.0));
        assert_eq!(o.dim(), (30, 40, 3));
        assert!(g == 1.0);
        for c in 0..3 {
            assert!(o.indexed_iter().all(|((_, _, k), &v)| k != c || v == o[(0, 0, c)]), "channel {c} not flat");
        }
    }

    /// Deterministic synthetic RPD fields (fixed xorshift) that between them reach every branch of the stages:
    /// a narrow dark noisy field (knee, Gamma g2, noisy Noise Filter, Black Print), a bright high-contrast one
    /// with sharp edges (Gamma g1 edge statistic) and a smooth full-range one (stage 1 only, flat Noise Filter).
    fn synth(kind: usize) -> Vec<Vec<f32>> {
        let (w, h) = (128usize, 96usize);
        let mut s = 0x9E3779B9u32 ^ (kind as u32 * 0x85EBCA6B);
        let mut rnd = move || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s >> 8) as f32 / (1u32 << 24) as f32
        };
        (0..h)
            .map(|y| {
                (0..3 * w)
                    .map(|i| {
                        let (x, c) = (i / 3, i % 3);
                        let v = match kind {
                            0 => 500.0 + 700.0 * (x as f32 / w as f32) + (rnd() - 0.5) * 160.0 + c as f32 * 40.0,
                            1 => {
                                let blk = ((x / 16 + y / 16) % 2) as f32;
                                1500.0 + blk * 1400.0 + (rnd() - 0.5) * 40.0 - c as f32 * 60.0
                            }
                            _ => 300.0 + 2700.0 * ((x + y) as f32 / (w + h) as f32) + c as f32 * 30.0,
                        };
                        v.clamp(0.0, 4095.0)
                    })
                    .collect()
            })
            .collect()
    }

    fn fnv(v: impl Iterator<Item = u16>) -> u64 {
        v.fold(0xcbf29ce484222325u64, |h, x| (h ^ x as u64).wrapping_mul(0x100000001b3))
    }

    /// Guards refactors: the exact and HQ outputs of the synthetic fields, bit for bit. A deliberate change of
    /// the maths has to update these values (and say why).
    #[test]
    fn golden_hashes_of_synthetic_fields() {
        const GOLDEN: [(u64, u64); 3] = [
            (0x49607797a565f9f4, 0xb1a86b2b09f619e7),
            (0x5c698b634dc4ec67, 0xbe47d2cf5b41460e),
            (0x5065b5d7e62b7422, 0xf213892c5e21de13),
        ];
        let mut got = vec![];
        for kind in 0..3 {
            let rpd = synth(kind);
            let (e, ge) = render(&rpd, Gamma::Auto { cap: 9.0 });
            let (q, gq) = render_hq(&rpd, Gamma::Auto { cap: 9.0 });
            assert_eq!(ge, gq);
            got.push((fnv(e.iter().copied()), fnv(q.iter().copied())));
            eprintln!("kind {kind}: gamma {ge:.4} exact {:016x} hq {:016x}", got[kind].0, got[kind].1);
        }
        assert_eq!(got, GOLDEN);
    }

    /// The synthetic fields keep reaching the branches they were made for.
    #[test]
    fn synthetic_fields_cover_the_stage_branches() {
        let mut pl = pre_ebp(&synth(0));
        assert!(ebp::gain_and_offset(&mut pl).knee.is_some(), "narrow field: knee");
        let mut pl = pre_ebp(&synth(2));
        assert!(ebp::gain_and_offset(&mut pl).knee.is_none(), "full-range field: stage 1 only");
        assert!(ebp::estimate_gamma(&pl).gamma > 1.0, "bright field: g1");
        let mut pl = pre_ebp(&synth(0));
        ebp::gain_and_offset(&mut pl);
        assert!(ebp::estimate_gamma(&pl).gamma < 1.0, "dark field: g2");
    }
}
