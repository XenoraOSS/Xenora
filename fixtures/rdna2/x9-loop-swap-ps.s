// SPDX-License-Identifier: GPL-3.0-or-later
// Original bounded-countdown graphics evidence. No compiler-inserted bound.
.text
s_min_u32 s32, s24, 4
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 1.0
v_mov_b32_e32 v10, 0.25
s_mov_b32 s33, 7
s_mov_b32 s35, 11
loop_header:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 loop_exit
s_mov_b32 s34, s33
s_mov_b32 s33, s35
s_mov_b32 s35, s34
s_sub_u32 s32, s32, 1
s_branch loop_header
loop_exit:
s_cmp_eq_u32 s33, s25
s_cbranch_scc0 exports
s_cmp_eq_u32 s35, s26
s_cbranch_scc0 exports
v_mov_b32_e32 v8, 1.0
exports:
exp mrt0, v8, v8, v8, v9 done vm
s_endpgm
