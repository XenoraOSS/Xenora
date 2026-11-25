// SPDX-License-Identifier: GPL-3.0-or-later
// Original nested-countdown evidence.
.text
s_min_u32 s32, s24, 4
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 1.0
s_mov_b32 s34, 1
outer_header:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 outer_exit
s_add_u32 s34, s34, 2
s_min_u32 s33, s25, 4
first_header:
s_cmp_lg_u32 s33, 0
s_cbranch_scc0 first_exit
s_add_u32 s34, s34, 3
s_sub_u32 s33, s33, 1
s_branch first_header
first_exit:
s_min_u32 s33, s25, 4
second_header:
s_cmp_lg_u32 s33, 0
s_cbranch_scc0 second_exit
s_add_u32 s34, s34, 5
s_sub_u32 s33, s33, 1
s_branch second_header
second_exit:
s_add_u32 s34, s34, 7
s_sub_u32 s32, s32, 1
s_branch outer_header
outer_exit:
s_cmp_eq_u32 s34, s26
s_cbranch_scc0 exports
v_mov_b32_e32 v8, 1.0
exports:
exp mrt0, v8, v8, v8, v9 done vm
s_endpgm
