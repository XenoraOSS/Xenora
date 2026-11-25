// SPDX-License-Identifier: GPL-3.0-or-later
// s24=count (original MIN bound 4), s25=select, s26=expected result.
// select==0 starts at 3, otherwise 7; each iteration adds 5.
.text
s_min_u32 s32, s24, 4
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 1.0
s_cmp_eq_u32 s25, 0
s_cbranch_scc0 other_preheader
s_mov_b32 s33, 3
s_branch loop_header
other_preheader:
s_mov_b32 s33, 7
loop_header:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 loop_exit
s_add_u32 s33, s33, 5
s_sub_u32 s32, s32, 1
s_branch loop_header
loop_exit:
s_cmp_eq_u32 s33, s26
s_cbranch_scc0 exports
v_mov_b32_e32 v8, 1.0
exports:
exp mrt0, v8, v8, v8, v9 done vm
s_endpgm
