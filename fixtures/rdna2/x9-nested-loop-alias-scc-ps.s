// SPDX-License-Identifier: GPL-3.0-or-later
// s24/s25 outer/inner counts; s26 expected sum, s27 expected child result.
// s36 is a MOV snapshot of the previous child result, consumed on next reentry.
// Child final SCC is consumed by CSELECT before any flag-changing instruction.
// Expected sum = 8*n + m*n*(n-1)/2; child result = 1+n*m (n,m bounded by 4).
.text
s_min_u32 s32, s24, 4
s_mov_b32 s34, 1
s_mov_b32 s35, 0
s_mov_b32 s36, 1
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 0
v_mov_b32_e32 v10, 0
v_mov_b32_e32 v11, 1.0
outer_header:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 outer_exit
s_add_u32 s35, s35, s36
s_min_u32 s33, s25, 4
inner_header:
s_cmp_lg_u32 s33, 0
s_cbranch_scc0 inner_exit
s_add_u32 s34, s34, 1
s_sub_u32 s33, s33, 1
s_branch inner_header
inner_exit:
s_cselect_b32 s37, 100, 7
s_add_u32 s35, s35, s37
s_mov_b32 s36, s34
s_sub_u32 s32, s32, 1
s_branch outer_header
outer_exit:
s_cmp_eq_u32 s35, s26
s_cbranch_scc0 sum_checked
v_mov_b32_e32 v8, 1.0
sum_checked:
s_cmp_eq_u32 s34, s27
s_cbranch_scc0 exports
v_mov_b32_e32 v9, 1.0
exports:
exp mrt0, v8, v9, v10, v11 done vm
s_endpgm
