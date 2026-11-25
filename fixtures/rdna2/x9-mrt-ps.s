// SPDX-License-Identifier: GPL-3.0-or-later
// Original synthetic eight-target fixture. Only the final export sets DONE.
.text
v_mov_b32_e32 v2, 1.0
v_mov_b32_e32 v3, 0
exp mrt0, v3, v3, v3, v2 vm
exp mrt1, v2, v3, v3, v2 vm
exp mrt2, v3, v2, v3, v2 vm
exp mrt3, v3, v3, v2, v2 vm
exp mrt4, v2, v2, v3, v2 vm
exp mrt5, v2, v3, v2, v2 vm
exp mrt6, v3, v2, v2, v2 vm
exp mrt7, v2, v2, v2, v2 done vm
s_endpgm
