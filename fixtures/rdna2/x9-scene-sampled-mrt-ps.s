// SPDX-License-Identifier: GPL-3.0-or-later
// Original scene shader: sample slot zero; RGBA at MRT0, BRGA at MRT1.
.text
v_interp_p1_f32 v2, v0, attr0.x
v_interp_p2_f32 v2, v1, attr0.x
v_interp_p1_f32 v3, v0, attr0.y
v_interp_p2_f32 v3, v1, attr0.y
image_sample_lz v[4:7], v[2:3], s[0:7], s[8:11] dmask:0xf dim:SQ_RSRC_IMG_2D
exp mrt0, v4, v5, v6, v7 vm
exp mrt1, v6, v4, v5, v7 done vm
s_endpgm
