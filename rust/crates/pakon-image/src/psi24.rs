//! The PSI 2.4 C-41 render after the scene balance: 12-bit RPD (SCP LUT +
//! shifts applied, see `colour::rpd_field`) -> `rpd.pf` to `romm.pf` at 12
//! bit -> EnhancedBlackPrinting (Gain and Offset, Gamma; `ebp`) -> Kodak
//! `srgb.pf`. The real DLL does exactly this; without the EBP stages the
//! renders have ~1.3x too much contrast and a shadow toe.

use std::sync::OnceLock;

use ndarray::Array3;
use rayon::prelude::*;

use crate::colour::Table;
use crate::ebp::{self, FPlanes, Gamma, Planes};

/// rpd_24.pf -> romm.pf, 65^3 u16 (`assets/make_psi24_tables.py`).
pub const RPD24_ROMM_U16: &[u8] = include_bytes!("../assets/rpd_24.romm65.u16");
/// romm.pf -> Kodak srgb.pf, 65^3 u16.
pub const ROMM_SRGB_U16: &[u8] = include_bytes!("../assets/romm.kodaksrgb65.u16");

pub struct Tables {
    pub rpd_romm: Table,
    pub romm_srgb: Table,
}

pub fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| Tables {
        rpd_romm: Table::from_u16_le(65, RPD24_ROMM_U16).expect("rpd_24.romm65.u16"),
        romm_srgb: Table::from_u16_le(65, ROMM_SRGB_U16).expect("romm.kodaksrgb65.u16"),
    })
}

/// Gamma exponent ceiling of the automatic estimator. At full resolution the
/// estimate is within ~0.03 of the best fit on the four reference frames
/// (0.96 / 0.70 / 0.70 / 0.73 against 0.93 / 0.68 / 0.65 / 0.72), but the edge
/// statistic it uses is resolution dependent (frame 1 estimates 1.19 on a 1/4
/// subsample), so a preview could overshoot: capped at 1.0.
pub const GAMMA_CAP: f64 = 1.0;

/// RPD field (rows of interleaved f32, 0..4095) -> 8-bit-equivalent 16-bit
/// sRGB (H, W, 3), like `colour::render_oem_scp`'s output.
pub fn render(rpd: &[Vec<f32>], gamma: Gamma) -> (Array3<u16>, f64) {
    let t = tables();
    let h = rpd.len();
    let w = if h > 0 { rpd[0].len() / 3 } else { 0 };
    let mut pl = Planes::new(w, h);
    {
        let [pr, pg, pb] = &mut pl.p;
        pr.par_chunks_mut(w.max(1)).zip(pg.par_chunks_mut(w.max(1))).zip(pb.par_chunks_mut(w.max(1))).enumerate().for_each(|(y, ((r, g), b))| {
            let row = &rpd[y];
            for x in 0..w {
                let s = t.rpd_romm.lookup([row[3 * x] / 4095.0, row[3 * x + 1] / 4095.0, row[3 * x + 2] / 4095.0]);
                r[x] = to12(s[0]);
                g[x] = to12(s[1]);
                b[x] = to12(s[2]);
            }
        });
    }
    // The DLL's EBP runs on 8-bit sRGB (see `ebp::set_plain8`): convert first.
    // `PSI24_ROMM12=1` keeps the old 12-bit ROMM path for comparisons.
    let e8 = std::env::var("PSI24_ROMM12").is_err();
    ebp::set_plain8(e8);
    let corr = std::env::var("PSI24_NO_PRECORR").is_err();
    if let (Ok(path), true) = (std::env::var("PSI24_DUMP_PREF"), e8) {
        // unrounded 8-bit-scale sRGB (before the pre-EBP correction), planar f32 LE
        let mut o = Vec::with_capacity(12 * w * h);
        for c in 0..3 {
            for i in 0..w * h {
                let s = t.romm_srgb.lookup([pl.p[0][i] as f32 / 4095.0, pl.p[1][i] as f32 / 4095.0, pl.p[2][i] as f32 / 4095.0]);
                o.extend_from_slice(&(s[c] * 255.0).to_le_bytes());
            }
        }
        std::fs::write(path, o).unwrap();
    }
    if e8 && w > 0 {
        let [pr, pg, pb] = &mut pl.p;
        pr.par_chunks_mut(w).zip(pg.par_chunks_mut(w)).zip(pb.par_chunks_mut(w)).for_each(|((r, g), b)| {
            for x in 0..w {
                let s = t.romm_srgb.lookup([r[x] as f32 / 4095.0, g[x] as f32 / 4095.0, b[x] as f32 / 4095.0]);
                let (r8, g8, b8) = (s[0] * 255.0, s[1] * 255.0, s[2] * 255.0);
                let k = if corr { 1.0 } else { 0.0 };
                r[x] = ((r8 + k * pre_corr(r8, &PRE_R, false) + 0.5).clamp(0.0, 255.0) as u16) << 4;
                g[x] = ((g8 + k * pre_corr(g8, &PRE_G, true) + 0.5).clamp(0.0, 255.0) as u16) << 4;
                b[x] = ((b8 + k * pre_corr(b8, &PRE_B, true) + 0.5).clamp(0.0, 255.0) as u16) << 4;
            }
        });
    }
    if let (Ok(path), true) = (std::env::var("PSI24_DUMP_PRE"), e8) {
        // planar 8-bit sRGB entering EBP (for comparing with the real stage dumps)
        let mut o = Vec::with_capacity(3 * w * h);
        for c in 0..3 {
            o.extend(pl.p[c].iter().map(|&v| (v >> 4) as u8));
        }
        std::fs::write(path, o).unwrap();
    }
    let used = if w > 0 { ebp::enhanced_black_printing(&mut pl, gamma).1 } else { 1.0 };
    let mut out = Array3::<u16>::zeros((h, w, 3));
    out.axis_iter_mut(ndarray::Axis(0)).into_par_iter().enumerate().for_each(|(y, mut orow)| {
        for x in 0..w {
            let i = y * w + x;
            if e8 {
                for c in 0..3 {
                    orow[(x, c)] = (pl.p[c][i] >> 4).min(255) * 257;
                }
                continue;
            }
            let s = t.romm_srgb.lookup([pl.p[0][i] as f32 / 4095.0, pl.p[1][i] as f32 / 4095.0, pl.p[2][i] as f32 / 4095.0]);
            for c in 0..3 {
                orow[(x, c)] = (s[c] * 65535.0 + 0.5).clamp(0.0, 65535.0) as u16;
            }
        }
    });
    (out, used)
}


/// High-bit-depth render: same pipeline and decisions as [`render`], but the 8-bit EBP stages run
/// continuously on float planes ([`ebp::hq_apply`]) and nothing is quantised between the ICC
/// tables (16-bit, trilinear in f64) and the output. Returns sRGB on a 0..255 float scale plus the
/// Gamma exponent. All parameter decisions come from the exact 8-bit chain (run on a clone).
pub fn render_hq_f32(rpd: &[Vec<f32>], gamma: Gamma) -> (Vec<Vec<f32>>, usize, usize, f64) {
    let t = tables();
    let h = rpd.len();
    let w = if h > 0 { rpd[0].len() / 3 } else { 0 };
    if w == 0 {
        return (vec![], 0, h, 1.0);
    }
    let corr = std::env::var("PSI24_NO_PRECORR").is_err();
    let mut f = FPlanes { w, h, p: std::array::from_fn(|_| vec![0f32; w * h]) };
    let mut q8 = Planes::new(w, h);
    {
        let [fr, fg, fb] = &mut f.p;
        let [qr, qg, qb] = &mut q8.p;
        fr.par_chunks_mut(w).zip(fg.par_chunks_mut(w)).zip(fb.par_chunks_mut(w)).zip(qr.par_chunks_mut(w).zip(qg.par_chunks_mut(w)).zip(qb.par_chunks_mut(w))).enumerate().for_each(|(y, (((fr, fg), fb), ((qr, qg), qb)))| {
            let row = &rpd[y];
            for x in 0..w {
                let rm = t.rpd_romm.lookup([row[3 * x] / 4095.0, row[3 * x + 1] / 4095.0, row[3 * x + 2] / 4095.0]);
                // exact statistics path: the 12-bit ROMM round trip, 8-bit rounding (like `render`)
                let s = t.romm_srgb.lookup([to12(rm[0]) as f32 / 4095.0, to12(rm[1]) as f32 / 4095.0, to12(rm[2]) as f32 / 4095.0]);
                let c8 = |v: f32, k: &[f32; 16], fade: bool| v * 255.0 + if corr { pre_corr(v * 255.0, k, fade) } else { 0.0 };
                let (r8, g8, b8) = (c8(s[0], &PRE_R, false), c8(s[1], &PRE_G, true), c8(s[2], &PRE_B, true));
                qr[x] = ((r8 + 0.5).clamp(0.0, 255.0) as u16) << 4;
                qg[x] = ((g8 + 0.5).clamp(0.0, 255.0) as u16) << 4;
                qb[x] = ((b8 + 0.5).clamp(0.0, 255.0) as u16) << 4;
                // HQ path: no 12-bit round trip, continuous correction, no rounding
                let s = t.romm_srgb.lookup(rm);
                fr[x] = c8(s[0], &PRE_R, false).clamp(0.0, 255.0);
                fg[x] = c8(s[1], &PRE_G, true).clamp(0.0, 255.0);
                fb[x] = c8(s[2], &PRE_B, true).clamp(0.0, 255.0);
            }
        });
    }
    ebp::set_plain8(true);
    let params = ebp::hq_params(&mut q8, gamma);
    drop(q8);
    ebp::hq_apply(&mut f, &params);
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

/// Measured difference (real - ours) of the 8-bit sRGB entering EBP, at knots 0, 16, .. 240 per
/// channel. Fitted by quantile (histogram) matching of our pre-EBP channel against the real `s0`
/// dumps, pooled over all four reference frames (the Gain & Offset stage is histogram driven, and
/// matching the histograms beats a least-squares / per-pixel fit end to end; knot 0 is kept from the
/// previous fit, too few pixels). The per-frame curves agree to ~0.5 level. Our `rpd.pf -> romm.pf ->
/// srgb.pf` tables are about 2-4 levels low in the highlights and 1-3 high in the shadows against the
/// DLL; skipping the 12-bit ROMM round trip changes the output by only 0.02-0.04 levels, so it is not
/// the cause (see PSI24_RE_NOTES section 5).
const PRE_R: [f32; 16] = [2.12, 2.24, 1.71, 0.90, 1.29, 2.11, 1.37, 2.90, 2.07, 3.12, 3.60, 3.61, 3.43, 4.16, 4.08, 4.19];
const PRE_G: [f32; 16] = [-0.39, -0.39, -1.18, -1.01, -0.64, 0.54, -0.04, 0.99, 1.04, 1.22, 1.98, 2.20, 2.19, 2.82, 2.77, 2.47];
const PRE_B: [f32; 16] = [-1.59, -1.64, -2.00, -1.35, -1.28, -0.29, -0.84, 0.47, 0.40, 0.51, 1.58, 1.56, 1.50, 2.12, 1.99, 1.83];

fn pre_corr(v: f32, k: &[f32; 16], fade: bool) -> f32 {
    let x = (v / 16.0).clamp(0.0, 15.0);
    let i = (x as usize).min(14);
    let f = x - i as f32;
    // G and B fade out over the last 7 levels: the DLL leaves them just under 255 where the red
    // is already clipped, and pushing them into the clipped bin flips the white-point branch of
    // Gain & Offset (probe P3b_c1800_s1000).
    (k[i] * (1.0 - f) + k[i + 1] * f) * if fade { ((255.0 - v) / 7.0).clamp(0.0, 1.0) } else { 1.0 }
}

#[inline]
fn to12(v: f32) -> u16 {
    (v * 4095.0 + 0.5).clamp(0.0, 4095.0) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_load_and_neutral_ramp_is_monotonic() {
        let t = tables();
        let mut last = [-1.0f32; 3];
        for i in 0..=40 {
            let x = 0.2 + 0.6 * i as f32 / 40.0;
            let s = t.romm_srgb.lookup(t.rpd_romm.lookup([x; 3]));
            for c in 0..3 {
                assert!(s[c] >= last[c] - 1e-4, "ch {c} not monotonic at {x}");
                last[c] = s[c];
            }
        }
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
}
