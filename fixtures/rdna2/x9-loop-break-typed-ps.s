// SPDX-License-Identifier: GPL-3.0-or-later
// s24=count,s25=stop,s26/s27=expected swapped high-bit words.
// RGB = accumulated eighths, both words match, SCC at exit (before comparison).
.text
s_min_u32 s32, s24, 4
s_mov_b32 s33, 0xffffffff
s_mov_b32 s34, 0x80000000
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 0
v_mov_b32_e32 v10, 0
v_mov_b32_e32 v11, 1.0
v_mov_b32_e32 v12, 0.125
header:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 exit
s_mov_b32 s35, s33
s_mov_b32 s33, s34
s_mov_b32 s34, s35
v_add_f32_e32 v8, v8, v12
s_cmp_eq_u32 s32, s25
s_cbranch_scc0 continuing
s_branch exit
continuing:
s_sub_u32 s32, s32, 1
s_branch header
exit:
s_cselect_b32 s36, 1, 0
s_cmp_eq_u32 s36, 1
s_cbranch_scc0 check_words
v_mov_b32_e32 v10, 1.0
check_words:
s_cmp_eq_u32 s33, s26
s_cbranch_scc0 export
s_cmp_eq_u32 s34, s27
s_cbranch_scc0 export
v_mov_b32_e32 v9, 1.0
export:
exp mrt0, v8, v9, v10, v11 done vm
s_endpgm
