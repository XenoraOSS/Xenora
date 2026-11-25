// SPDX-License-Identifier: GPL-3.0-or-later
// Original per-draw uniform fixture. s24 is supplied at runtime; it is not
// a specialization constant. Zero shifts positions left, nonzero shifts right.
// Synthetic ABI proposal: s24..s31 are eight uniform uint words, buffer2.
.text
s_cmp_eq_u32 s24, 0
s_cbranch_scc0 right
v_mov_b32_e32 v8, lit(0xbf000000)
s_branch join
right:
v_mov_b32_e32 v8, lit(0x3f000000)
join:
v_add_f32_e32 v0, v0, v8
exp pos0, v0, v1, v2, v3 done
s_endpgm
