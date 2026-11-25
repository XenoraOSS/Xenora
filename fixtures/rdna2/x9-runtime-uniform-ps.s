// SPDX-License-Identifier: GPL-3.0-or-later
// Original runtime join: s25 == 0 selects red; any other value selects green.
// Every runtime path defines all export components. No wave-mask interpretation.
.text
v_mov_b32_e32 v10, 0
v_mov_b32_e32 v11, 1.0
s_cmp_eq_u32 s25, 0
s_cbranch_scc0 green
v_mov_b32_e32 v8, 1.0
v_mov_b32_e32 v9, 0
s_branch join
green:
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 1.0
join:
exp mrt0, v8, v9, v10, v11 done vm
s_endpgm
