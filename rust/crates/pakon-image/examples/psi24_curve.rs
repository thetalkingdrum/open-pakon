//! Per-column EBP transfer: ours vs the DLL (real 8-bit output inverted through srgb.pf).
//! `psi24_curve <lut> <in.raw> <real.rgb> <shifts> [gamma] > csv`: x, pre r g b, ours-post r g b, real-post r g b (ROMM12)
use pakon_image::ebp::{self, Gamma, Planes};
use pakon_image::psi24::tables;
fn u16s(p: &str) -> Vec<u16> {
    std::fs::read(p)
        .unwrap()
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect()
}
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (lut, raw, real) = (u16s(&a[1]), u16s(&a[2]), std::fs::read(&a[3]).unwrap());
    let sh: Vec<f32> = a[4].split(',').map(|v| v.parse().unwrap()).collect();
    let gm = match a.get(5).map(String::as_str) {
        None | Some("auto") => Gamma::Auto { cap: 9.0 },
        Some(v) => Gamma::Fixed(v.parse().unwrap()),
    };
    let (w, h) = (2250usize, 1500usize);
    let t = tables();
    let mut pl = Planes::new(w, h);
    for i in 0..w * h {
        // rpd -> romm12
        let rpd: Vec<f32> = (0..3)
            .map(|c| {
                (lut[c * 4096 + raw[c * w * h + i].min(4095) as usize] as f32 + sh[c])
                    .clamp(0.0, 4095.0)
                    / 4095.0
            })
            .collect();
        let s = t.rpd_romm.lookup([rpd[0], rpd[1], rpd[2]]);
        for c in 0..3 {
            pl.p[c][i] = (s[c] * 4095.0 + 0.5).clamp(0.0, 4095.0) as u16;
        }
    }
    let pre = pl.clone();
    let (gi, g) = ebp::enhanced_black_printing(&mut pl, gm);
    eprintln!("ours: gamma {g:.3} {gi:?}");
    let row = 750;
    for x in (0..w).step_by(25).chain([w - 1]) {
        let i = row * w + x;
        let tgt: [f32; 3] = std::array::from_fn(|c| real[c * w * h + i] as f32 / 255.0);
        // Newton from the ours-post start (finite differences)
        let mut v: [f32; 3] = std::array::from_fn(|c| pl.p[c][i] as f32 / 4095.0);
        for _ in 0..30 {
            let f = t.romm_srgb.lookup(v);
            let r: [f32; 3] = std::array::from_fn(|c| f[c] - tgt[c]);
            if r.iter().all(|e| e.abs() < 0.0006) {
                break;
            }
            let mut j = [[0f32; 3]; 3];
            for k in 0..3 {
                let mut vv = v;
                vv[k] += 0.004;
                let f2 = t.romm_srgb.lookup(vv);
                for c in 0..3 {
                    j[c][k] = (f2[c] - f[c]) / 0.004;
                }
            }
            // solve j d = -r (Cramer)
            let det = |m: [[f32; 3]; 3]| {
                m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
                    - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
                    + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
            };
            let d0 = det(j);
            if d0.abs() < 1e-6 {
                break;
            }
            for k in 0..3 {
                let mut m = j;
                for c in 0..3 {
                    m[c][k] = -r[c];
                }
                v[k] = (v[k] + det(m) / d0 * 0.8).clamp(0.0, 1.0);
            }
        }
        println!(
            "{x},{},{},{},{},{},{},{},{},{},{},{},{}",
            pre.p[0][i],
            pre.p[1][i],
            pre.p[2][i],
            pl.p[0][i],
            pl.p[1][i],
            pl.p[2][i],
            (v[0] * 4095.0) as i32,
            (v[1] * 4095.0) as i32,
            (v[2] * 4095.0) as i32,
            real[i],
            real[w * h + i],
            real[2 * w * h + i]
        );
    }
}
