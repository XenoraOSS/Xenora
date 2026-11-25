// SPDX-License-Identifier: GPL-3.0-or-later
// Original SCC0 paths produce (1, 0, .5, 1). Compare expectations are literal:
// signed -1 < 1 true; unsigned -1 < 1 false; 1 == 0x12345678 false;
// unsigned 0x80000000 > 1 true. Vector instructions do not change SCC.
// AMD RDNA2 ISA 12.4 SOPC / 12.5 SOPP; no runtime lane divergence.
.text
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 0
v_mov_b32_e32 v10, 0
v_mov_b32_e32 v11, 1.0
v_mov_b32_e32 v13, lit(0x3f000000)
s_cmp_lt_i32 -1, 1
v_mov_b32_e32 v12, 0
s_cbranch_scc0 unsigned_compare
v_mov_b32_e32 v8, 1.0
unsigned_compare:
s_cmp_lt_u32 -1, 1
v_add_f32_e32 v12, v11, v11
s_cbranch_scc0 literal_false
v_mov_b32_e32 v9, 1.0
literal_false:
s_cmp_eq_u32 1, lit(0x12345678)
s_cbranch_scc0 literal_true
v_mov_b32_e32 v8, 0
literal_true:
s_cmp_gt_u32 lit(0x80000000), 1
v_mov_b32_e32 v12, 0
s_cbranch_scc0 output
v_mul_f32_e32 v10, v11, v13
output:
exp mrt0, v8, v9, v10, v11 done vm
s_endpgm
