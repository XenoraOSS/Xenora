// SPDX-License-Identifier: GPL-3.0-or-later
// Recurring child exits: outer even uses a true unconditional exit at child=2;
// outer odd uses a false SCC0 exit at child=1. Child resets to3 each entry.
// Each visited child body swaps high-bit words and adds1/16. Outer consumes
// child exit SCC via CSELECT, adding10 for true and1 for false.
// s24=outer bound4,s25=expected SCC sum,s26/s27=expected final swapped words.
.text
s_min_u32 s32, s24, 4
s_mov_b32 s34, 0xffffffff
s_mov_b32 s35, 0x80000000
s_mov_b32 s37, 0
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 0
v_mov_b32_e32 v10, 0
v_mov_b32_e32 v11, 1.0
v_mov_b32_e32 v12, 0.0625
outer_header:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 outer_exit
s_mov_b32 s33, 3
inner_header:
s_cmp_lg_u32 s33, 0
s_cbranch_scc0 inner_exit
s_mov_b32 s36, s34
s_mov_b32 s34, s35
s_mov_b32 s35, s36
v_add_f32_e32 v8, v8, v12
s_cmp_eq_u32 s32, 2
s_cbranch_scc0 check_four
s_branch even_outer
check_four:
s_cmp_eq_u32 s32, 4
s_cbranch_scc0 odd_outer
even_outer:
s_cmp_eq_u32 s33, 2
s_cbranch_scc0 inner_continue
s_branch inner_exit
odd_outer:
s_cmp_lg_u32 s33, 1
s_cbranch_scc0 inner_exit
inner_continue:
s_sub_u32 s33, s33, 1
s_branch inner_header
inner_exit:
s_cselect_b32 s38, 10, 1
s_add_u32 s37, s37, s38
s_sub_u32 s32, s32, 1
s_branch outer_header
outer_exit:
s_cmp_eq_u32 s37, s25
s_cbranch_scc0 words
v_mov_b32_e32 v10, 1.0
words:
s_cmp_eq_u32 s34, s26
s_cbranch_scc0 export
s_cmp_eq_u32 s35, s27
s_cbranch_scc0 export
v_mov_b32_e32 v9, 1.0
export:
exp mrt0, v8, v9, v10, v11 done vm
s_endpgm
