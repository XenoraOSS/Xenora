// SPDX-License-Identifier: GPL-3.0-or-later
// Original sequential bounded-loop admission fixture.
.text
s_min_u32 s32, s24, 4
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 1.0
v_mov_b32_e32 v10, 0.25
loop_header_0:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 loop_exit_0
v_add_f32_e32 v8, v10, v8
s_sub_u32 s32, s32, 1
s_branch loop_header_0
loop_exit_0:
s_min_u32 s32, s24, 4
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 1.0
v_mov_b32_e32 v10, 0.25
loop_header_1:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 loop_exit_1
v_add_f32_e32 v8, v10, v8
s_sub_u32 s32, s32, 1
s_branch loop_header_1
loop_exit_1:
s_min_u32 s32, s24, 4
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 1.0
v_mov_b32_e32 v10, 0.25
loop_header_2:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 loop_exit_2
v_add_f32_e32 v8, v10, v8
s_sub_u32 s32, s32, 1
s_branch loop_header_2
loop_exit_2:
s_min_u32 s32, s24, 4
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 1.0
v_mov_b32_e32 v10, 0.25
loop_header_3:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 loop_exit_3
v_add_f32_e32 v8, v10, v8
s_sub_u32 s32, s32, 1
s_branch loop_header_3
loop_exit_3:
s_min_u32 s32, s24, 4
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 1.0
v_mov_b32_e32 v10, 0.25
loop_header_4:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 loop_exit_4
v_add_f32_e32 v8, v10, v8
s_sub_u32 s32, s32, 1
s_branch loop_header_4
loop_exit_4:
s_min_u32 s32, s24, 4
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 1.0
v_mov_b32_e32 v10, 0.25
loop_header_5:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 loop_exit_5
v_add_f32_e32 v8, v10, v8
s_sub_u32 s32, s32, 1
s_branch loop_header_5
loop_exit_5:
s_min_u32 s32, s24, 4
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 1.0
v_mov_b32_e32 v10, 0.25
loop_header_6:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 loop_exit_6
v_add_f32_e32 v8, v10, v8
s_sub_u32 s32, s32, 1
s_branch loop_header_6
loop_exit_6:
s_min_u32 s32, s24, 4
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 1.0
v_mov_b32_e32 v10, 0.25
loop_header_7:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 loop_exit_7
v_add_f32_e32 v8, v10, v8
s_sub_u32 s32, s32, 1
s_branch loop_header_7
loop_exit_7:
s_min_u32 s32, s24, 4
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 1.0
v_mov_b32_e32 v10, 0.25
loop_header_8:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 loop_exit_8
v_add_f32_e32 v8, v10, v8
s_sub_u32 s32, s32, 1
s_branch loop_header_8
loop_exit_8:
exp mrt0, v8, v8, v8, v9 done vm
s_endpgm
