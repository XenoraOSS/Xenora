// SPDX-License-Identifier: GPL-3.0-or-later
// Original forward branch preserves positions instead of collapsing geometry.
.text
s_branch position
v_mov_b32_e32 v0, 0
v_mov_b32_e32 v1, 0
position:
exp pos0, v0, v1, v2, v3 done
s_endpgm
