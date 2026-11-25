// SPDX-License-Identifier: GPL-3.0-or-later
// Original actual low->high flag chain. s24/25 low operands, s26/27 high operands,
// s28/29 literal expected low/high words. RGB = low match/high match/final SCC.
.text
s_mov_b32 s32, s26
s_add_u32 s34, s24, s25
s_addc_u32 s32, s32, s32
v_mov_b32_e32 v8, 0
v_mov_b32_e32 v9, 0
v_mov_b32_e32 v10, 0
v_mov_b32_e32 v11, 1.0
s_cbranch_scc0 flag_done
v_mov_b32_e32 v10, 1.0
flag_done:
s_cmp_eq_u32 s34, s28
s_cbranch_scc0 low_done
v_mov_b32_e32 v8, 1.0
low_done:
s_cmp_eq_u32 s32, s29
s_cbranch_scc0 exports
v_mov_b32_e32 v9, 1.0
exports:
exp mrt0, v8, v9, v10, v11 done vm
s_endpgm
