// SPDX-License-Identifier: GPL-3.0-or-later
// Original bounded-countdown graphics evidence. No compiler-inserted bound.
.text
s_min_u32 s32, s24, 4
s_mov_b32 s33, 3
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 0
v_mov_b32_e32 v10, 0
v_mov_b32_e32 v11, 1.0
s_cbranch_scc0 min_saved
v_mov_b32_e32 v9, 1.0
min_saved:
loop_header:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 loop_exit
s_add_u32 s33, s33, 5
s_sub_u32 s32, s32, 1
s_branch loop_header

loop_exit:
s_cbranch_scc0 exit_flag_saved
v_mov_b32_e32 v10, 1.0
exit_flag_saved:
s_cmp_eq_u32 s33, s25
s_cbranch_scc0 exports
v_mov_b32_e32 v8, 1.0
exports:
exp mrt0, v8, v9, v10, v11 done vm
s_endpgm
