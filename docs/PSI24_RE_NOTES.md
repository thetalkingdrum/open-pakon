# PSI 2.4 Reverse-Engineering & Ground-Truth Notes

This document records the binary RE findings from `TLXClientDemo.exe`, `TLB.dll` (PSI 2.4), and `oem/psi24/PakonIMAu.dll`, plus pixel-level ground-truth measurements against a real PSI 2.4 / `TLXClientDemo` export (`/Volumes/windows sha/000001`).

---

## 1. Ground-Truth Validation (`/Volumes/windows sha/000001`)

Test strip: 4-frame C-41 roll scanned on F-135+ serial `17157` (`F135PLUS-17157-eeprom.zip`).
Available ground-truth artifacts from `TLXClientDemo` / PSI 2.4 (`tiff/real 2.4/`):
- `raw/10..13.raw` — 16-byte LE header `(16, 3000, 2000, 48)`, 3 planar `u16` channels (`R, G, B`).
- `rpd/10..13.raw` — 12-bit RPD planar `u16` files produced by `TLB.dll`.
- `10..13.tif` — final 8-bit sRGB TIFFs exported by real PSI 2.4 (`TLXClientDemo` at factory defaults).

### Verified stages
1. **`raw → RPD` is bit-accurate:**
   - Passing `raw/10.raw` through `colour::c41_lut()` + serial `17157`'s 3×10 EEPROM `negative_matrix` matches `rpd/10.raw` with `abs_mean = 0.14` RPD codes (out of 4095).
   - PSI 2.4 (`TLB.dll`) uses the exact same `c41_lut` + per-unit 3×10 EEPROM `NegMatrix` as PSI 3.
2. **`rpd_24.pf` vs `srgb.pf`:**
   - PSI 2.4's `Config/ColorCorrection/rpd.pf` (207,340 bytes, staged as `profiles/rpd_24.pf`) produces identical output under Perceptual, Relative Colorimetric, and BPC intents.
   - PSI 2.4 also ships a 173,948-byte 3D LUT `srgb.pf` (unlike PSI 3's 3,144-byte matrix `srgb.pf`), but `rpd_24.pf → PSI 2.4 srgb.pf` differs from `rpd_24.pf → standard sRGB` by only ~2–3 sRGB levels.

### Remaining difference (`open-pakon` PSI 2.4 vs `real 2.4`)
Comparing `tiff/2.4/frame_01..04.tif` (`open-pakon`) against `tiff/real 2.4/10..13.tif`:

| Frame | `real 2.4` mean RGB | `open24` mean RGB | `open30` mean RGB |
|---|---|---|---|
| 10 (`frame_01`) | `[96.4, 102.9, 108.5]` | `[107.9, 110.2, 111.6]` | `[121.1, 119.9, 119.0]` |
| 11 (`frame_02`, night flash) | `[73.4, 70.2, 71.0]` | `[94.7, 95.4, 96.1]` | `[60.6, 56.3, 54.3]` |
| 12 (`frame_03`) | `[66.0, 59.7, 59.3]` | `[100.1, 97.6, 96.8]` | `[67.2, 59.1, 54.1]` |
| 13 (`frame_04`) | `[90.3, 84.5, 82.1]` | `[117.7, 117.5, 116.6]` | `[79.5, 74.2, 69.5]` |

On frame 10 (`10.tif` vs `frame_01.tif`):
- `sky` (`y=200..400, x=2500..2800`): `real 2.4 = [144.7, 188.5, 217.4]` vs `open24 = [150.9, 185.2, 206.0]`
- `shadow` (`y=200..400, x=200..500`): `real 2.4 = [10.5, 10.6, 10.5]` vs `open24 = [35.7, 33.3, 31.2]`
- `center`: `real 2.4 = [229.6, 188.3, 176.2]` vs `open24 = [222.7, 177.9, 161.0]`

Empirical fit from `rpd/10..13.raw` → `10..13.tif` through `rpd_24.pf`:
- `real 2.4` exhibits the **same** per-channel `scp_luts` slope ratio (`R:G:B ≈ 0.92 : 1.00 : 1.05`) that `kcdfs.rs` produces (`LUT2048 = [1945, 2123, 2162]`).
- On top of `scp_luts`, `real 2.4` has:
  1. An overall **~1.3×–1.4× contrast / shadow-toe curve** (around `1550` RPD), which reduces MAE against `real 2.4` from `13.4` down to `~7.9` sRGB.
  2. **~100–170 RPD lower shifts** (`~[-160, -100, -60]` on frames 11–13), keeping night/flash shadows at ~10–15 sRGB instead of lifting them to ~35–65.

---

## 2. `TLXClientDemo.exe` Findings

Location: `~/Downloads/PAKON PSI 2.4/fx35install/program files/Pakon/F-X35 COM SERVER/TLXClientDemo.exe` (native 32-bit x86 MFC 7.1 Unicode app, `ImageBase 0x00400000`).

- **Sliders / `PutPictureColorSettings`:**
  - `CColorAdjustDlg` (`0x401580..0x401aef`) maps slider range `0..2000` to signed `[-1000, +1000]` centered at `0`.
  - All 6 settings (`Red`, `Green`, `Blue`, `Brightness`, `Contrast`, `Sharpness`) default to **`0`**. No hidden non-zero defaults are passed.
- **`OnSave` (`0x40af90..0x40ba75`) `iSaveControl` flags:**
  - Preloads `ebx = 0x1000` (`SAV_UseColorKcdfs`) at `0x40b2e0` (kept when `Image Size == 0` Original, overwritten by `0x1`/`0x2` for Display/Save size).
  - Default flags OR'd in: `0x4` (`SAV_UseCurrentRotation`), `0x10` (`SAV_UseColorCorrection`), `0x20` (`SAV_UseColorSceneBalance`), `0x40` (`SAV_UseColorAdjustments`), `0x80` (`SAV_UseScratchRemovalIfAvailable`), `0x400` (`SAV_FileHeader`).
  - Preview `SaveToClientMemory` calls (`0x407e58`, `0x4080d7`) hardcode `iSaveControl = 0x1bfd` (includes `SAV_UseColorKcdfs`).
- `Config/ColorCorrection/defaults.ini` has empty sections (`[1]..[123]`) with no per-film overrides.

---

## 3. `TLB.dll` (PSI 2.4) Disassembly Map

Location: `~/Downloads/PAKON PSI 2.4/fx35install/program files/Pakon/F-X35 COM SERVER/TLB.dll` (`ImageBase 0x10000000`).

### `PakonIMAu.dll` function table (`0x10012820..0x10012914`, global ptr `[0x100743a4]`)
- `[esi + 0x2c]` — `PIColorAdjustPlanar_8` (string `0x1005cfbc`)
- `[esi + 0x38]` — `PIColorCorrectColNegPlanarScan` (`0x1005cf64`)
- `[esi + 0x40]` — `PIColorCorrectColNegPlanarSave` (`0x1005cf44`)
- `[esi + 0x44]` — `PIColorCorrectColRevPlanar` (`0x1005cf08`)
- `[esi + 0x48]` — `PIColorSceneBalanceColRevPlanar` (`0x1005cee8`)
- `[esi + 0x4c]` — `PIColorSceneBalanceColNegPlanar` (`0x1005cf24`)
- `[esi + 0x50]` — `PIKcdfsStartNewRoll` (`0x1005ced4`)
- `[esi + 0x54]` — `PIKcdfsAddThumbNail` (`0x1005cec0`)
- `[esi + 0x58]` — `PIKcdfsChainLoop` (`0x1005ceac`)
- `[esi + 0x5c]` — `PIKcdfsGetShift_R` (`0x1005ce98`)
- `[esi + 0x60]` — `PIKcdfsGetShift_G` (`0x1005ce84`)
- `[esi + 0x64]` — `PIKcdfsGetShift_B` (`0x1005ce70`)

### KCDFS Roll Balance Runner (`0x10034550..0x1003497f`)
- `0x10034564`: sets `ebp = 1` (`do_scp = 1`).
- `0x100345f7`: calls `0x1000d7b0` to compute `dmin` (`[esp+0x2c]`, `[esp+0x34]`, `[esp+0x3c]`).
- `0x1003468c..0x10034697`: calls `PIKcdfsStartNewRoll(roll_obj + 0xca8, dmin_r, dmin_g, dmin_b)`. `roll_obj + 0xca8` holds the 3×4096 `u16` SCP LUTs.
- `0x100347d9`: builds `0x4018` thumbnails via `0x10028000` and calls `PIKcdfsAddThumbNail` (up to 40 frames).
- `0x10034833..0x10034853`: reads `pref_flag` from `[edx + 0x4ac] == 1` and calls `PIKcdfsChainLoop(do_scp=1, pref_flag)`.
- `0x10034879..0x100348b5`: reads `PIKcdfsGetShift_{R,G,B}` into `[frame + 0x68..]` via `0x1000c4a0/d0/500`.
- `0x10034913..0x1003491a`: calls `PIKcdfsStartNewRoll(roll_obj + 0xca8, ...)` again after reading shifts.

### Color Correction / Save Dispatch (`0x10023bb0..0x10023ec0`)
Called from `0x10027e75` and `0x10028606`, receiving `roll_obj + 0xca8` (`scp_luts`), `[frame + 0x68]` (shifts + adjustments), and `SAV_UseColorAdjustments` (`edx & 0x40`).
- Jump table at `0x10023f64` (`[0x10023c20, 0x10023d94, 0x10023eb1]`), case map `1..8` at `0x10023f70`: `[0, 1, 2, 0, 2, 2, 2, 0]`.
- `film = 1` (C-41 color negative) takes case `0` (`0x10023c20`), packs struct `esp + 0x58..0xc4` including `scp_luts` at `esp + 0xb8/bc/c0`, and calls **`[ecx + 0x44]`** (`0x10023d89`).

---

## 4. `oem/psi24/PakonIMAu.dll` Notes & Next Step

- **`CiColorCorrectionKcdfs` (`0x10001410..0x10001ed0`):**
  - `bStep1` (`0x10001940`): `kcdfs.rs` was fixed in `afd8147` to match `createAlgData` + `_sbaCalcDens` exact stack args (`dmin_a` preserved for `_sbaCalcDens`, `mat_buf` passed as arg 8).
  - `bStep2` (`0x10001c20`): calls `SbaCalcFosResults` (`0x1022bb20`), `Sba(..., mode=2, ...)` (`0x102286c0`), and either `SbaBuildSCPLuts` (`0x1022b250`) or `Preference` (`0x1022ad90`).
  - `PIKcdfsGetShift_{R,G,B}` (`0x10001e50/80/b0`) read `i16` at `[frame + 0x39c6/c8/ca]`.
- **Where to look next for the ~1.4× contrast + ~-150 black-point difference:**
  - Inspect `0x10068c30` (called by `PIColorCorrectColRevPlanar` `0x10005450` and `PIColorSceneBalanceColNegPlanar` `0x10005860`) and the struct-based `0x10023d89` target in `oem/psi24/PakonIMAu.dll` to see if `PakonIMAu.dll` applies a contrast/toe curve, `lut1b.00` / `lut2p.00`, or black-point adjustment during final RPD → sRGB rendering when KCDFS is active.
