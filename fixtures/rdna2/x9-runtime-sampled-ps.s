// SPDX-License-Identifier: GPL-3.0-or-later
// Original runtime-uniform sampling diamond, with one common MRT0/DONE suffix.
// s24 == 0 samples tex0/samp0; otherwise samples tex1/samp1.
// The feasible-pair union is bits0|3 (0x09), independent of uniform values.
.text
v_interp_p1_f32 v2, v0, attr0.x
v_interp_p2_f32 v2, v1, attr0.x
v_interp_p1_f32 v3, v0, attr0.y
v_interp_p2_f32 v3, v1, attr0.y
s_cmp_eq_u32 s24, 0
s_cbranch_scc0 second_pair
image_sample_lz v[4:7], v[2:3], s[0:7], s[8:11] dmask:0xf dim:SQ_RSRC_IMG_2D
s_branch join
second_pair:
image_sample_lz v[4:7], v[2:3], s[12:19], s[20:23] dmask:0xf dim:SQ_RSRC_IMG_2D
join:
exp mrt0, v4, v5, v6, v7 done vm
s_endpgm
