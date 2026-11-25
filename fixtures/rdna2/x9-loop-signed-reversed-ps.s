// SPDX-License-Identifier: GPL-3.0-or-later
// Original signed-positive countdown. s24=input,s25=expected trips,
// s26=expected final bits,s27=expected exit SCC,s28=expected decrement SCC sum.
// RGB independently checks trips, final bits, and both saved SCC quantities.
.text
s_min_i32 s32, 0x100, s24
s_mov_b32 s33, 0
s_mov_b32 s35, 0
header:
s_cmp_lt_i32 0, s32
s_cbranch_scc0 exit
s_add_u32 s33, s33, 1
s_sub_u32 s32, s32, 1
s_cselect_b32 s34, 1, 0
s_add_u32 s35, s35, s34
s_branch header
exit:
s_cselect_b32 s36, 1, 0
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 0
v_mov_b32_e32 v10, 0
v_mov_b32_e32 v11, 1.0
s_cmp_eq_u32 s33, s25
s_cbranch_scc0 final_bits
v_mov_b32_e32 v8, 1.0
final_bits:
s_cmp_eq_u32 s32, s26
s_cbranch_scc0 flags
v_mov_b32_e32 v9, 1.0
flags:
s_cmp_eq_u32 s36, s27
s_cbranch_scc0 export
s_cmp_eq_u32 s35, s28
s_cbranch_scc0 export
v_mov_b32_e32 v10, 1.0
export:
exp mrt0, v8, v9, v10, v11 done vm
s_endpgm
