// SPDX-License-Identifier: GPL-3.0-or-later
// Original PE/native VS: same result check as ADD PS. Match left, mismatch right.
.text
s_mov_b32 s32, s24
s_add_u32 s32, s32, s25
s_cmp_eq_u32 s32, s27
v_mov_b32_e32 v8, lit(0x3f000000)
v_mul_f32_e32 v0, v0, v8
s_cbranch_scc0 right
v_mov_b32_e32 v8, lit(0xbf000000)
s_branch join
right:
v_mov_b32_e32 v8, lit(0x3f000000)
join:
v_add_f32_e32 v0, v0, v8
exp pos0, v0, v1, v2, v3 done
s_endpgm
