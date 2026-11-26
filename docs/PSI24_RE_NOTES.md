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
_Corrected: the loader stores each `GetProcAddress` result one call after its push, so earlier slot names for `+0x38..+0x4c` were off by one. Slots `+0x2c` and `+0x50` and above were not re-checked and may be off by one too._
- `[esi + 0x2c]` — `PIColorAdjustPlanar_8` (string `0x1005cfbc`)
- `[esi + 0x38]` — the `0x1005cf84` name (not identified)
- `[esi + 0x3c]` — `PIColorCorrectColNegPlanarScan` (`0x1005cf64`)
- `[esi + 0x40]` — `PIColorCorrectColNegPlanarSave` (`0x1005cf44`)
- `[esi + 0x44]` — `PIColorSceneBalanceColNegPlanar` (`0x1005cf24`)
- `[esi + 0x48]` — `PIColorCorrectColRevPlanar` (`0x1005cf08`)
- `[esi + 0x4c]` — `PIColorSceneBalanceColRevPlanar` (`0x1005cee8`)
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
- **Correction:** `0x10068c30` is a `std::basic_string` helper, not a colour curve. The C-41 path is `PIColorSceneBalanceColNegPlanar` (`0x10005860`): `p+0x6c = (film != 1)` and C-41 takes the EnhancedBlackPrinting (EBP) branch.
- **Pipeline there:** baked SCP LUT + shift (`FUN_10001fe0`: `out = clamp(lut[i] + shift, 0, 4095)`, same as our port) -> `ImaEBPOperation` -> contrast (`1 + p[9]/1000`, 1.0 at default sliders) -> ICC combine/colour conversion.
- **EBP chain order:** Gain and Offset -> Gamma -> Noise Filter -> Black Print, all attributes at constructor defaults (only `padValue` is set). The `FUN_10166010` method once read as a noise filter is Black Print.
- **Result:** Gain and Offset (per-channel auto-levels, ~0.1 % clip, about 1.14x gain, black point 27 -> 0) is the missing contrast and toe. On a display-referred 12-bit signal it cuts MAE vs real from 15.3 to 5.9, and with Gamma (gamma ~0.95) to about 5.1. EBP on the RPD domain is wrong (crushes the image).
- **Still open:** the warm cast (it is upstream of EBP, tone-dependent, in SCP/shift/table), the real shifts, the exact RPD -> 12-bit ROMM conversion, and the Noise Filter code.
- **Details and scripts:** `~/pakon-re/psi24-ghidra/` (`EBP_FINDINGS.md`, `EBP_VALIDATION.md`, `ebp_sketch.py`, Ghidra project and decompiles).


---

## 5. EBP chain, bit-level findings (validated against the real DLL)

Validated with a Windows host that runs the real `PakonIMAu.dll` offline (hooks, whole-image stage dumps, synthetic probes). Code: `rust/crates/pakon-image/src/ebp.rs`, `psi24.rs`; examples `psi24_probe`, `psi24_stage`, `psi24_curve`, `psi24_pt`.

**Domain.** The C-41 EBP runs on **8-bit planar sRGB** (after rpd -> romm -> srgb), using the `uchar` ("E") template instances. The 12-bit "G" instances (`0x10183e40`, `0x10180dc0`) are never called. Plain 8-bit defaults: black 0.10 %, white 0.15 %, shadow/highlight 0.06 (thresholds 5/250), Gamma meanLow 39.06, meanHigh 48.82, citras 0.5; Black Print luma 0.5, chroma 0.25, luma max 50, chroma max 20, ratio 0.4. Order: Gain&Offset -> Gamma -> Noise Filter -> Black Print.

**Gain & Offset.** Histogram is the image blurred with horizontal then vertical `[1 4 6 4 1]`, each pass `>>4` (truncating): bit-exact against dumped histograms. Percentiles give lo/hi; `whiteOffsetMethod=1` with the bVar2 branch (hi_R > avg -> `((hi_c+hi_R)/2+255)/2`) and the knee apply. A channel is empty only when `hi <= lo`. The double at struct `+0x60` is the average gain (765/sum r); the Gamma-builder `+0x10` double is k1 x that gain. Ours is 98.6 % / 99.9 % exact on the real s0.

**Gamma.** 8-bit tables: `lutL[i] = floor(255*(i/255)^g + 0.5)` and `gainL` (Q12). Per pixel: `L=(R+G+B+1)/3`, `c' = lutL[L] + trunc_toward_zero((c-L)*gainL[L]/4096)` (92 % exact against the real tables). The estimator uses 8-bit floor blurs (`>>4` for the 5-tap, `>>2` for the 3-tap) and lands within about 0.01 of the DLL's gamma. It is a histogram statistic, not spatial.

**Black Print.** 3x3 `[1 1 1;1 8 1;1 1 1]/16` blur, M = mean over all samples, lut1/lut2 in Q12. Matches the real stage 99.1 % / 95.8 % exact.

**Noise Filter (`ImaEBPNoiseFilterOp<uchar>`, `0x1016c830`, vtable `0x10284a04`; coring 20, boost 2.0, size 10).** Active, **not decoded**. Ours is an empirical model: per channel `out = box3(c) + F_c(hp)` with `hp = Y - box3(Y)`, Y=(R+G+B)/3, F_c fitted tables (49 entries, hp -24..24); the offset part is weighted by min(1, activity/0.3). This stage is what cuts chroma, so skipping it oversaturates.

**Pre-EBP conversion.** Our `rpd -> romm -> srgb` tables are 2-4 levels low in highlights and 1-3 high in shadows versus the DLL's s0. A fitted per-channel 1D correction is applied (G/B fade over the last 7 levels so the bVar2 branch does not flip). Root cause not found.

**Results vs the DLL with real shifts (same input).** Frame 1 MAE 1.0, frame 2 1.2, frame 3 1.3, frame 4 1.5 (variant A); frame 2 variant B 2.5. Probe suite mean MAE 1.6 (was 12-23). Whole EBP from the real s0: MAE 0.8-0.9.

**Open.** Decode the real Noise Filter; find the table difference behind the pre-EBP correction (currently fitted on one film); probe `P3a_1500_3000` M4 (MAE 9); frame 2 variant B bias.
