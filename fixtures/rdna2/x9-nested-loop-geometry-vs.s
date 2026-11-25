// SPDX-License-Identifier: GPL-3.0-or-later
// Original nested vertex countdown: shift x by min(s24,4)*min(s25,4)/16.
.text
s_min_u32 s32, s24, 4
v_mov_b32_e32 v8, v0
v_mov_b32_e32 v10, 0.0625
outer_header:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 outer_exit
s_min_u32 s33, s25, 4
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
exp pos0, v8, v1, v2, v3 done
s_endpgm
