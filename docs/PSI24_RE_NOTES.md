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

**Noise Filter (`ImaEBPNoiseFilterOp<uchar>`, exec `0x1016c830`, vtable `0x10284a04`; attributes +0xc4 = 20.0, +0xc8 = 2.0, +0xcc = 10.0): decoded.** Ported in `ebp.rs::noise_filter`; 98.4-99.6 % of samples exact against the real s2 -> s3 dumps of four frames (the remaining ~0.5 % sit on strong edges, cause not found). It is image-adaptive:
1. Noise estimate `nv` (stored as float at struct +0xe4): `sum((d*d+32)>>6) / (n*64-64)` over the interior, d = 3x3 Laplacian (8*c - sum of 8 neighbours) of R - B. Matches the dumped values to ~0.1 % (the exact loop bounds are not pinned down).
2. Y = (R+G+B)/3; if nv > 0.25, Y is blurred with [1 1 1;1 8 1;1 1 1]>>4 (truncating, edges replicated).
3. hp3 = Y - lp3(Y) + 128, hp5 = Y - lp5(Y) + 128 (byte): lp3 = [1 3 1]/5 per axis, lp5 = [1 4 6 4 1]>>4 per axis, every pass truncated (`0x10170b90`).
4. Y' = Y + trunc(core(hp3)*A + .5) + trunc(core(hp5)*B + .5), soft coring core(d) = d-X for d >= X, d+X for d < -X, else 0; X = min(trunc(20 nv + .5), 10), A = min(.4 + 2 nv, 1.5), B = min(.2 + 2 nv, 1).
5. C1 = (2G-R-B)/4 + 128, C2 = (B-R)/2 + 128 (C division) from the *unblurred* RGB, blurred with the 3x3 kernel of step 2, then smoothed by m = trunc(10 nv + .5): m < 3 [1 3 1]/5, m = 3 binomial 3 >> 2, m 4-5 binomial 5 >> 4, m >= 6 binomial 9 >> 8 (horizontal then vertical, each truncated, edges replicated; `0x101722a0`, `0x101728b0`).
6. R = Y' - (4 c1 + 3)/6 - c2, G = Y' + (8 c1 + 3)/6, B = Y' - (4 c1 + 3)/6 + c2, clamped (c1, c2 = chroma - 128).
Real values seen: nv 0.338 / 0.480 / 0.265 / 0.603 on frames 1 / 4 / 2 / 3 (X 7/10/5/10, m 3/5/3/6). Not modelled: the DLL's first column/row of the 5-tap filter weighs 12:3:1 instead of 11:4:1 (no measurable effect). This is what cut chroma; the earlier fitted tables are gone.

**Pre-EBP conversion.** Our `rpd -> romm -> srgb` tables are 2-4 levels low in highlights and 1-3 high in shadows versus the DLL's s0. A fitted per-channel 1D correction is applied (G/B fade over the last 7 levels so the bVar2 branch does not flip). Root cause not found.

**Results vs the DLL with real shifts (same input).** Frame 1 MAE 0.87, frame 2 1.07, frame 3 0.78, frame 4 1.23 (variant A); frame 2 variant B 2.3. Whole EBP from the real s0: MAE 0.5-0.9. Probe suite mean MAE 1.6 (was 12-23). Whole EBP from the real s0: MAE 0.8-0.9.

**Pre-EBP refit (done, after the exact NF).** s0 is *upstream* of the NF, so the exact NF does not change what PRE_* has to match; the old table was already close. Findings:
- The per-frame correction curves (real s0 - our unrounded srgb, as a function of our level) agree across all four frames to ~0.5 level, so one 1D table per channel is right to first order. The remaining s0 error (MAE ~0.35-0.55) is cross-channel (the real stage is a 3D table), not fixable with 1D curves.
- A least-squares / per-pixel fit lowers s0 MAE by ~20 % but makes the end-to-end *worse* (frames 1/3: 0.87/0.78 -> 1.4/1.8). The end-to-end error is dominated by the discrete Gain & Offset percentiles (`lo` of G/B off by one level changes `p`), so the right objective is **histogram (quantile) matching**: correction(v) = q_real(F_ours(v)) - v, averaged over all four frames. That is what PRE_* now holds. It also needs the correction applied to the *unrounded* sRGB value (`PSI24_DUMP_PREF=<f32 planar>` dumps it).
- End to end (variant A, frames 1/2/3/4): 0.87/1.07/0.78/1.23 -> **0.84/1.12/0.79/1.14**; frame 2 variant B 2.34 -> 1.89; probe suite M3 1.69 -> 1.66, M4 1.41 -> 1.41. Frame 4 bias is still +1.0 (its `lo` R/G are 1 level low, gamma est 0.717 vs real 0.725).
- Held-out check: coordinate descent directly on end-to-end MAE overfits (fit on frames 1+4: 0.65/0.61 in sample, but 1.44/1.88 on frames 2/3; fit on 2+3 breaks probe suite M4, 10.2), so it was not used. Pooled quantile matching is the only variant that was never worse than the old table on any set.
- Direct `rpd_24.pf -> srgb.pf` without the 12-bit ROMM round trip (unrounded float into srgb.pf): differs from the 12-bit path by only 0.04 / 0.02 / 0.02 levels (R/G/B), so it does not remove the need for PRE_*; the 2-4 level offset is a real difference between our tables and the DLL's (3D/ cross-channel, or the SCP/shift stage), not quantisation.

**Open.** Find the table difference behind the pre-EBP correction (the 3D residual); probe `P3a_1500_3000` M4 (MAE 9); frame 2 variant B bias; frame 4 `lo` off by one.

## 6. High-bit-depth path (`psi24::render_hq`, `render_hq_f32`)

Opt-in; the bit-exact 8-bit `psi24::render` is unchanged and stays the validation reference. `render_hq` returns u16 (`v * 257`), `render_hq_f32` returns sRGB on a 0..255 float scale.
- **Decisions are exact.** The 8-bit chain runs once on a clone (`ebp::hq_params`) and records G&O (`off`, `s`, knee `l2/h2/tl/th/k1/k2`, `p`), the Gamma exponent, the Noise Filter `nv` (after Gamma) and the Black Print mean (after the NF). `ebp::hq_apply` then applies the same stages continuously on float planes (G&O without floor, `255*(L/255)^g` and chroma gain per pixel, NF with float blurs/soft coring on the 0..255 scale, Black Print weights unrounded). No quantisation between stages.
- **Front end.** `make_psi24_tables.py` writes `<u2` and `Table::from_u16_le` interpolates in f64, so rpd -> romm -> srgb is kept at 16 bit and float; the 12-bit ROMM rounding is skipped and PRE_R/G/B is applied as the same continuous interpolated curve (no 8-bit rounding). The DLL's 12-bit 'G' path is not used.
- **Bias / per-stage loss model.** A plain float chain sits +0.5 (R) / +2.3 (G, B) levels above the exact 8-bit chain on all four frames (and +1.8 .. +3.6 against the DLL on G/B of variant A): the DLL's floors/truncations lose that much on average. Instead of a fixed end-of-chain constant (first version, `HQ_BIAS`), `ebp::HQ_LOSS = [0.5, 1.0, 0.75, 0]` models it per stage: `go_hq` subtracts 0.5 only in the stage-1-only case (the knee LUT rounds); `noise_filter_hq` weights by texture `wt = min(nv/0.1, 1)` (flat patches lose 0 under truncating blurs) and subtracts `wt*(1/3 + [nv>0.25] 15/32)` from Y and `wt*0.75*(15/32 + 2*k_loss(m))` from u and v (`k_loss` = 0.4 / 3/8 / 15/32 / 255/512 for m <= 2 / 3 / 4-5 / >= 6) before the RGB rebuild, ahead of Black Print. The recipe was checked on frames and probes; the only change from it is the chroma scale 0.75 (1.0 gave +0.03 mean MAE; the optimum is flat from 0.5 to 1.0, luma scale flat 0.9-1.1, floor 0.35-0.65). Tuning knobs: `PSI24_HQ_K=floor,luma,chroma,residual`; `PSI24_HQ_BIAS=0` turns the model off (pure unbiased float).
- **PRE smoothing (HQ only).** The PRE_R/G/B tables have a sawtooth in knots 5-9 (histogram quantile matching on a coarse grid). The HQ float path applies a `[1 6 1]/8` smoothed copy; the statistics path (decisions) keeps the raw table, the 8-bit path is unchanged. `[1 2 1]/4` made frame 4 worse (variant A MAE 1.55 -> 1.73), `[1 6 1]/8` costs +0.02 mean MAE on variant A frames (1.172 -> 1.19; frame 4 1.55 -> 1.64) and is neutral on the probes (`PSI24_HQ_NOSMOOTH=1` restores the raw table).
- **Results vs the real DLL (8-bit quantised HQ vs exact 8-bit path, MAE / signed bias R,G,B).** Variant A frames 1-4: 0.84 / 1.12 / 0.79 / 1.14 (exact, bias -0.1/-0.5/-0.4, -0.7/-0.9/-1.0, -0.5/-0.5/-0.4, +1.0/+1.1/+0.7) -> **1.26 / 1.33 / 1.15 / 1.59** (HQ, bias -0.5/-0.4/-0.5, -0.2/-1.0/-1.0, -0.8/-0.5/-0.3, +1.1/+1.2/+0.6). Without the bias constant the HQ MAE is 1.76 / 1.53 / 1.79 / 2.93 (G, B +1.8 .. +3.5 high). Probe suite (mean of 61 renders): M4 1.42 -> 2.33 (2.20 without the constant), M3 2.86 -> 3.27 (3.19). The remaining +0.2..0.45 MAE on frames is the integer rounding noise of the DLL that a float path cannot reproduce; probes are synthetic flats/ramps where the integer quirks matter more.
- Unit test `psi24::tests::hq_stays_close_to_the_8bit_path_and_has_no_banding`: HQ vs 8-bit MAE/bias on a synthetic gradient, and many more distinct 16-bit codes than the 8-bit path on a smooth row. `PSI24_HQ=1` switches `psi24_probe` / `psi24_ref` to HQ (output quantised to 8 bit for the comparison).

**HQ numbers with the loss model** (MAE, variant A frames 1-4 / frame 2 variant B / probe suite M3, M4): exact 0.83 / 1.10 / 0.74 / 1.12, B 1.87, 1.66 / 1.41; HQ with model 1.06 / 1.11 / 0.95 / 1.64, B 1.81, 1.69 / 1.70; pure float (`PSI24_HQ_BIAS=0`) 1.75 / 1.54 / 1.79 / 3.00, B 1.15, 1.53 / 1.70. Signed bias R,G,B of HQ on variant A frames 1-4: +0.3 +0.3 +0.7 / -0.3 -0.8 -0.4 / -0.3 -0.7 +0.3 / +1.4 +1.3 +1.3 (exact: -0.1 -0.5 -0.4 / -0.7 -0.9 -1.0 / -0.5 -0.5 -0.4 / +1.0 +1.1 +0.8). Frame 4 stays the worst (its G&O `lo` is off by one, see above).

### Exact fixes found by disassembly (Neon review #3), all verified on the stage dumps
- **Black Print** rounds `LT = round(0.5 M)` and `CT = round(0.25 M)` (`+0.5` before `ftol`, 0x101663ec / 0x1016643a); truncating gave frame 3 only 82 % exact, rounding gives 100 % (frames 1, 3, 4 all 100 %). `M` is summed over all tiles on the x87 stack and never stored in `this`.
- **Gamma estimator** histogram: luma `<< 7`, 5-tap blurs in Q7, one rounding when binning = `(binom5(l8) + 128) >> 8`; reproduces the DLL's gamma (Gamma struct `+0x58`) on all four frames (frame 3 0.6236).
- **Gain & Offset `lo2`** window is centred, `h2[b-1] + h2[b] + h2[b+1]` (the trailing window was a port bug); G&O is now 100 % exact on frames 2 and 3, 99.9 % on frame 4, 98.6 % on frame 1.
- Whole EBP from the real s0: MAE 0.13 / 0.16 / 0.20 / 0.17 (frames 1, 4, 2, 3), 82-89 % exact, bias ~0. Probe suite M3 1.53, M4 1.39.
- Open: the HQ `HQ_LOSS` G&O floor term (0.5). The DLL rounds in stage 1 as well, so the term looks wrong in principle, but removing it makes HQ frame 4 worse (1.02 -> 1.39) and the probe suite M3 better (1.68 -> 1.54); refit after the CMM tables are replaced.

## The pre-EBP colour step is now the sampled Kodak CMM (Task 4)

The Windows harness (`psi24-cmm`) calls the DLL's own `SpCombineXforms(rpd.pf, srgb.pf)` + `SpEvaluate` on our grids
(cross-check: all 3440 s0 pairs from the grid capture reproduced exactly). `SpEvaluate` output types: 1 = u8, 4 = u16 12-bit
range, 5 = u16 full range (0, 2, 3, 7, 8 rejected). Type 5 on a 65^3 grid (input `round(i*4095/64)`) is stored as
`assets/cmm65.u16` (R-major) and used as the single cmm_in -> sRGB table (`psi24::Tables::cmm`), replacing the
LittleCMS rpd -> romm -> srgb pair and the fitted `PRE_R/G/B` (still reachable with `PSI24_OLD_TABLES=1`).
- On the 3440 exact `cmm_in` -> `s0` pairs: MAE 0.19/0.27/0.31 (old pair 1.75/1.18/1.08), 83-89 % exact after rounding,
  bias +0.08/+0.09/+0.19 (old -1.47/-0.05/+0.32).
- Interpolation: trilinear and tetrahedral from the 65^3 grid agree to <0.1 u8 level in range (max 0.32); along 1-D ramps the
  worst case is 1.3 levels (at the steep clip knee). The DLL's own interpolation is not known.
- The transform clips hard (grey reaches 255 near input 2600), with gamut clipping to 0: 65 nodes are poor only across those kinks.
- The u8 output is not `round(u16/257)` (92 % match); we use the 16-bit table and round once.
- Probe suite (21 probes) M3/M4 mean MAE: 1.53/1.39 -> 0.77/0.70 (P3a_1500_3000 M4 9.66 -> 1.17). HQ: 1.68/1.65 -> 1.22/1.09.
- Real frames end to end (variant A, exact path) MAE 1 / 2 / 3 / 4: 0.75 / 0.86 / 0.57 / 0.94 -> 0.60 / 0.64 / 0.66 / 1.00.
  Pre-EBP s0 against the real dumps shows +0.4..+0.6 bias on frames 2-4 (shadows +0.7..+1.0): a constant -2 (12-bit) on cmm_in
  removes it (frames 2/3/4 MAE 0.65/0.69/0.69 -> 0.61/0.55/0.65, frame 1 prefers 0). Shifts enter exactly as `clip(clip(LUT)+shift)` (scan of a
  scale factor on the shift has a sharp minimum at 1.0). Origin of the ~2 level offset is not known; not applied.
- HQ_LOSS was tuned against the old tables (on the new ones HQ frames 2/3 showed -1.0..-1.4 bias, MAE 1.20/1.22). Refit by
  coordinate descent on the mean MAE of HQ against our own exact 8-bit path over frames 1-4 + 8 probes (M4): `[0.5,1.0,0.75,0]` ->
  `[0.25,0.8,0.825,0.2]`, mean MAE 0.889 -> 0.838. Against the real DLL, HQ frames 1/2/3/4 MAE 0.89/1.20/1.22/0.99 -> 0.89/1.00/1.03/1.14
  (mean 1.075 -> 1.015); probe suite HQ M3/M4 1.22/1.09 -> 1.19/1.09. The surface is flat near the optimum and frames still disagree
  (frames 2/3, both with the gamma clamped at 0.7, stay -0.6..-1.2 low in G, frame 4 is +0.2..+0.6): a content-dependent loss is
  left, not a constant one. Fitted on 12 images, no held-out set yet (the second film stock will be one).

## Second roll (Task 3, Base 16, 3000x2000): held-out check and the Gamma g1 branch

New roll, 8 frames, real DLL stage dumps (`psi24-film2.zip`). Base 16 frames are 3000x2000; the port needs no change except the size (`PSI24_W/PSI24_H` for `psi24_probe`/`psi24_stage`).

- Gamma struct (`this` after build): `+0x50` = g1, `+0x58` = g2, `+0x60` = the exponent used (blend), `+0x68` = trimmed mean. `+0x10` is NOT the exponent.
- g2 and the blend were already exact on all frames (0.7112, 0.8047, 0.9323, 0.9174, 0.9889 reproduced to 4 digits).
- g1 was wrong on bright frames: the old edge rule `trunc(0.75*sigma)` gave g1 1.09 where the DLL has 0.957 (frame 11), 1.057 vs 0.796 (frame 12). The old roll never reached this branch. Edge pixels `|binom3-binom5| > k*sigma`, continuous (no truncation), fit all five real g1 for `k` in 0.87..1.02; `EDGE_THR = 1.0`. Residual g1 error +0.001..+0.019 (frame 17 largest), so the rule is close but probably not exactly the DLL's.
- Result with auto gamma (exact path, MAE vs real look): frames 3/7/10/11/12/15/17/21 = 0.71/0.80/0.69/1.02/0.46/1.22/1.71/0.85; HQ 1.29/1.12/1.22/1.47/1.06/1.43/1.59/1.07. Before the g1 fix frames 11/15/17/21 were 8.8/1.6/1.6/3.1.
- No regression on roll 1 (probe suite exact 0.77/0.68, HQ 1.19/1.07).
- Not reached: `nv <= 0.25` (lowest nv on this roll 0.294); KCDFS shifts only valid for frames 1..25 of a 37-frame roll.

## Review #4 applied (u8 CMM table, analytic HQ Noise Filter loss), checked on two held-out rolls

Data: roll A (`psi24-film2`, 8 frames) and roll B (`psi24-film2b`, 8 frames, different KCDFS LUT/shifts), Base 16. Auto gamma is within 0.01 of the DLL's `+0x60` on all 16 frames (g1 rule above).

| | suite M3/M4 | roll A | roll B |
|---|---|---|---|
| exact, type-5 table (before) | 0.77 / 0.68 | 0.932 | 1.008 |
| exact, u8 table (type 1) | 0.62 / 0.63 | 0.926 | 0.975 |
| exact, u8 + node-position warp | 0.71 / 0.66 | 0.941 | 1.000 (rejected) |
| HQ, fitted `HQ_LOSS` | 1.05 / 1.24 | 1.292 | 1.351 |
| HQ, analytic NF loss, `HQ_LOSS=[0,1,1,0]` | 0.98 / 1.05 | 1.009 | 1.069 |

- Review's claim that the -2 input offset does not exist: not re-verified here; the exact path no longer shows it as a problem after the u8 table.
- The Kodak transform is not monotonic near black on the neutral axis (the test does not assume it).
- NF model (from the C code): chroma `/4`, `/2` truncate toward zero (pull toward 0 by 0.375 / 0.25 before the blur); `(4u+3)/6`, `(8u+3)/6` add +1 when u < 0; luma and chroma blur floors at full strength. No fitted constants; the G&O floor and residual bias terms are 0.
