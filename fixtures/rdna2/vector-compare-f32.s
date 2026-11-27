// SPDX-License-Identifier: GPL-3.0-or-later
// Encoding inventory for Wave32 binary32 VOPC compares (guide Table 61, opcodes 0-15).
// Operand and execution cases are in tests/validation/tests/vopc_f32.rs.
.text
buffer_load_dword v2, v0, s[0:3], 0 idxen
s_waitcnt vmcnt(0)
v_mov_b32_e32 v3, 0x7fc00000
v_cmp_f_f32_e32 vcc_lo, v2, v3
v_cmp_lt_f32_e32 vcc_lo, v2, v3
v_cmp_eq_f32_e32 vcc_lo, v2, v3
v_cmp_le_f32_e32 vcc_lo, v2, v3
v_cmp_gt_f32_e32 vcc_lo, v2, v3
v_cmp_lg_f32_e32 vcc_lo, v2, v3
v_cmp_ge_f32_e32 vcc_lo, v2, v3
v_cmp_o_f32_e32 vcc_lo, v2, v3
v_cmp_u_f32_e32 vcc_lo, v2, v3
v_cmp_nge_f32_e32 vcc_lo, v2, v3
v_cmp_nlg_f32_e32 vcc_lo, v2, v3
v_cmp_ngt_f32_e32 vcc_lo, v2, v3
v_cmp_nle_f32_e32 vcc_lo, v2, v3
v_cmp_neq_f32_e32 vcc_lo, v2, v3
v_cmp_nlt_f32_e32 vcc_lo, v2, v3
v_cmp_tru_f32_e32 vcc_lo, v2, v3
s_mov_b32 s8, 0x80000000
v_cmp_lt_f32_e32 vcc_lo, s8, v3
v_mov_b32_e32 v255, 1.0
v_cmp_u_f32_e32 vcc_lo, vcc_lo, v255
v_cmp_neq_f32_e32 vcc_lo, 0x80000000, v3
v_mov_b32_e32 v2, vcc_lo
buffer_store_dword v2, v0, s[4:7], 0 idxen
s_waitcnt_vscnt null, 0
s_endpgm
