//! PSI 2.4 EnhancedBlackPrinting (EBP), the stage the real C-41 path runs on
//! 12-bit ROMM RGB between the RPD profile and the sRGB conversion: Gain and
//! Offset (per-channel percentile auto-levels plus a luma knee), then Gamma
//! (image-adaptive luma gamma). Decoded from `PakonIMAu.dll` 2.4
//! (`ImaEBPGainAndOffsetOp` `FUN_10183e40`, `ImaEBPGammaOp` `FUN_10180dc0`),
//! constants are the DLL's Romm12 defaults. Black Print and the Noise Filter
//! are not ported. The gamma estimator is partly reconstructed (see
//! [`Gamma::estimate`]).

use rayon::prelude::*;

const MAXV: usize = 4095;

// The DLL's C-41 EBP runs on 8-bit sRGB data (the `E` template instances: 256-bin
// histograms, 256-entry LUTs) with the *plain* attribute defaults (probe dump,
// `PROBE_FINDINGS.md`). We emulate it on the 12-bit machinery by feeding `v8 << 4`
// and switching the constants with `set_plain8(true)`.
thread_local! { static PLAIN8: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
/// Per calling thread (the stages read it before they fan out to rayon).
pub fn set_plain8(on: bool) {
    PLAIN8.with(|c| c.set(on));
}
fn plain8() -> bool {
    PLAIN8.with(|c| c.get())
}
fn black_pct() -> f64 {
    if plain8() { 0.10 } else { BLACK_PCT }
}
fn white_pct() -> f64 {
    if plain8() { 0.15 } else { WHITE_PCT }
}
fn mean_lo_pct() -> f64 {
    if plain8() { 39.06 } else { MEAN_LO_PCT }
}
fn mean_hi_pct() -> f64 {
    if plain8() { 48.82 } else { MEAN_HI_PCT }
}
fn citras() -> f64 {
    if plain8() { 0.5 } else { CITRAS }
}

/// A 12-bit three-plane image.
#[derive(Debug, Clone)]
pub struct Planes {
    pub w: usize,
    pub h: usize,
    pub p: [Vec<u16>; 3],
}

impl Planes {
    pub fn new(w: usize, h: usize) -> Planes {
        Planes { w, h, p: std::array::from_fn(|_| vec![0u16; w * h]) }
    }
}

/// `[1 4 6 4 1]` (x) then (y), edges replicated, unnormalised (sum of
/// weights 256).
fn binom5(src: &[i32], w: usize, h: usize) -> Vec<i32> {
    const K: [i32; 5] = [1, 4, 6, 4, 1];
    let mut tmp = vec![0i32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let row = &src[y * w..(y + 1) * w];
        for x in 0..w {
            let mut s = 0;
            for (k, wt) in K.iter().enumerate() {
                s += wt * row[(x + k).saturating_sub(2).min(w - 1)];
            }
            out[x] = s;
        }
    });
    let mut out = vec![0i32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, o)| {
        for (k, wt) in K.iter().enumerate() {
            let r = &tmp[(y + k).saturating_sub(2).min(h - 1) * w..][..w];
            for x in 0..w {
                o[x] += wt * r[x];
            }
        }
    });
    out
}

/// `[1 2 1]` (x) then (y), edges replicated, unnormalised (sum 16).
fn binom3(src: &[i32], w: usize, h: usize) -> Vec<i32> {
    let mut tmp = vec![0i32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let row = &src[y * w..(y + 1) * w];
        for x in 0..w {
            out[x] = row[x.saturating_sub(1)] + 2 * row[x] + row[(x + 1).min(w - 1)];
        }
    });
    let mut out = vec![0i32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, o)| {
        let (a, b, c) = (&tmp[y.saturating_sub(1) * w..][..w], &tmp[y * w..][..w], &tmp[(y + 1).min(h - 1) * w..][..w]);
        for x in 0..w {
            o[x] = a[x] + 2 * b[x] + c[x];
        }
    });
    out
}

/// Result of [`gain_and_offset`].
#[derive(Debug, Clone, Copy)]
pub struct GainInfo {
    pub lo: [i32; 3],
    pub hi: [i32; 3],
    pub lo2: i32,
    pub hi2: i32,
    pub k1: f64,
    /// `k1 * 3*255/sum(ranges)`: what the Gamma stage reads from this one.
    pub p: f64,
}

// Romm12 defaults of ImaEBPGainAndOffsetOp.
const BLACK_PCT: f64 = 0.08;
const WHITE_PCT: f64 = 0.10;
const SH_PCT: f64 = 0.06;
const SHADOW_THR: i32 = 5;
const HIGHLIGHT_THR: i32 = 250;

/// Gain and Offset: per-channel LUT from 0.08 % / 0.10 % percentiles of the
/// blurred 8-bit histogram (stretch capped near 2x for the narrowest
/// channel), plus the luma shadow/highlight knee when the histogram needs it.
pub fn gain_and_offset(img: &mut Planes) -> GainInfo {
    let (w, h) = (img.w, img.h);
    let n = w * h;
    let mut hist = [[0u64; 256]; 3];
    for c in 0..3 {
        let x8: Vec<i32> = img.p[c].iter().map(|&v| (v >> 4) as i32).collect();
        for s in binom5(&x8, w, h) {
            hist[c][(s >> 8).clamp(0, 255) as usize] += 1;
        }
    }
    let nb = (n as f64 * black_pct() * 0.01 + 0.5) as u64;
    let nw = (n as f64 * white_pct() * 0.01 + 0.5) as u64;
    let mut lo = [0i32; 3];
    let mut hi = [0i32; 3];
    let mut valid = [false; 3];
    for c in 0..3 {
        lo[c] = if nb > 0 {
            let mut acc = 0;
            (0..256).find(|&i| { acc += hist[c][i]; acc >= nb }).unwrap_or(256) as i32
        } else {
            0
        };
        hi[c] = if nw > 0 {
            let mut acc = 0;
            255 - (0..256).find(|&k| { acc += hist[c][255 - k]; acc >= nw }).unwrap_or(256) as i32
        } else {
            255
        };
        valid[c] = hi[c] > lo[c];
    }
    let avg_hi = hi.iter().sum::<i32>() / 3;
    let mut bvar2 = false;
    if hi[0] <= avg_hi {
        for c in 0..3 {
            if valid[c] {
                hi[c] = (hi[c] + avg_hi) / 2;
            }
        }
    } else {
        bvar2 = true;
        let h0 = hi[0];
        for c in 0..3 {
            if valid[c] {
                hi[c] = ((hi[c] + h0) / 2 + 255) / 2;
            }
        }
    }
    let t = ((lo[0] + lo[2]) >> 1) & 0xff;
    if t < lo[1] && valid[1] {
        lo[1] = t;
    }
    // A channel whose percentile range is under 4 levels counts as empty (range 0, which
    // makes the whole stage the identity). Probe data: P3a_2000_3000, P3b s200 vs s500.
    let r: [i32; 3] = std::array::from_fn(|c| if hi[c] - lo[c] < 4 { 0 } else { hi[c] - lo[c] });
    let tot: i32 = r.iter().sum();
    let gain_avg = if tot > 0 { 3.0 * 255.0 / tot as f64 } else { 1.0 };
    let minr = *r.iter().min().unwrap();
    let r2: [i32; 3] = if minr < 128 {
        if minr <= 0 { [0; 3] } else { std::array::from_fn(|c| ((r[c] * 128 + (minr >> 1)) / minr).min(255)) }
    } else {
        r
    };
    let s: [f64; 3] = std::array::from_fn(|c| if r2[c] > 0 { 255.0 / r2[c] as f64 } else { 1.0 });
    let off: [i32; 3] = std::array::from_fn(|c| if r2[c] > 0 { lo[c] } else { 0 });
    let lut8: [Vec<i32>; 3] = std::array::from_fn(|c| (0..256).map(|v| (((v - off[c]) as f64 * s[c]).floor() as i32).clamp(0, 255)).collect());
    // Luma histogram of the stage-1 image (unblurred).
    let mut h2 = [0u64; 256];
    for i in 0..n {
        let m: i32 = (0..3).map(|c| lut8[c][(img.p[c][i] >> 4) as usize]).sum();
        h2[((3 + 2 * m) / 6) as usize] += 1;
    }
    let cnt2 = (n as f64 * SH_PCT * 0.03 + 0.5) as u64;
    let mut b = 3usize;
    let mut acc = h2[2] + h2[3] + h2[4];
    while acc < cnt2 && b < 255 {
        b += 1;
        if b < 255 {
            acc = h2[b - 2] + h2[b - 1] + h2[b];
        }
    }
    let lo2 = b as i32;
    let cnt2h = if bvar2 { cnt2 / 2 } else { cnt2 };
    let mut u = 0xfcusize;
    acc = h2[0xfb] + h2[0xfc] + h2[0xfd];
    while acc < cnt2h && u > 3 {
        u -= 1;
        acc = h2[u - 1] + h2[u] + h2[u + 1];
    }
    let hi2 = u as i32;
    let mut k1 = 1.0;
    let mut luts: [Vec<u16>; 3] = Default::default();
    let stage1_only = (lo2 <= SHADOW_THR && HIGHLIGHT_THR <= hi2) || (hi2 as f64) < lo2 as f64 + 63.75;
    if stage1_only {
        for c in 0..3 {
            luts[c] = (0..=MAXV).map(|x| ((x as f64 - (off[c] << 4) as f64) * s[c]).floor().clamp(0.0, MAXV as f64) as u16).collect();
        }
    } else {
        let tgt_lo = if lo2 > SHADOW_THR { lo2 * 2 / 3 } else { lo2 };
        let tgt_hi = if bvar2 { (hi2 * 3 + 255) >> 2 } else { (hi2 + 255) / 2 };
        let (l2, h2f, tl, th) = ((lo2 << 4) as f64, (hi2 << 4) as f64, (tgt_lo << 4) as f64, (tgt_hi << 4) as f64);
        k1 = if h2f > l2 { (th - tl) / (h2f - l2) } else { 1.0 };
        let k2 = if (MAXV as f64) > h2f { (MAXV as f64 - th) / (MAXV as f64 - h2f) } else { 1.0 };
        for c in 0..3 {
            luts[c] = (0..=MAXV)
                .map(|x| {
                    let y1 = (x as f64 - (off[c] << 4) as f64) * s[c];
                    let y = if y1 < l2 {
                        y1 * if l2 != 0.0 { tl / l2 } else { 1.0 }
                    } else if y1 <= h2f {
                        tl + (y1 - l2) * k1
                    } else {
                        th + (y1 - h2f) * k2
                    };
                    (y + 0.5).floor().clamp(0.0, MAXV as f64) as u16
                })
                .collect();
        }
    }
    for c in 0..3 {
        let l = &luts[c];
        img.p[c].par_iter_mut().for_each(|v| *v = l[(*v as usize).min(MAXV)]);
    }
    GainInfo { lo, hi, lo2, hi2, k1, p: k1 * gain_avg }
}

// Romm12 defaults of ImaEBPGammaOp.
const G_LO: f64 = 0.7;
const G_HI: f64 = 2.0;
const MEAN_LO_PCT: f64 = 30.07;
const MEAN_HI_PCT: f64 = 39.84;
const EDGE_THR: f64 = 0.75;
const CITRAS: f64 = 0.6;

/// How Gamma picks its exponent.
#[derive(Debug, Clone, Copy)]
pub enum Gamma {
    /// The decoded estimator, clamped to `[0.7, cap]`.
    Auto { cap: f64 },
    Fixed(f64),
}

/// Trimmed-histogram statistics of the luma, the inputs of the estimator.
#[derive(Debug, Clone, Copy)]
pub struct GammaEstimate {
    pub gamma: f64,
    pub mean: f64,
    pub median: i32,
}

/// The estimator: 5x5-blurred 8-bit luma histogram with the two outermost
/// non-empty bins dropped at each end gives `mean` and `median`; a dark image
/// (mean below 30 %) gets `g2` (pulling the median up), a bright one `g1`
/// (pulling the mean of the edge pixels to the target), in between a blend.
/// Narrow histograms (span <= 63.75 bins) give 1. The edge-pixel threshold
/// scale (`EDGE_THR * sigma`) is reconstructed, not read from the DLL.
pub fn estimate_gamma(img: &Planes) -> GammaEstimate {
    let (w, h) = (img.w, img.h);
    let n = w * h;
    let t_lo = (mean_lo_pct() * 2.56).ceil();
    let t_hi = (mean_hi_pct() * 2.56).ceil();
    let l8: Vec<i32> = (0..n).map(|i| (((img.p[0][i] as i32 + img.p[1][i] as i32 + img.p[2][i] as i32) + 1) / 3) >> 4).collect();
    let s5 = binom5(&l8, w, h);
    let mut hist = [0i64; 256];
    for &s in &s5 {
        hist[((s + 128) >> 8).clamp(0, 255) as usize] += 1;
    }
    let (mut i, mut seen, mut cnt_lo, mut sum_lo) = (0usize, 0, 0i64, 0i64);
    while i < 255 && seen < 2 {
        if hist[i] > 0 {
            cnt_lo += hist[i];
            seen += 1;
            sum_lo += hist[i] * i as i64;
        }
        i += 1;
    }
    let lo_end = i as i64;
    let (mut j, mut seen, mut cnt_hi, mut sum_hi) = (255i64, 0, 0i64, 0i64);
    while j >= 0 && seen < 2 {
        if hist[j as usize] > 0 {
            cnt_hi += hist[j as usize];
            seen += 1;
            sum_hi += hist[j as usize] * j;
        }
        j -= 1;
    }
    let inner = n as i64 - cnt_hi - cnt_lo;
    let half = (inner as f64 * 0.5).ceil() as i64;
    let mut est = GammaEstimate { gamma: 1.0, mean: 0.0, median: 0 };
    if half >= 1 && ((j - (lo_end - 1) - 1) as f64) > 63.75 {
        let (mut cum, mut k) = (0i64, 0usize);
        while cum - cnt_lo < half && k < 256 {
            cum += hist[k];
            k += 1;
        }
        let med = k as i32 - 1;
        let tot: i64 = hist.iter().enumerate().map(|(i, &c)| c * i as i64).sum();
        let mean = (tot - sum_lo - sum_hi) as f64 / inner.max(1) as f64;
        let mut g1 = 1.0;
        if mean >= t_lo {
            let s3 = binom3(&l8, w, h);
            let d = |i: usize| s3[i] as f64 / 16.0 - s5[i] as f64 / 256.0;
            let ss: f64 = (0..n).into_par_iter().map(|i| d(i) * d(i)).sum();
            let sig = (ss / (n.max(2) - 1) as f64).sqrt();
            let thr = (sig * EDGE_THR).trunc();
            let (mut sum, mut cnt) = (0f64, 0u64);
            for i in 0..n {
                if d(i).abs() > thr {
                    sum += l8[i] as f64;
                    cnt += 1;
                }
            }
            if cnt > 0 {
                let e = sum / cnt as f64 / 255.0;
                if e.ln() != 0.0 {
                    g1 = ((mean_hi_pct() * 0.01 + e) * 0.5).ln() / e.ln();
                }
            }
        }
        let g2 = (mean < t_hi).then(|| (((med as f64 * 3.0 + t_hi) * mean / (med as f64 * 3.0 + mean)) / 255.0).ln() / (mean / 255.0).ln());
        let g = match g2 {
            None => g1,
            Some(g2) if mean <= t_lo => g2,
            Some(g2) => ((mean - t_lo) * g1 + (t_hi - mean) * g2) / (t_hi - t_lo),
        };
        est = GammaEstimate { gamma: g.clamp(G_LO, G_HI), mean, median: med };
    }
    est
}

/// Gamma: luma through `x^g`, chroma scaled by `1 + citras * (slope - 1)`
/// (`p` is [`GainInfo::p`]). Returns the exponent used.
pub fn gamma_correction(img: &mut Planes, p: f64, mode: Gamma) -> f64 {
    let g = match mode {
        Gamma::Fixed(g) => g,
        Gamma::Auto { cap } => estimate_gamma(img).gamma.min(cap),
    };
    let lut_l: Vec<i32> = (0..=MAXV).map(|i| ((MAXV as f64 * (i as f64 / MAXV as f64).powf(g) + 0.5).floor() as i32).clamp(0, MAXV as i32)).collect();
    let gain_l: Vec<i32> = (0..=MAXV)
        .map(|i| {
            let t = i as f64 / (MAXV as f64 * p);
            let mut slope = t.powf(g - 1.0) * p.powf(g) * g;
            if !slope.is_finite() {
                slope = 1.0;
            }
            (((slope - 1.0) * citras() + 1.0) * (1.0 / p) * 4096.0 + 0.5).floor() as i32
        })
        .collect();
    let [pr, pg, pb] = &mut img.p;
    (pr, pg, pb).into_par_iter_zip(|r, g_, b| {
        let l = ((*r as i32 + *g_ as i32 + *b as i32) * 2 + 3) / 6;
        let (ln, gk) = (lut_l[l as usize], gain_l[l as usize]);
        for v in [r, g_, b] {
            let c = *v as i32;
            *v = (ln + (((c - l) * gk + 0x800) >> 12)).clamp(0, MAXV as i32) as u16;
        }
    });
    g
}

trait ZipThree {
    fn into_par_iter_zip(self, f: impl Fn(&mut u16, &mut u16, &mut u16) + Sync + Send);
}
impl ZipThree for (&mut Vec<u16>, &mut Vec<u16>, &mut Vec<u16>) {
    fn into_par_iter_zip(self, f: impl Fn(&mut u16, &mut u16, &mut u16) + Sync + Send) {
        self.0.par_iter_mut().zip(self.1.par_iter_mut()).zip(self.2.par_iter_mut()).for_each(|((a, b), c)| f(a, b, c));
    }
}

/// The whole chain on a ROMM12 image: Gain and Offset, then Gamma.
pub fn enhanced_black_printing(img: &mut Planes, gamma: Gamma) -> (GainInfo, f64) {
    let gi = gain_and_offset(img);
    let g = gamma_correction(img, gi.p, gamma);
    (gi, g)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A smooth gradient with per-channel ranges, `lo..hi` in 12-bit units.
    fn ramp(w: usize, h: usize, lo: [u16; 3], hi: [u16; 3]) -> Planes {
        let mut p = Planes::new(w, h);
        for c in 0..3 {
            for y in 0..h {
                for x in 0..w {
                    let t = (x + y * 3) as f64 / (w + 3 * h) as f64;
                    p.p[c][y * w + x] = (lo[c] as f64 + t * (hi[c] - lo[c]) as f64) as u16;
                }
            }
        }
        p
    }

    #[test]
    fn blur_keeps_constants_and_sums_to_256() {
        let v = vec![7i32; 40 * 30];
        assert!(binom5(&v, 40, 30).iter().all(|&s| s == 7 * 256));
        assert!(binom3(&v, 40, 30).iter().all(|&s| s == 7 * 16));
    }

    #[test]
    fn gain_and_offset_stretches_each_channel_to_full_range() {
        let mut im = ramp(400, 300, [400, 380, 360], [3500, 3600, 3700]);
        let gi = gain_and_offset(&mut im);
        for c in 0..3 {
            let (mn, mx) = (*im.p[c].iter().min().unwrap(), *im.p[c].iter().max().unwrap());
            assert!(mn < 40, "ch {c} min {mn}");
            assert!(mx > 3900, "ch {c} max {mx}");
        }
        assert!(gi.p > 1.0 && gi.p < 1.4, "p {}", gi.p);
        assert!(gi.hi[0] > gi.lo[0]);
    }

    #[test]
    fn short_histogram_gets_the_knee_and_stays_ordered() {
        // 8-bit range 100..140: stage 1 stretches by ~2x (capped), the luma
        // histogram is then wider than 63.75 bins so the knee follows.
        let mut im = ramp(400, 300, [1600, 1600, 1600], [2240, 2240, 2240]);
        let gi = gain_and_offset(&mut im);
        assert_eq!((gi.lo, gi.hi), ([100; 3], [138; 3]));
        assert!(gi.k1 > 1.0, "knee expected: {gi:?}");
        let row = &im.p[0][..400];
        assert!(row.windows(2).all(|w| w[0] <= w[1]));
        assert!(im.p[0].iter().all(|&v| v <= 4095));
    }

    #[test]
    fn gamma_one_is_identity_for_neutral_and_chroma() {
        let mut im = ramp(64, 48, [100, 500, 900], [3000, 3200, 3400]);
        let before = im.clone();
        let g = gamma_correction(&mut im, 1.0, Gamma::Fixed(1.0));
        assert_eq!(g, 1.0);
        for c in 0..3 {
            for (a, b) in im.p[c].iter().zip(&before.p[c]) {
                assert!((*a as i32 - *b as i32).abs() <= 1);
            }
        }
    }

    #[test]
    fn gamma_below_one_brightens_midtones() {
        let mut im = ramp(64, 48, [0, 0, 0], [4095, 4095, 4095]);
        let mid = im.p[1][24 * 64 + 32];
        gamma_correction(&mut im, 1.0, Gamma::Fixed(0.8));
        assert!(im.p[1][24 * 64 + 32] > mid + 100);
    }

    #[test]
    fn estimator_is_one_for_a_narrow_histogram_and_clamped_otherwise() {
        let narrow = ramp(200, 150, [1000; 3], [1400; 3]);
        assert_eq!(estimate_gamma(&narrow).gamma, 1.0);
        let dark = ramp(200, 150, [0; 3], [1500; 3]);
        let g = estimate_gamma(&dark).gamma;
        assert!((G_LO..=G_HI).contains(&g), "{g}");
        assert!(g < 1.0, "a dark image is brightened: {g}");
    }
}
