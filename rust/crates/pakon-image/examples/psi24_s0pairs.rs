//! Compare our rpd->romm->srgb tables with the DLL's CMM: `psi24_s0pairs <s0_pairs.csv>`
//! (columns: image,mode,cell,rpd_RGB,cmm_in_RGB,s0_RGB,patch_constant). Input = cmm_in.
use pakon_image::psi24::tables;
fn main() {
    let p = std::env::args().nth(1).unwrap();
    let t = tables();
    let txt = std::fs::read_to_string(p).unwrap();
    let mut n = 0usize;
    let mut st = [[0f64; 3]; 4]; // abs, signed for variants a (12-bit roundtrip) / b (direct)
    let mut rows = vec![];
    for l in txt.lines().skip(1) {
        let f: Vec<&str> = l.split(',').collect();
        if f[12] != "True" {
            continue;
        }
        let ci: Vec<f32> = (6..9).map(|i| f[i].parse().unwrap()).collect();
        let s0: Vec<f32> = (9..12).map(|i| f[i].parse().unwrap()).collect();
        let rm = t.rpd_romm.lookup([ci[0] / 4095.0, ci[1] / 4095.0, ci[2] / 4095.0]);
        let q = [(rm[0] * 4095.0 + 0.5).floor() / 4095.0, (rm[1] * 4095.0 + 0.5).floor() / 4095.0, (rm[2] * 4095.0 + 0.5).floor() / 4095.0];
        let a = t.romm_srgb.lookup(q);
        let b = t.romm_srgb.lookup(rm);
        let mut e = [0f32; 3];
        for c in 0..3 {
            let da = a[c] as f64 * 255.0 - s0[c] as f64;
            let db = b[c] as f64 * 255.0 - s0[c] as f64;
            st[0][c] += da.abs();
            st[1][c] += da;
            st[2][c] += db.abs();
            st[3][c] += db;
            e[c] = (b[c] * 255.0 - s0[c]) as f32;
        }
        if let Ok(o) = std::env::var("DUMP") { use std::io::Write; let mut fh = std::fs::OpenOptions::new().append(true).create(true).open(o).unwrap(); writeln!(fh, "{},{},{},{},{},{},{},{},{},{},{},{}", f[0], f[1], ci[0], ci[1], ci[2], s0[0], s0[1], s0[2], b[0]*255.0, b[1]*255.0, b[2]*255.0, 0).unwrap(); }
        rows.push((f[1].to_string(), ci, s0, e));
        n += 1;
    }
    let nf = n as f64;
    println!("n={n}\n12bit-roundtrip: MAE {:.3} {:.3} {:.3} bias {:+.3} {:+.3} {:+.3}", st[0][0] / nf, st[0][1] / nf, st[0][2] / nf, st[1][0] / nf, st[1][1] / nf, st[1][2] / nf);
    println!("direct:          MAE {:.3} {:.3} {:.3} bias {:+.3} {:+.3} {:+.3}", st[2][0] / nf, st[2][1] / nf, st[2][2] / nf, st[3][0] / nf, st[3][1] / nf, st[3][2] / nf);
    // direct error vs s0 level (R) for grey-ish: bucket by s0 level of each channel
    for c in 0..3 {
        let mut b = vec![(0f64, 0usize); 8];
        for (_, _, s0, e) in &rows {
            let k = ((s0[c] / 32.0) as usize).min(7);
            b[k].0 += e[c] as f64;
            b[k].1 += 1;
        }
        print!("ch{c} mean(ours-real) by s0/32: ");
        for (s, k) in &b {
            print!("{:+.2}({k}) ", if *k > 0 { s / *k as f64 } else { 0.0 });
        }
        println!();
    }
}
