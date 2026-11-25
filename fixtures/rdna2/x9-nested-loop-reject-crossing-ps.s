// SPDX-License-Identifier: GPL-3.0-or-later
// Original crossing intervals: A header, B header, A latch, B latch.
// Must reject; edge-entry/exit validation may reject before the forest check.
.text
s_min_u32 s32, s24, 2
s_min_u32 s33, s25, 2
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 1.0
header_a:
s_cmp_lg_u32 s32, 0
s_cbranch_scc0 exit_a
header_b:
s_cmp_lg_u32 s33, 0
s_cbranch_scc0 exit_b
s_sub_u32 s32, s32, 1
s_branch header_a
exit_a:
s_sub_u32 s33, s33, 1
s_branch header_b
exit_b:
exp mrt0, v8, v8, v8, v9 done vm
s_endpgm
