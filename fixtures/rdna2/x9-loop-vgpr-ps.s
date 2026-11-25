// SPDX-License-Identifier: GPL-3.0-or-later
// Original runtime countdown: s24 requested count, explicitly bounded by guest MIN.
// Counts 0/1/2/3/4+ yield RGB 0/.25/.5/.75/1, with alpha always one.
.text
s_min_u32 s32, s24, 4
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 1.0
v_mov_b32_e32 v10, 0.25
loop_header:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 loop_exit
v_add_f32_e32 v8, v10, v8
s_sub_u32 s32, s32, 1
s_branch loop_header
loop_exit:
exp mrt0, v8, v8, v8, v9 done vm
s_endpgm
