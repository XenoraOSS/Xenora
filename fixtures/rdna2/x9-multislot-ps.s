// SPDX-License-Identifier: GPL-3.0-or-later
// Original synthetic two-texture/two-sampler shader using all four pairs.
// Resource SGPR bases 0/12 map to tex0/1; sampler bases 8/20 map to samp0/1.
// Output: A/s1.r, B/s0.g, A/s0.b, B/s1.a.
.text
v_interp_p1_f32 v2, v0, attr0.x
v_interp_p2_f32 v2, v1, attr0.x
v_interp_p1_f32 v3, v0, attr0.y
v_interp_p2_f32 v3, v1, attr0.y
image_sample_lz v[4:7], v[2:3], s[0:7], s[20:23] dmask:0xf dim:SQ_RSRC_IMG_2D
image_sample_lz v[8:11], v[2:3], s[12:19], s[8:11] dmask:0xf dim:SQ_RSRC_IMG_2D
image_sample_lz v[12:15], v[2:3], s[0:7], s[8:11] dmask:0xf dim:SQ_RSRC_IMG_2D
image_sample_lz v[16:19], v[2:3], s[12:19], s[20:23] dmask:0xf dim:SQ_RSRC_IMG_2D
exp mrt0, v4, v9, v14, v19 done vm
s_endpgm
