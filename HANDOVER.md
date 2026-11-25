# Handover: `psi24-color-engine` Branch

This file summarizes the work done on `psi24-color-engine` for continuing on the scanner-connected machine. *(Delete `HANDOVER.md` before opening an upstream PR to `jorshhh/open-pakon`.)*

## Git & Build Setup

- **Fork (`origin`):** `https://github.com/thetalkingdrum/open-pakon.git`
- **Upstream (`upstream`):** `https://github.com/jorshhh/open-pakon`
- **Active branch:** `psi24-color-engine`

On macOS (with Homebrew `rustup` / `lcms2`):
```bash
PATH="/opt/homebrew/opt/rustup/bin:/opt/homebrew/bin:$PATH" cargo test --offline --manifest-path rust/Cargo.toml
PATH="/opt/homebrew/opt/rustup/bin:/opt/homebrew/bin:$PATH" cargo build --offline --release --manifest-path rust/Cargo.toml --bin pakon_app
```

---

## What Was Built

1. **PSI 2.4 Color Engine Mode & `.pakscan` Loader (`3cb7c6a`)**
   - Added `ColorEngine::{Psi3, Psi24}` across `pakon-roll`, `pakon-web`, and the Vue UI (**Adjust** view: `Colour: PSI 3 / PSI 2.4` dropdown).
   - Staged `profiles/rpd_24.pf` and baked `rust/crates/pakon-image/assets/rpd_24.srgb52.u8` (52³ table).
   - Added zero-copy `Mmap` loader for `PKLB` v1 `.pakscan` rolls in `rust/crates/pakon-image/src/ribbon.rs`.

2. **Real PSI 2.4 Kodak KCDFS Under Unicorn (`f400912`)**
   - Staged PSI 2.4 `oem/psi24/PakonIMAu.dll` and `oem/psi24/KCDFS/*` (`kcdfs.su`, `dls_1.7.pcode`, `lut1b.00`, `lut2p.00`, etc.).
   - Implemented `rust/crates/pakon-oem/src/kcdfs.rs` to run PSI 2.4's pure-C KCDFS core (`createAlgData`, `_sbaCalcDens`, `Sba`, `SbaCalcFosResults`, `SbaBuildSCPLuts`, `Preference`) in 32-bit x86 Unicorn.
   - Wired `ColorEngine::Psi24` in `rust/crates/pakon-roll/src/lib.rs` to apply KCDFS's 4096-entry per-channel SCP LUTs + Stage-2 shifts via `colour::render_oem_scp` in `pakon-image/src/colour.rs` (falling back to Ansel `sba-CN-psi24.dpi` if `oem/psi24/` is unavailable).

3. **TLX Planar `.raw` Roll Loader & KCDFS Fixes (`afd8147`)**
   - Added `rust/crates/pakon-image/src/tlx.rs`: detects TLXClientDemo planar 16-bit `.raw` files (16-byte LE header `(16, width, height, 48)` + 3 `R, G, B` planes), loads all matching `.raw` files in the folder in natural order, pre-orients them, and scales 14-bit linear transmittance to `open-pakon`'s `raw16` convention (`v * 4 + 300`).
   - Updated `prescan` (`pakon-roll/src/lib.rs`) and `pakon-web/src/jobs.rs` (`Meta::preframed`) to skip strip framing and ICE (since TLX `.raw` files are already flat-fielded, cropped, and ICE-cleaned) and populate exact frame boxes immediately.
   - Fixed KCDFS `bStep1` arguments (`createAlgData` / `_sbaCalcDens`) and added `estimate_roll_dmin` (0.5th percentile across roll thumbnails) when `scene_dmin` is `None` (fixes blue cast on rolls without a measured leader Dmin).
   - Enabled offline `NegMatrix` fallback in `Env::oem_path_for` for both `Psi3` and `Psi24`, plus a fallback `rpd_balance` guard in `Renderer::new`.

---

## Important Notes & Next Steps on the Scanner Machine

1. **Connect the Pakon scanner to archive its EEPROM:**
   - When `open-pakon` sees a warm scanner, it archives the unit's EEPROM (including its real 3×10 `NegMatrix`) into `~/Library/Application Support/Pakon/eeprom/` (or `backups/eeprom/`).
   - Once archived, **both** PSI 3 and PSI 2.4 automatically use the scanner's real `NegMatrix` (even when reopening offline TLX `.raw` or `.pakscan` rolls later).
   - *Note:* When offline with **no** EEPROM archive, `Env::oem_path_for` currently falls back to `colour::psi24_neg_matrix_default()` (based on F-235's `_ClientColNegMat.txt` linear part ÷ 4 + F-135 baseline intercepts). Consider replacing that offline fallback with a real F-135/F-135+ EEPROM `NegMatrix` from `test/fixtures/eeprom/` if you need offline accuracy without an EEPROM archive.

2. **Test live scanning + A/B comparison:**
   - Run a live scan on the scanner machine and compare **PSI 3 (Ansel)** vs **PSI 2.4 (KCDFS)** exports (16-bit TIFF and JPEG).
   - Re-test the TLX `.raw` rolls (`RAW max 800`, `yashica T5 raw with header`) once the real scanner EEPROM is archived.
