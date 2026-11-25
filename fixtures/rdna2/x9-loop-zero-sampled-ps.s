// SPDX-License-Identifier: GPL-3.0-or-later
// Original bounded-countdown graphics evidence. No compiler-inserted bound.
.text
v_interp_p1_f32 v2, v0, attr0.x
v_interp_p2_f32 v2, v1, attr0.x
v_interp_p1_f32 v3, v0, attr0.y
v_interp_p2_f32 v3, v1, attr0.y
s_mov_b32 s32, 0
v_mov_b32_e32 v4, 0
v_mov_b32_e32 v5, 0
v_mov_b32_e32 v6, 0
v_mov_b32_e32 v7, 0
loop_header:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 loop_exit
s_cmp_eq_u32 s32, 2
s_cbranch_scc0 second
image_sample_lz v[4:7], v[2:3], s[0:7], s[8:11] dmask:0xf dim:SQ_RSRC_IMG_2D
s_branch sampled
second:
image_sample_lz v[4:7], v[2:3], s[12:19], s[20:23] dmask:0xf dim:SQ_RSRC_IMG_2D
sampled:
s_sub_u32 s32, s32, 1
s_branch loop_header
loop_exit:
exp mrt0, v4, v5, v6, v7 done vm
s_endpgm
