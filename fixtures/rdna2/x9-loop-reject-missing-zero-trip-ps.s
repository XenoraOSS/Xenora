// SPDX-License-Identifier: GPL-3.0-or-later
// Original bounded-countdown graphics evidence. No compiler-inserted bound.
.text
s_min_u32 s32, s24, 4
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 1.0
v_mov_b32_e32 v10, 0.25
loop_header:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 loop_exit
s_mov_b32 s33, 7
s_sub_u32 s32, s32, 1
s_branch loop_header
loop_exit:
s_cmp_eq_u32 s33, 7
exp mrt0, v8, v8, v8, v9 done vm
s_endpgm
