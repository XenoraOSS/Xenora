// SPDX-License-Identifier: GPL-3.0-or-later
// Negative constant prunes sampling and the body-only s31 read.
.text
v_interp_p1_f32 v2, v0, attr0.x
v_interp_p2_f32 v2, v1, attr0.x
v_interp_p1_f32 v3, v0, attr0.y
v_interp_p2_f32 v3, v1, attr0.y
s_mov_b32 s32, -1
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 0
v_mov_b32_e32 v10, 0
v_mov_b32_e32 v11, 1.0
header:
s_cmp_gt_i32 s32, 0
s_cbranch_scc0 exit
s_mov_b32 s38, s31
image_sample_lz v[4:7], v[2:3], s[0:7], s[8:11] dmask:0xf dim:SQ_RSRC_IMG_2D
v_add_f32_e32 v8, v8, v4
v_add_f32_e32 v9, v9, v5
v_add_f32_e32 v10, v10, v6
s_sub_i32 s32, s32, 1
s_branch header
exit:
exp mrt0, v8, v9, v10, v11 done vm
s_endpgm
