// SPDX-License-Identifier: GPL-3.0-or-later
// Original uniform conditional geometry: signed -1 < 1 is true (fallthrough),
// unsigned 0xffffffff < 1 is false (taken). Vector math must preserve SCC.
// AMD RDNA2 ISA 12.4 SOPC / 12.5 SOPP; no runtime lane divergence.
.text
v_mov_b32_e32 v8, lit(0x3f000000)
v_mov_b32_e32 v9, lit(0x3f000000)
s_cmp_lt_i32 -1, 1
v_mul_f32_e32 v0, v0, v8
s_cbranch_scc0 check_unsigned
v_add_f32_e32 v0, v0, v9
check_unsigned:
s_cmp_lt_u32 -1, 1
v_mov_b32_e32 v8, 0
s_cbranch_scc0 output
v_mov_b32_e32 v1, 0
output:
exp pos0, v0, v1, v2, v3 done
s_endpgm
