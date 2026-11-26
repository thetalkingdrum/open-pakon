//! Smoke test for PSI 2.4 Kodak KCDFS scene balance and SCP LUT generation.

use std::path::PathBuf;

use ndarray::Array3;
use pakon_oem::kcdfs::{default_dll, kcdfs_dir, roll_balance};
use pakon_oem::sba::{ANALYSIS_H, ANALYSIS_W};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

#[test]
fn kcdfs_loads_and_balances_synthetic_roll() {
    let root = repo();
    let dll = default_dll(&root);
    if !dll.is_file() {
        eprintln!("skipped: no {}", dll.display());
        return;
    }
    let dir = kcdfs_dir(&root);

    let frame_offsets: [i16; 5] = [-80, -30, 0, 40, 90];
    let frames: Vec<Array3<i16>> = frame_offsets
        .iter()
        .map(|&ev| {
            let mut img = Array3::<i16>::zeros((ANALYSIS_H, ANALYSIS_W, 3));
            for y in 0..ANALYSIS_H {
                for x in 0..ANALYSIS_W {
                    let delta = ((x as i16) - 24) * 12 + ((y as i16) - 16) * 8 + ev;
                    img[(y, x, 0)] = (600 + delta).clamp(200, 3800);
                    img[(y, x, 1)] = (1050 + delta).clamp(200, 3800);
                    img[(y, x, 2)] = (1300 + delta).clamp(200, 3800);
                }
            }
            img
        })
        .collect();

    let dmin = Some([350, 750, 1000]);

    let res_no_scp = roll_balance(&dll, &dir, &frames, dmin, false).expect("roll_balance(do_scp_pass=false)");
    assert_eq!(res_no_scp.shifts.len(), frames.len());
    assert!(res_no_scp.scp_luts.is_none());
    for (i, s) in res_no_scp.shifts.iter().enumerate() {
        for &v in s {
            assert!(
                v.is_finite() && (-2000.0..2000.0).contains(&v),
                "no-scp shift[{i}] out of range: {s:?}"
            );
        }
    }
    eprintln!("KCDFS shifts (do_scp_pass=false): {:?}", res_no_scp.shifts);

    let res_scp = roll_balance(&dll, &dir, &frames, dmin, true).expect("roll_balance(do_scp_pass=true)");
    assert_eq!(res_scp.shifts.len(), frames.len());
    for (i, s) in res_scp.shifts.iter().enumerate() {
        for &v in s {
            assert!(
                v.is_finite() && (-2000.0..2000.0).contains(&v),
                "scp shift[{i}] out of range: {s:?}"
            );
        }
    }
    eprintln!("KCDFS shifts (do_scp_pass=true):  {:?}", res_scp.shifts);

    let luts = res_scp.scp_luts.expect("scp_luts should be Some when do_scp_pass=true");
    let sample_idx = [0usize, 512, 1024, 2048, 3072, 4095];
    for (ch, name) in [(0, "R"), (1, "G"), (2, "B")] {
        for &v in luts[ch].iter() {
            assert!(v <= 4095, "SCP LUT channel {name} entry {v} > 4095");
        }
        let samples: Vec<(usize, u16)> = sample_idx.iter().map(|&idx| (idx, luts[ch][idx])).collect();
        eprintln!("SCP LUT {name} samples: {samples:?}");
    }

    // Verify single-frame balance_against(roll) matches full roll_balance.
    let single = pakon_oem::kcdfs::roll_balance_against(&dll, &dir, &frames[2..3], Some(&frames), dmin, true).unwrap();
    assert_eq!(single.shifts[0], res_scp.shifts[2], "single-frame balance_against should match full roll");
}

/// Ground truth from the real PSI 2.4 `PakonIMAu.dll` (PIKcdfsChainLoop(do_scp=1, pref_flag=0),
/// four 128x192 landscape thumbnails, dmin 467/804/1125, mindmin 162/401/607).
/// Regression: bStep1 must write the frame dmin through the pointer at setup_rec+4
/// (`*DAT_1030bf84`), otherwise every shift comes out ~+35..+60 too high.
#[test]
fn kcdfs_matches_real_dll_shifts() {
    use pakon_oem::kcdfs::Kcdfs;

    let root = repo();
    let dll = default_dll(&root);
    if !dll.is_file() {
        eprintln!("skipped: no {}", dll.display());
        return;
    }
    let data = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/psi24_ref_a");
    let (h, w) = (128usize, 192usize);
    let frames: Vec<Array3<i16>> = (1..=4)
        .map(|i| {
            let b = std::fs::read(data.join(format!("thumb{i:02}.raw"))).unwrap();
            let v: Vec<i16> = b.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]]) as i16).collect();
            Array3::from_shape_fn((h, w, 3), |(y, x, c)| v[c * h * w + y * w + x])
        })
        .collect();
    let want: Vec<i32> = std::fs::read(data.join("shifts_i32.bin"))
        .unwrap()
        .chunks(4)
        .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    let mut k = Kcdfs::new(&dll, &kcdfs_dir(&root), [162, 401, 607]).expect("Kcdfs::new");
    let res = k.balance_roll(&frames, [467, 804, 1125], true, false).expect("balance_roll");
    assert_eq!(res.shifts.len(), 4);
    for (f, s) in res.shifts.iter().enumerate() {
        for c in 0..3 {
            let d = (s[c] as i32 - want[f * 3 + c]).abs();
            assert!(d <= 2, "frame {f} ch {c}: ours {} real {}", s[c], want[f * 3 + c]);
        }
    }
}
