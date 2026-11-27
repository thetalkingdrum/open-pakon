//! The PSI 2.4 C-41 render after the scene balance: 12-bit RPD (SCP LUT +
//! shifts applied, see `colour::rpd_field`) -> `rpd.pf` to `romm.pf` at 12
//! bit -> EnhancedBlackPrinting (Gain and Offset, Gamma; `ebp`) -> Kodak
//! `srgb.pf`. The real DLL does exactly this; without the EBP stages the
//! renders have ~1.3x too much contrast and a shadow toe.

use std::sync::OnceLock;

use ndarray::Array3;
use rayon::prelude::*;

use crate::colour::Table;
use crate::ebp::{self, Gamma, Planes};

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
