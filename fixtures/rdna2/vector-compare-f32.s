// SPDX-License-Identifier: GPL-3.0-or-later
// Wave32 VOPC binary32 compare vectors (#786): the ordered, unordered and
// complement relations over lane inputs that include quiet and signaling
// NaNs, both signed zeros, infinities and subnormals; SGPR, literal, VGPR
// and vcc_lo sources; partial EXEC; and runtime s_cbranch_vccz on a TRU
// result (never taken) and an F result (always taken). Each wave folds its
// masks into v2 as v2 = (v2 << 1) ^ mask. Expected results come from exact
// definitions in tests/validation/known_answers.rs, not from this file.
.text
buffer_load_dword v1, v0, s[0:3], 0 idxen
s_waitcnt vmcnt(0)
s_mov_b32 s8, 0x3fc00000              // 1.5f in an SGPR (a bit pattern no inline float constant shares)
v_mov_b32_e32 v2, 0                   // accumulator
v_mov_b32_e32 v5, 1                   // shift count
v_mov_b32_e32 v6, s8                  // 1.5 in a VGPR for the VSRC1-only compares
v_cmp_lt_f32_e32 vcc_lo, s8, v1       // 1.5 < x, false on NaN lanes
v_mov_b32_e32 v4, vcc_lo
v_xor_b32_e32 v2, v2, v4
v_cmp_eq_f32_e32 vcc_lo, v1, v1       // x == x: false exactly on NaN lanes
v_mov_b32_e32 v4, vcc_lo
v_lshlrev_b32_e32 v2, v5, v2
v_xor_b32_e32 v2, v2, v4
v_cmp_u_f32_e32 vcc_lo, v1, v1        // unordered: true exactly on NaN lanes
v_mov_b32_e32 v4, vcc_lo
v_lshlrev_b32_e32 v2, v5, v2
v_xor_b32_e32 v2, v2, v4
v_cmp_o_f32_e32 vcc_lo, v1, v1        // ordered: the complement of U
v_mov_b32_e32 v4, vcc_lo
v_lshlrev_b32_e32 v2, v5, v2
v_xor_b32_e32 v2, v2, v4
v_cmp_lg_f32_e32 vcc_lo, 0x3f400000, v1 // 0.75 < x or 0.75 > x, false on NaN lanes
v_mov_b32_e32 v4, vcc_lo
v_lshlrev_b32_e32 v2, v5, v2
v_xor_b32_e32 v2, v2, v4
v_cmp_ngt_f32_e32 vcc_lo, s8, v1      // NaN or 1.5 <= x
v_mov_b32_e32 v4, vcc_lo
v_lshlrev_b32_e32 v2, v5, v2
v_xor_b32_e32 v2, v2, v4
v_cmp_le_f32_e32 vcc_lo, v1, v1        // x <= x: false exactly on NaN lanes
v_mov_b32_e32 v4, vcc_lo
v_lshlrev_b32_e32 v2, v5, v2
v_xor_b32_e32 v2, v2, v4
v_cmp_gt_f32_e32 vcc_lo, s8, v1       // 1.5 > x, false on NaN lanes
v_mov_b32_e32 v4, vcc_lo
v_lshlrev_b32_e32 v2, v5, v2
v_xor_b32_e32 v2, v2, v4
v_cmp_ge_f32_e32 vcc_lo, v1, v6       // x >= 1.5, false on NaN lanes
v_mov_b32_e32 v4, vcc_lo
v_lshlrev_b32_e32 v2, v5, v2
v_xor_b32_e32 v2, v2, v4
v_cmp_nge_f32_e32 vcc_lo, s8, v1      // NaN or 1.5 < x
v_mov_b32_e32 v4, vcc_lo
v_lshlrev_b32_e32 v2, v5, v2
v_xor_b32_e32 v2, v2, v4
v_cmp_nlg_f32_e32 vcc_lo, v1, v1      // NaN or x == x: every lane passes
v_mov_b32_e32 v4, vcc_lo
v_lshlrev_b32_e32 v2, v5, v2
v_xor_b32_e32 v2, v2, v4
v_cmp_nle_f32_e32 vcc_lo, v1, v6      // NaN or x > 1.5
v_mov_b32_e32 v4, vcc_lo
v_lshlrev_b32_e32 v2, v5, v2
v_xor_b32_e32 v2, v2, v4
s_mov_b32 exec_lo, 0x0f0f0f0f
v_cmp_neq_f32_e32 vcc_lo, s8, v1      // only the EXEC lanes, NaN or 1.5 != x
s_mov_b32 exec_lo, -1
v_mov_b32_e32 v4, vcc_lo
v_lshlrev_b32_e32 v2, v5, v2
v_xor_b32_e32 v2, v2, v4
v_cmp_nlt_f32_e32 vcc_lo, vcc_lo, v1  // reads the mask bits as a float; NaN or mask >= x
v_mov_b32_e32 v4, vcc_lo
v_lshlrev_b32_e32 v2, v5, v2
v_xor_b32_e32 v2, v2, v4
v_cmp_tru_f32_e32 vcc_lo, v1, v1      // all EXEC lanes
s_cbranch_vccz .Lskip                 // runtime branch, never taken
v_xor_b32_e32 v2, v2, v1
.Lskip:
v_cmp_f_f32_e32 vcc_lo, v1, v1        // no lane at all
s_cbranch_vccz .Ldone                 // runtime branch, always taken
v_mov_b32_e32 v2, 0
.Ldone:
buffer_store_dword v2, v0, s[4:7], 0 idxen
s_waitcnt_vscnt null, 0
s_endpgm
