// SPDX-License-Identifier: GPL-3.0-or-later
// s24=outer,s25=child, s26=expected trips, s27=final child bits,
// s28=final outer bits. Bounds4; negative child bits survive zero trips.
.text
s_min_i32 s32, s24, 4
s_min_i32 s33, 4, s25
s_mov_b32 s34, 0
outer_header:
s_cmp_gt_i32 s32, 0
s_cbranch_scc0 outer_exit
s_min_i32 s33, 4, s25
inner_header:
s_cmp_lt_i32 0, s33
s_cbranch_scc0 inner_exit
s_add_u32 s34, s34, 1
s_sub_i32 s33, s33, 1
s_branch inner_header
inner_exit:
s_sub_u32 s32, s32, 1
s_branch outer_header
outer_exit:
s_cselect_b32 s35, 1, 0
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 0
v_mov_b32_e32 v10, 0
v_mov_b32_e32 v11, 1.0
s_cmp_eq_u32 s34, s26
s_cbranch_scc0 child_bits
v_mov_b32_e32 v8, 1.0
child_bits:
s_cmp_eq_u32 s33, s27
s_cbranch_scc0 outer_bits
v_mov_b32_e32 v9, 1.0
outer_bits:
s_cmp_eq_u32 s32, s28
s_cbranch_scc0 export
s_cmp_eq_u32 s35, 0
s_cbranch_scc0 export
v_mov_b32_e32 v10, 1.0
export:
exp mrt0, v8, v9, v10, v11 done vm
s_endpgm
