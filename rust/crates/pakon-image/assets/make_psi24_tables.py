#!/usr/bin/env python3
"""Regenerate the PSI 2.4 EBP colour tables (needs `imagecodecs` and the OEM
install's Config/ColorCorrection folder for srgb.pf, which the repo does not
keep).  Both are 65^3 grids, u16 little-endian, R-major, 16-bit in / 16-bit out,
LittleCMS intent 0:
  rpd_24.romm65.u16    rpd.pf (== profiles/rpd_24.pf) -> romm.pf   (12-bit RPD in, ROMM out)
  romm.kodaksrgb65.u16 romm.pf -> Kodak srgb.pf
usage: make_psi24_tables.py <ColorCorrection dir> <profiles dir>
"""
import sys
import numpy as np
import imagecodecs as ic

N = 65
cc, prof = sys.argv[1], sys.argv[2]
rp = open(f"{prof}/rpd_24.pf", "rb").read()
rm = open(f"{prof}/romm.pf", "rb").read()
ks = open(f"{cc}/srgb.pf", "rb").read()
g = np.rint(np.arange(N) * 65535 / (N - 1)).astype(np.uint16)
G = np.ascontiguousarray(np.stack(np.meshgrid(g, g, g, indexing="ij"), -1).reshape(1, -1, 3))
for name, a, b in (("rpd_24.romm65.u16", rp, rm), ("romm.kodaksrgb65.u16", rm, ks)):
    t = ic.cms_transform(G, a, b, colorspace="rgb", outcolorspace="rgb", intent=0, outdtype="uint16")
    t.astype("<u2").tofile(name)

# cmm65.u16 is not made here: it is the real Kodak CMM (`SpCombineXforms(rpd.pf, srgb.pf)` + `SpEvaluate`,
# output type 5 = u16 full range) sampled on a 65^3 grid with input levels round(i*4095/64) by the
# Windows harness in psi24-cmm (Task 4, `out/C65_u16_type5_16bit.bin`, planar R fastest) and
# transposed to R-major (r, g, b, channel). Input 12-bit cmm_in (SCP LUT + shifts), output 0..65535.
