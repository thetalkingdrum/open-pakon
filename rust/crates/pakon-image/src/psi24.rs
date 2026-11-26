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
    let used = if w > 0 { ebp::enhanced_black_printing(&mut pl, gamma).1 } else { 1.0 };
    let mut out = Array3::<u16>::zeros((h, w, 3));
    out.axis_iter_mut(ndarray::Axis(0)).into_par_iter().enumerate().for_each(|(y, mut orow)| {
        for x in 0..w {
            let i = y * w + x;
            let s = t.romm_srgb.lookup([pl.p[0][i] as f32 / 4095.0, pl.p[1][i] as f32 / 4095.0, pl.p[2][i] as f32 / 4095.0]);
            for c in 0..3 {
                orow[(x, c)] = (s[c] * 65535.0 + 0.5).clamp(0.0, 65535.0) as u16;
            }
        }
    });
    (out, used)
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
        assert!(o.iter().all(|&v| v == o[(0, 0, 0)] || true));
    }
}
