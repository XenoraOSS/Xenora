// SPDX-License-Identifier: GPL-3.0-or-later
// Original MIN-bounded runtime sampling loop. Each sample contributes to RGB.
// Counter==2 samples pair0; every other positive counter samples pair1.
.text
v_interp_p1_f32 v2, v0, attr0.x
v_interp_p2_f32 v2, v1, attr0.x
v_interp_p1_f32 v3, v0, attr0.y
v_interp_p2_f32 v3, v1, attr0.y
s_min_u32 s32, s24, 4
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 0
v_mov_b32_e32 v10, 0
v_mov_b32_e32 v11, 1.0
loop_header:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 loop_exit
s_cmp_eq_u32 s32, 2
s_cbranch_scc0 second_pair
image_sample_lz v[4:7], v[2:3], s[0:7], s[8:11] dmask:0xf dim:SQ_RSRC_IMG_2D
s_branch sample_join
second_pair:
image_sample_lz v[4:7], v[2:3], s[12:19], s[20:23] dmask:0xf dim:SQ_RSRC_IMG_2D
sample_join:
v_add_f32_e32 v8, v8, v4
v_add_f32_e32 v9, v9, v5
v_add_f32_e32 v10, v10, v6
s_sub_u32 s32, s32, 1
s_branch loop_header
loop_exit:
exp mrt0, v8, v9, v10, v11 done vm
s_endpgm
