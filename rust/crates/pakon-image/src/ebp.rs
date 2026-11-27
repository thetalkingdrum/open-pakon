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

/// The 8-bit instance's blurs: horizontal pass then vertical pass, each divided
/// by its weight sum with truncation (`>> 4` for [1 4 6 4 1], `>> 2` for
/// [1 2 1]); reproduces the DLL's dumped histograms bit for bit. Returns 8-bit values.
fn blur_floor(src: &[i32], w: usize, h: usize, k: &[i32], shift: u32) -> Vec<i32> {
    let r = k.len() / 2;
    let mut tmp = vec![0i32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let row = &src[y * w..(y + 1) * w];
        for x in 0..w {
            let mut s = 0;
            for (i, wt) in k.iter().enumerate() {
                s += wt * row[(x + i).saturating_sub(r).min(w - 1)];
            }
            out[x] = s >> shift;
        }
    });
    let mut out = vec![0i32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, o)| {
        for x in 0..w {
            let mut s = 0;
            for (i, wt) in k.iter().enumerate() {
                s += wt * tmp[((y + i).saturating_sub(r)).min(h - 1) * w + x];
            }
            o[x] = s >> shift;
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
    /// Stage-1 offsets (8-bit units) and gains, as applied (for the HQ path).
    pub off: [i32; 3],
    pub s: [f64; 3],
    /// Knee `(l2, h2, tl, th, k1, k2)` in 12-bit units, `None` when only stage 1 ran.
    pub knee: Option<[f64; 6]>,
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
        if plain8() {
            for v in blur_floor(&x8, w, h, &[1, 4, 6, 4, 1], 4) {
                hist[c][v.clamp(0, 255) as usize] += 1;
            }
        } else {
            for s in binom5(&x8, w, h) {
                hist[c][(s >> 8).clamp(0, 255) as usize] += 1;
            }
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
    // A channel with no range (hi <= lo) counts as empty: range 0 makes the whole stage the identity.
    let r: [i32; 3] = std::array::from_fn(|c| if hi[c] <= lo[c] { 0 } else { hi[c] - lo[c] });
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
    let mut knee = None;
    let mut luts: [Vec<u16>; 3] = Default::default();
    let stage1_only = if std::env::var("EBP_OR").is_ok() { lo2 <= SHADOW_THR || HIGHLIGHT_THR <= hi2 } else { lo2 <= SHADOW_THR && HIGHLIGHT_THR <= hi2 } || (hi2 as f64) < lo2 as f64 + 63.75;
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
        knee = Some([l2, h2f, tl, th, k1, k2]);
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
    GainInfo { lo, hi, lo2, hi2, k1, p: k1 * gain_avg, off, s, knee }
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
    let l8: Vec<i32> = (0..n).map(|i| if plain8() { ((img.p[0][i] >> 4) as i32 + (img.p[1][i] >> 4) as i32 + (img.p[2][i] >> 4) as i32 + 1) / 3 } else { (((img.p[0][i] as i32 + img.p[1][i] as i32 + img.p[2][i] as i32) + 1) / 3) >> 4 }).collect();
    let (s5, s5_scale) = if plain8() { (blur_floor(&l8, w, h, &[1, 4, 6, 4, 1], 4), 1.0) } else { (binom5(&l8, w, h), 256.0) };
    let mut hist = [0i64; 256];
    for &s in &s5 {
        hist[if plain8() { s } else { (s + 128) >> 8 }.clamp(0, 255) as usize] += 1;
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
            let (s3, s3_scale) = if plain8() { (blur_floor(&l8, w, h, &[1, 2, 1], 2), 1.0) } else { (binom3(&l8, w, h), 16.0) };
            let d = |i: usize| s3[i] as f64 / s3_scale - s5[i] as f64 / s5_scale;
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
    if plain8() {
        // 8-bit instance: 256-entry tables, chroma term truncated toward zero (checked
        // against the real stage dumps: 92 % of samples exact, the rest within 1).
        const M8: i32 = 255;
        let lut8: Vec<i32> = (0..=M8).map(|i| ((M8 as f64 * (i as f64 / M8 as f64).powf(g) + 0.5).floor() as i32).clamp(0, M8)).collect();
        let gain8: Vec<i32> = (0..=M8)
            .map(|i| {
                let t = i as f64 / (M8 as f64 * p);
                let mut slope = t.powf(g - 1.0) * p.powf(g) * g;
                if !slope.is_finite() {
                    slope = 1.0;
                }
                (((slope - 1.0) * citras() + 1.0) * (1.0 / p) * 4096.0 + 0.5).floor() as i32
            })
            .collect();
        let [pr, pg, pb] = &mut img.p;
        (pr, pg, pb).into_par_iter_zip(|r, g_, b| {
            let (r8, g8, b8) = ((*r >> 4) as i32, (*g_ >> 4) as i32, (*b >> 4) as i32);
            let l = ((r8 + g8 + b8) + 1) / 3;
            let (ln, gk) = (lut8[l as usize], gain8[l as usize]);
            for (v, c) in [(r, r8), (g_, g8), (b, b8)] {
                *v = ((ln + ((c - l) * gk) / 4096).clamp(0, M8) as u16) << 4;
            }
        });
        return g;
    }
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
    if plain8() && std::env::var("PSI24_NO_NF").is_err() {
        noise_filter(img);
    }
    if plain8() && std::env::var("PSI24_NO_BP").is_err() {
        black_print(img);
    }
    (gi, g)
}


// ImaEBPNoiseFilterOp<uchar>, exec at 0x1016c830, decoded from the DLL (attributes at
// +0xc4 = 20.0, +0xc8 = 2.0, +0xcc = 10.0) and checked against the stage dumps of four real
// frames (98.4-99.6 % of samples exact). It is image-adaptive:
//   1. noise estimate nv = sum((d*d + 32) >> 6) / (n*64 - 64) over the interior, d = the 3x3
//      Laplacian (8*c - sum of the 8 neighbours) of R - B;
//   2. luma Y = (R+G+B)/3; if nv > 0.25 Y is first blurred with [1 1 1; 1 8 1; 1 1 1] >> 4;
//   3. detail hp3 = Y - lp3(Y), hp5 = Y - lp5(Y) (+128, byte): lp3 = [1 3 1]/5 per axis,
//      lp5 = [1 4 6 4 1] >> 4 per axis, every pass truncated;
//   4. Y += trunc(core(hp3) * A + .5) + trunc(core(hp5) * B + .5), where core() is a soft
//      coring with threshold X = min(trunc(20 nv + .5), 10), A = min(.4 + 2 nv, 1.5),
//      B = min(.2 + 2 nv, 1);
//   5. chroma C1 = (2G-R-B)/4, C2 = (B-R)/2 (+128) of the unblurred image, blurred with the
//      same 3x3 kernel, then smoothed by m = trunc(10 nv + .5): m < 3 [1 3 1]/5, 3 -> [1 2 1] >> 2,
//      4..5 -> [1 4 6 4 1] >> 4, >= 6 -> binomial 9 >> 8 (horizontal then vertical, truncated);
//   6. R = Y - (4 c1 + 3)/6 - c2, G = Y + (8 c1 + 3)/6, B = Y - (4 c1 + 3)/6 + c2 (C division).
// Left out: the DLL's first output column/row of the 5-tap filter weighs 12:3:1 instead of
// 11:4:1 (no measurable effect).

fn conv_sep(src: &[i32], w: usize, h: usize, co: &[i32], norm: impl Fn(i32) -> i32 + Sync) -> Vec<i32> {
    conv_sep_v(src, w, h, co, co, norm)
}

/// [`conv_sep`] with its own taps `cv` for the vertical pass.
fn conv_sep_v(src: &[i32], w: usize, h: usize, co: &[i32], cv: &[i32], norm: impl Fn(i32) -> i32 + Sync) -> Vec<i32> {
    let k = co.len();
    let l = (k - 1) / 2;
    let mut tmp = vec![0i32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let s = &src[y * w..(y + 1) * w];
        for x in 0..w {
            let mut acc = 0;
            for (i, &c) in co.iter().enumerate() {
                let xx = (x as isize + i as isize - l as isize).clamp(0, w as isize - 1) as usize;
                acc += c * s[xx];
            }
            row[x] = norm(acc);
        }
    });
    let mut out = vec![0i32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let mut acc = 0;
            for (i, &c) in cv.iter().enumerate() {
                let yy = (y as isize + i as isize - l as isize).clamp(0, h as isize - 1) as usize;
                acc += c * tmp[yy * w + x];
            }
            row[x] = norm(acc);
        }
    });
    out
}

fn binom_row(k: usize) -> Vec<i32> {
    let mut r = vec![1i32];
    for _ in 1..k {
        let mut n = vec![1i32; r.len() + 1];
        for i in 1..r.len() {
            n[i] = r[i - 1] + r[i];
        }
        r = n;
    }
    r
}

/// 3x3 [1 1 1; 1 8 1; 1 1 1] >> 4 (truncating), edges replicated.
fn blur_bp_trunc(src: &[i32], w: usize, h: usize) -> Vec<i32> {
    let mut o = vec![0i32; w * h];
    o.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (ya, yb) = (y.saturating_sub(1), (y + 1).min(h - 1));
        for x in 0..w {
            let (xa, xb) = (x.saturating_sub(1), (x + 1).min(w - 1));
            let s = src[ya * w + xa] + src[ya * w + x] + src[ya * w + xb] + src[y * w + xa] + src[y * w + xb] + src[yb * w + xa] + src[yb * w + x] + src[yb * w + xb];
            row[x] = (s + 8 * src[y * w + x]) >> 4;
        }
    });
    o
}

fn nf_hp(y: &[i32], w: usize, h: usize, k: usize) -> Vec<usize> {
    // The DLL's vertical lp3 (FUN_10170b90, k == 3) overwrites its row buffer before reloading it, so it
    // weights rows (y-1, y, y+1) as [1, 2, 2]/5, not [1, 3, 1]/5 (a 0.2 px shift; horizontal stays [1, 3, 1]).
    let lp = if k == 3 { conv_sep_v(y, w, h, &[1, 3, 1], &[1, 2, 2], |a| a / 5) } else { conv_sep(y, w, h, &[1, 4, 6, 4, 1], |a| a >> 4) };
    y.iter().zip(&lp).map(|(&a, &b)| ((a - b + 128) & 255) as usize).collect()
}

fn nf_table(x: i32, gain: f64) -> [i32; 256] {
    std::array::from_fn(|k| {
        let d = k as i32 - 128;
        let v = if d >= x { d - x } else if d < -x { d + x } else { 0 };
        if v == 0 { 0 } else { (v as f64 * gain + 0.5) as i32 }
    })
}

/// The noise estimate `nv` (float32 in the DLL) of an 8-bit planar image.
pub fn noise_level(r: &[i32], b: &[i32], w: usize, h: usize) -> f32 {
    let d: Vec<i32> = r.iter().zip(b).map(|(&a, &c)| a - c).collect();
    let sum: i64 = (1..h - 1)
        .into_par_iter()
        .map(|y| {
            let mut s = 0i64;
            for x in 1..w - 1 {
                let i = y * w + x;
                let nb = d[i - w - 1] + d[i - w] + d[i - w + 1] + d[i - 1] + d[i + 1] + d[i + w - 1] + d[i + w] + d[i + w + 1];
                let v = 8 * d[i] - nb;
                s += ((v * v + 32) >> 6) as i64;
            }
            s
        })
        .sum();
    (sum as f64 / (((w - 2) * (h - 2)) as f64 * 64.0 - 64.0)) as f32
}

pub fn noise_filter(img: &mut Planes) {
    let (w, h) = (img.w, img.h);
    if w < 3 || h < 3 {
        return;
    }
    let raw: [Vec<i32>; 3] = std::array::from_fn(|c| img.p[c].iter().map(|&v| ((v >> 4) as i32).min(255)).collect());
    let nv = noise_level(&raw[0], &raw[2], w, h) as f64;
    let x = ((20.0 * nv + 0.5) as i32).min(10);
    let m = (10.0 * nv + 0.5) as i32;
    let a = (0.4 + 2.0 * nv).min(1.5);
    let b = (0.2 + 2.0 * nv).min(1.0);
    let n = w * h;
    let mut y: Vec<i32> = (0..n).map(|i| (raw[0][i] + raw[1][i] + raw[2][i]) / 3).collect();
    let c1: Vec<i32> = (0..n).map(|i| ((2 * raw[1][i] - raw[2][i] - raw[0][i]) / 4 + 128) & 255).collect();
    let c2: Vec<i32> = (0..n).map(|i| ((raw[2][i] - raw[0][i]) / 2 + 128) & 255).collect();
    if nv > 0.25 {
        y = blur_bp_trunc(&y, w, h);
    }
    let (h3, h5) = (nf_hp(&y, w, h, 3), nf_hp(&y, w, h, 5));
    let (ta, tb) = (nf_table(x, a), nf_table(x, b));
    for i in 0..n {
        y[i] = (y[i] + ta[h3[i]] + tb[h5[i]]).clamp(0, 255);
    }
    let sm = |p: &[i32]| -> Vec<i32> {
        let p = blur_bp_trunc(p, w, h);
        match m {
            i32::MIN..=2 => conv_sep(&p, w, h, &[1, 3, 1], |a| a / 5),
            3 => conv_sep(&p, w, h, &binom_row(3), |a| a >> 2),
            4 | 5 => conv_sep(&p, w, h, &binom_row(5), |a| a >> 4),
            _ => conv_sep(&p, w, h, &binom_row(9), |a| a >> 8),
        }
    };
    let (c1, c2) = (sm(&c1), sm(&c2));
    let [pr, pg, pb] = &mut img.p;
    pr.par_iter_mut().zip(pg.par_iter_mut()).zip(pb.par_iter_mut()).enumerate().for_each(|(i, ((r, g), bl))| {
        let (u, v) = (c1[i] - 128, c2[i] - 128);
        let t = (u * 4 + 3) / 6;
        *r = ((y[i] - t - v).clamp(0, 255) as u16) << 4;
        *g = ((y[i] + (u * 8 + 3) / 6).clamp(0, 255) as u16) << 4;
        *bl = ((y[i] - t + v).clamp(0, 255) as u16) << 4;
    });
}

// ImaEBPBlackPrintOp<uchar> defaults (EBP_FINDINGS.md section 3).
const BP_LUMA: f64 = 0.5;
const BP_CHROMA: f64 = 0.25;
const BP_LUMA_MAX: f64 = 50.0;
const BP_CHROMA_MAX: f64 = 20.0;
const BP_RATIO: f64 = 0.4;

/// Black Print on 8-bit data held as `v8 << 4`: in pixels darker than a
/// threshold (about a third of the mean) the colour blends to a 3x3-blurred,
/// and where the local chroma energy is small to a grey, version. The mean `M`
/// is taken over all samples of the image (which samples the DLL uses is not
/// established; it runs the op in 70 strips).
pub fn black_print(img: &mut Planes) {
    let (w, h) = (img.w, img.h);
    if w < 2 || h < 2 {
        return;
    }
    let raw: [Vec<i32>; 3] = std::array::from_fn(|c| img.p[c].iter().map(|&v| ((v as i32 + 8) >> 4).min(255)).collect());
    let m: f64 = raw.iter().map(|p| p.iter().map(|&v| v as i64).sum::<i64>()).sum::<i64>() as f64 / (3 * w * h) as f64;
    let lt = ((m * BP_LUMA) as i32).clamp(1, BP_LUMA_MAX as i32) as f64;
    let ct = ((m * BP_CHROMA) as i32).clamp(1, BP_CHROMA_MAX as i32) as f64;
    let lut1: Vec<i32> = (0..256).map(|v| (((lt - v as f64) * 4096.0 / (lt * BP_RATIO) + 0.5).floor() as i32).clamp(0, 4096)).collect();
    let n2 = (255.0 * 1.5) as usize + 1;
    let lut2: Vec<i32> = (0..n2).map(|v| (((ct - v as f64) * 4096.0 / (ct * BP_RATIO) + 0.5).floor() as i32).clamp(0, 4096)).collect();
    // 3x3 blur [1 1 1; 1 8 1; 1 1 1] / 16, edges replicated.
    let blur: [Vec<i32>; 3] = std::array::from_fn(|c| {
        let p = &raw[c];
        let mut o = vec![0i32; w * h];
        o.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let (ya, yb) = (y.saturating_sub(1), (y + 1).min(h - 1));
            for x in 0..w {
                let (xa, xb) = (x.saturating_sub(1), (x + 1).min(w - 1));
                let s = p[ya * w + xa] + p[ya * w + x] + p[ya * w + xb] + p[y * w + xa] + p[y * w + xb] + p[yb * w + xa] + p[yb * w + x] + p[yb * w + xb];
                row[x] = (s + 8 * p[y * w + x] + 8) >> 4;
            }
        });
        o
    });
    let [pr, pg, pb] = &mut img.p;
    pr.par_chunks_mut(w).zip(pg.par_chunks_mut(w)).zip(pb.par_chunks_mut(w)).enumerate().for_each(|(y, ((r, g), b))| {
        for x in 0..w {
            let i = y * w + x;
            let l = (blur[0][i] + blur[1][i] + blur[2][i]) / 3;
            let wgt = lut1[l.clamp(0, 255) as usize];
            if wgt == 0 {
                continue;
            }
            let e: i32 = (0..3).map(|c| (l - raw[c][i]).abs()).sum();
            let q = lut2[(e as usize).min(n2 - 1)];
            let i13 = ((4096 - q) * wgt + 0x800) >> 12;
            let s2 = (((q * l + 0x800) >> 12) * wgt + 0x800) >> 12;
            let dst = [&mut r[x], &mut g[x], &mut b[x]];
            for (c, d) in dst.into_iter().enumerate() {
                let o = ((raw[c][i] * (4096 - wgt) + 0x800) >> 12) + s2 + ((blur[c][i] * i13 + 0x800) >> 12);
                *d = (o.clamp(0, 255) as u16) << 4;
            }
        }
    });
}

// ---------------------------------------------------------------------------------------------
// High-bit-depth ("HQ") path. The same four 'E' stages in the sRGB domain, on float planes with
// the 0..255 scale, without any quantisation between the stages. Every *decision* (G&O lo/hi/lo2/
// hi2/k1/p, the Gamma exponent, the Noise Filter nv/X/m/A/B, the Black Print mean) is taken from
// the exact 8-bit chain ([`hq_params`]), so the look is the one of the bit-exact path; only the
// curves and filters are applied continuously. Thresholds stay on the 0..255 scale.

/// A float three-plane image, values on a 0..255 scale (not clamped between the stages except where
/// the 8-bit stage clamps).
#[derive(Debug, Clone)]
pub struct FPlanes {
    pub w: usize,
    pub h: usize,
    pub p: [Vec<f32>; 3],
}

/// Decisions of the exact 8-bit chain, the parameters of [`hq_apply`].
#[derive(Debug, Clone, Copy)]
pub struct HqParams {
    pub gain: GainInfo,
    pub gamma: f64,
    /// Noise estimate entering the Noise Filter (after Gamma).
    pub nv: f64,
    /// Black Print mean over all samples (after the Noise Filter).
    pub bp_mean: f64,
}

/// Run the exact 8-bit chain on `q8` (8-bit values held as `v8 << 4`, [`set_plain8`] on) and
/// record its decisions. `q8` ends up as the exact 8-bit result.
pub fn hq_params(q8: &mut Planes, gamma: Gamma) -> HqParams {
    let gain = gain_and_offset(q8);
    let g = gamma_correction(q8, gain.p, gamma);
    let (w, h) = (q8.w, q8.h);
    let raw: [Vec<i32>; 3] = std::array::from_fn(|c| q8.p[c].iter().map(|&v| ((v >> 4) as i32).min(255)).collect());
    let nv = if w >= 3 && h >= 3 { noise_level(&raw[0], &raw[2], w, h) as f64 } else { 0.0 };
    if std::env::var("PSI24_NO_NF").is_err() {
        noise_filter(q8);
    }
    let tot: i64 = q8.p.iter().map(|p| p.iter().map(|&v| ((v as i64 + 8) >> 4).min(255)).sum::<i64>()).sum();
    HqParams { gain, gamma: g, nv, bp_mean: tot as f64 / (3 * w * h).max(1) as f64 }
}

/// Stage 1 + knee of Gain and Offset, continuous. `v` on the 0..255 scale.
fn go_hq(f: &mut FPlanes, gi: &GainInfo) {
    for c in 0..3 {
        let (off, s) = (gi.off[c] as f64, gi.s[c]);
        f.p[c].par_iter_mut().for_each(|v| {
            let y1 = (*v as f64 - off) * s * 16.0; // 12-bit units
            let y = match gi.knee {
                None => y1,
                Some([l2, h2, tl, th, k1, k2]) => {
                    if y1 < l2 {
                        y1 * if l2 != 0.0 { tl / l2 } else { 1.0 }
                    } else if y1 <= h2 {
                        tl + (y1 - l2) * k1
                    } else {
                        th + (y1 - h2) * k2
                    }
                }
            };
            *v = (y / 16.0).clamp(0.0, 255.0) as f32;
        });
    }
}

fn gamma_hq(f: &mut FPlanes, p: f64, g: f64) {
    // thread-local `PLAIN8` is only set on the calling thread: read it before going parallel
    let cit = citras();
    let [pr, pg, pb] = &mut f.p;
    pr.par_iter_mut().zip(pg.par_iter_mut()).zip(pb.par_iter_mut()).for_each(|((r, gg), b)| {
        let l = ((*r + *gg + *b) as f64 / 3.0).max(0.0);
        let ln = 255.0 * (l / 255.0).powf(g);
        let mut slope = (l.max(1.0) / (255.0 * p)).powf(g - 1.0) * p.powf(g) * g;
        if !slope.is_finite() {
            slope = 1.0;
        }
        let gk = ((slope - 1.0) * cit + 1.0) / p;
        for v in [r, gg, b] {
            *v = (ln + (*v as f64 - l) * gk).clamp(0.0, 255.0) as f32;
        }
    });
}

/// Separable float convolution with edge replication, normalised by the sum of the taps.
fn conv_sep_f(src: &[f32], w: usize, h: usize, co: &[f32]) -> Vec<f32> {
    let l = (co.len() - 1) / 2;
    let norm = 1.0 / co.iter().sum::<f32>();
    let mut tmp = vec![0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let s = &src[y * w..(y + 1) * w];
        for x in 0..w {
            let mut a = 0.0;
            for (i, &c) in co.iter().enumerate() {
                a += c * s[(x as isize + i as isize - l as isize).clamp(0, w as isize - 1) as usize];
            }
            row[x] = a * norm;
        }
    });
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (i, &c) in co.iter().enumerate() {
            let t = &tmp[(y as isize + i as isize - l as isize).clamp(0, h as isize - 1) as usize * w..][..w];
            for x in 0..w {
                row[x] += c * t[x] * norm;
            }
        }
    });
    out
}

/// 3x3 `[1 1 1; 1 8 1; 1 1 1] / 16`, edges replicated.
fn blur_bp_f(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut o = vec![0f32; w * h];
    o.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (ya, yb) = (y.saturating_sub(1), (y + 1).min(h - 1));
        for x in 0..w {
            let (xa, xb) = (x.saturating_sub(1), (x + 1).min(w - 1));
            let s = src[ya * w + xa] + src[ya * w + x] + src[ya * w + xb] + src[y * w + xa] + src[y * w + xb] + src[yb * w + xa] + src[yb * w + x] + src[yb * w + xb];
            row[x] = (s + 8.0 * src[y * w + x]) * (1.0 / 16.0);
        }
    });
    o
}

fn noise_filter_hq(f: &mut FPlanes, nv: f64) {
    let (w, h) = (f.w, f.h);
    if w < 3 || h < 3 {
        return;
    }
    let x = ((20.0 * nv + 0.5) as i32).min(10) as f32;
    let m = (10.0 * nv + 0.5) as i32;
    let a = (0.4 + 2.0 * nv).min(1.5) as f32;
    let b = (0.2 + 2.0 * nv).min(1.0) as f32;
    let n = w * h;
    let mut y: Vec<f32> = (0..n).map(|i| (f.p[0][i] + f.p[1][i] + f.p[2][i]) / 3.0).collect();
    let c1: Vec<f32> = (0..n).map(|i| (2.0 * f.p[1][i] - f.p[2][i] - f.p[0][i]) / 4.0).collect();
    let c2: Vec<f32> = (0..n).map(|i| (f.p[2][i] - f.p[0][i]) / 2.0).collect();
    if nv > 0.25 {
        y = blur_bp_f(&y, w, h);
    }
    let lp3 = conv_sep_f(&y, w, h, &[1.0, 3.0, 1.0]);
    let lp5 = conv_sep_f(&y, w, h, &[1.0, 4.0, 6.0, 4.0, 1.0]);
    let core = |d: f32| if d >= x { d - x } else if d < -x { d + x } else { 0.0 };
    y.par_iter_mut().zip(lp3.par_iter().zip(lp5.par_iter())).for_each(|(v, (&l3, &l5))| {
        let (h3, h5) = (*v - l3, *v - l5);
        *v = (*v + core(h3) * a + core(h5) * b).clamp(0.0, 255.0);
    });
    let sm = |p: &[f32]| -> Vec<f32> {
        let p = blur_bp_f(p, w, h);
        let k: Vec<f32> = match m {
            i32::MIN..=2 => vec![1.0, 3.0, 1.0],
            3 => binom_row(3).iter().map(|&v| v as f32).collect(),
            4 | 5 => binom_row(5).iter().map(|&v| v as f32).collect(),
            _ => binom_row(9).iter().map(|&v| v as f32).collect(),
        };
        conv_sep_f(&p, w, h, &k)
    };
    let (u, v) = (sm(&c1), sm(&c2));
    let [pr, pg, pb] = &mut f.p;
    pr.par_iter_mut().zip(pg.par_iter_mut()).zip(pb.par_iter_mut()).enumerate().for_each(|(i, ((r, g), bl))| {
        let t = u[i] * (4.0 / 6.0);
        *r = (y[i] - t - v[i]).clamp(0.0, 255.0);
        *g = (y[i] + u[i] * (8.0 / 6.0)).clamp(0.0, 255.0);
        *bl = (y[i] - t + v[i]).clamp(0.0, 255.0);
    });
}

fn black_print_hq(f: &mut FPlanes, m: f64) {
    let (w, h) = (f.w, f.h);
    if w < 2 || h < 2 {
        return;
    }
    let lt = ((m * BP_LUMA) as i32).clamp(1, BP_LUMA_MAX as i32) as f32;
    let ct = ((m * BP_CHROMA) as i32).clamp(1, BP_CHROMA_MAX as i32) as f32;
    let blur: [Vec<f32>; 3] = std::array::from_fn(|c| blur_bp_f(&f.p[c], w, h));
    let raw = f.p.clone();
    let [pr, pg, pb] = &mut f.p;
    pr.par_chunks_mut(w).zip(pg.par_chunks_mut(w)).zip(pb.par_chunks_mut(w)).enumerate().for_each(|(y, ((r, g), b))| {
        for x in 0..w {
            let i = y * w + x;
            let l = (blur[0][i] + blur[1][i] + blur[2][i]) / 3.0;
            let wgt = ((lt - l) / (lt * BP_RATIO as f32)).clamp(0.0, 1.0);
            if wgt == 0.0 {
                continue;
            }
            let e: f32 = (0..3).map(|c| (l - raw[c][i]).abs()).sum();
            let q = ((ct - e) / (ct * BP_RATIO as f32)).clamp(0.0, 1.0);
            let i13 = (1.0 - q) * wgt;
            let s2 = q * l * wgt;
            for (c, d) in [&mut r[x], &mut g[x], &mut b[x]].into_iter().enumerate() {
                *d = (raw[c][i] * (1.0 - wgt) + s2 + blur[c][i] * i13).clamp(0.0, 255.0);
            }
        }
    });
}

/// Mean level (0..255 scale) the integer chain loses per channel against the float one, which the
/// float path subtracts at the end so that the look (not just the decisions) stays that of the DLL.
/// Measured as (HQ - exact 8-bit) bias on the four reference frames (variant A): G and B +2.3 on all
/// four, R +0.1 .. +1.1. It comes from the floors/truncations of the 8-bit Noise Filter's chroma
/// path (3x3 blur, smoothing passes, `(4u+3)/6`) and the 8-bit Gamma/G&O rounding. Set
/// `PSI24_HQ_BIAS=0` to get the unbiased float result.
pub const HQ_BIAS: [f32; 3] = [0.55, 2.3, 2.3];

/// The HQ chain on float planes (0..255 scale): Gain and Offset, Gamma, Noise Filter, Black Print,
/// with the decisions in `p` ([`hq_params`]).
pub fn hq_apply(f: &mut FPlanes, p: &HqParams) {
    go_hq(f, &p.gain);
    gamma_hq(f, p.gain.p, p.gamma);
    if std::env::var("PSI24_NO_NF").is_err() {
        noise_filter_hq(f, p.nv);
    }
    if std::env::var("PSI24_NO_BP").is_err() {
        black_print_hq(f, p.bp_mean);
    }
    if std::env::var("PSI24_HQ_BIAS").map(|v| v != "0").unwrap_or(true) {
        for c in 0..3 {
            let b = HQ_BIAS[c];
            f.p[c].par_iter_mut().for_each(|v| *v = (*v - b).max(0.0));
        }
    }
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
