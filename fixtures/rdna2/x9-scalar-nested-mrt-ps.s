// SPDX-License-Identifier: GPL-3.0-or-later
// Original typed local and SCC joins. s24 inherited, s26 overwritten inner value,
// s25 outer condition, s27 inner condition, s28 literal expected selected result.
.text
s_mov_b32 s32, s24
s_cmp_eq_u32 s25, 0
s_cbranch_scc0 outer_false
s_mov_b32 s32, s26
s_cmp_eq_u32 s27, 0
s_cbranch_scc0 inner_false
s_add_u32 s32, s32, 1
s_branch inner_join
inner_false:
s_cmp_eq_u32 0, 1
inner_join:
s_branch outer_join
outer_false:
s_cmp_eq_u32 0, 0
outer_join:
s_cselect_b32 s33, s32, s24
s_mov_b32 s105, s33
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 0
v_mov_b32_e32 v10, 0
v_mov_b32_e32 v11, 1.0
s_cbranch_scc0 flag_done
v_mov_b32_e32 v9, 1.0
flag_done:
s_cmp_eq_u32 s105, s28
s_cbranch_scc0 exports
v_mov_b32_e32 v8, 1.0
exports:
exp mrt0, v8, v9, v10, v11 vm
exp mrt1, v10, v8, v9, v11 done vm
s_endpgm
