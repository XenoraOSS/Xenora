// SPDX-License-Identifier: GPL-3.0-or-later
// Original vertex early break: x advances by .125 per continuing iteration.
// (n,stop)=(0,2),(4,4),(4,2),(4,0) => x shifts0,0,.25,.5.
.text
s_min_u32 s32, s24, 4
v_mov_b32_e32 v8, v0
v_mov_b32_e32 v9, 1.0
v_mov_b32_e32 v10, 0.125
loop_header:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 loop_exit
s_cmp_lg_u32 s32, s25
s_cbranch_scc0 loop_exit
v_add_f32_e32 v8, v10, v8
s_sub_u32 s32, s32, 1
s_branch loop_header
loop_exit:
exp pos0, v8, v1, v2, v3 done
s_endpgm
