// SPDX-License-Identifier: GPL-3.0-or-later
// Child counter initialized once: first outer iteration exhausts it.
// Later entries retain zero, so every child entry remains bounded and safe.
.text
s_min_u32 s32, s24, 4
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 1.0
v_mov_b32_e32 v10, 0.0625
s_min_u32 s33, s25, 4
outer_header:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 outer_exit
inner_header:
s_cmp_lg_u32 s33, 0
s_cbranch_scc0 inner_exit
v_add_f32_e32 v8, v8, v10
s_sub_u32 s33, s33, 1
s_branch inner_header
inner_exit:
s_sub_u32 s32, s32, 1
s_branch outer_header
outer_exit:
exp mrt0, v8, v8, v8, v9 done vm
s_endpgm
