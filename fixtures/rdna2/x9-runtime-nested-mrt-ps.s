// SPDX-License-Identifier: GPL-3.0-or-later
// Original nested runtime diamonds. Inherit v8 on two paths, overwrite on one.
// The nested SCC survives the outer join and drives a second branch.
.text
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 1.0
v_mov_b32_e32 v10, 0
v_mov_b32_e32 v11, 1.0
s_cmp_eq_u32 s24, 0
s_cbranch_scc0 outer_false
s_cmp_eq_u32 s25, 0
s_cbranch_scc0 inner_join
v_mov_b32_e32 v8, 1.0
inner_join:
s_branch outer_join
outer_false:
v_mov_b32_e32 v9, 0
s_cmp_eq_u32 0, 0
outer_join:
s_cbranch_scc0 exports
v_mov_b32_e32 v10, 1.0
exports:
exp mrt0, v8, v9, v10, v11 vm
exp mrt1, v10, v8, v9, v11 done vm
s_endpgm
