// SPDX-License-Identifier: GPL-3.0-or-later
// s24=count (MIN 4), s25/s26=expected pair. Red/green independently match.
// Begin (7,11); counter==2 swaps the pair, other iterations increment first.
.text
s_min_u32 s32, s24, 4
s_mov_b32 s33, 7
s_mov_b32 s35, 11
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 0
v_mov_b32_e32 v10, 0
v_mov_b32_e32 v11, 1.0
loop_header:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 loop_exit
s_cmp_eq_u32 s32, 2
s_cbranch_scc0 increment
s_mov_b32 s34, s33
s_mov_b32 s33, s35
s_mov_b32 s35, s34
s_branch body_join
increment:
s_add_u32 s33, s33, 1
body_join:
s_sub_u32 s32, s32, 1
s_branch loop_header
loop_exit:
s_cmp_eq_u32 s33, s25
s_cbranch_scc0 first_checked
v_mov_b32_e32 v8, 1.0
first_checked:
s_cmp_eq_u32 s35, s26
s_cbranch_scc0 exports
v_mov_b32_e32 v9, 1.0
exports:
exp mrt0, v8, v9, v10, v11 done vm
s_endpgm
