// SPDX-License-Identifier: GPL-3.0-or-later
// Original runtime integer relation. Input words retain their exact bits.
.text
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 1.0
v_mov_b32_e32 v10, 0
v_mov_b32_e32 v11, 1.0
s_cmp_gt_i32 s24, s25
s_cbranch_scc0 join
v_mov_b32_e32 v8, 1.0
v_mov_b32_e32 v9, 0
join:
exp mrt0, v8, v9, v10, v11 done vm
s_endpgm
