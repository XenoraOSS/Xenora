// SPDX-License-Identifier: GPL-3.0-or-later
// Original forward branches skip a literal move and a premature export.
.text
v_mov_b32_e32 v8, lit(0x3f000000)
v_mov_b32_e32 v9, 1.0
v_mov_b32_e32 v10, 0
s_branch color
v_mov_b32_e32 v8, lit(0x3e800000)
color:
s_branch output
exp mrt0, v10, v10, v10, v9 done vm
output:
exp mrt0, v8, v10, v9, v9 done vm
s_endpgm
