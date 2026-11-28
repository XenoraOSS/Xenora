// SPDX-License-Identifier: GPL-3.0-or-later
// Original Wave32 VOP3A vectors for the six ordered binary32 compare encodings.
// Each compare writes vcc_lo through the explicit 64-bit destination field.
// The operands exercise clear, NEG, ABS and negative-absolute input modifiers;
// each wave folds the masks into v2 as v2 = (v2 << 1) ^ mask. The per-lane
// expectations for these forms are in tests/validation/tests/vop3_compare_f32.rs.
.text
buffer_load_dword v1, v0, s[0:3], 0 idxen // Load the lane's input word.
s_waitcnt vmcnt(0)                        // Wait for the load.
v_mov_b32_e32 v2, 0                       // Clear the accumulator.
v_mov_b32_e32 v5, 1                       // Shift count for the fold.
v_mov_b32_e32 v3, 0x3f800000              // Second source: 1.0.
v_cmp_lt_f32_e64 vcc_lo, v1, v3           // Unmodified sources.
v_mov_b32_e32 v4, vcc_lo                  // Move the mask into a VGPR.
v_lshlrev_b32_e32 v2, v5, v2              // Shift the accumulator.
v_xor_b32_e32 v2, v2, v4                  // Fold the mask.
v_cmp_eq_f32_e64 vcc_lo, -v1, v3          // NEG on SRC0.
v_mov_b32_e32 v4, vcc_lo                  // Move the mask into a VGPR.
v_lshlrev_b32_e32 v2, v5, v2              // Shift the accumulator.
v_xor_b32_e32 v2, v2, v4                  // Fold the mask.
v_cmp_le_f32_e64 vcc_lo, |v1|, v3         // ABS on SRC0.
v_mov_b32_e32 v4, vcc_lo                  // Move the mask into a VGPR.
v_lshlrev_b32_e32 v2, v5, v2              // Shift the accumulator.
v_xor_b32_e32 v2, v2, v4                  // Fold the mask.
v_cmp_gt_f32_e64 vcc_lo, -|v1|, v3        // NEG and ABS on SRC0.
v_mov_b32_e32 v4, vcc_lo                  // Move the mask into a VGPR.
v_lshlrev_b32_e32 v2, v5, v2              // Shift the accumulator.
v_xor_b32_e32 v2, v2, v4                  // Fold the mask.
v_cmp_lg_f32_e64 vcc_lo, v1, -|v3|        // NEG and ABS on SRC1.
v_mov_b32_e32 v4, vcc_lo                  // Move the mask into a VGPR.
v_lshlrev_b32_e32 v2, v5, v2              // Shift the accumulator.
v_xor_b32_e32 v2, v2, v4                  // Fold the mask.
v_cmp_ge_f32_e64 vcc_lo, -v1, |v3|        // NEG on SRC0, ABS on SRC1.
v_mov_b32_e32 v4, vcc_lo                  // Move the mask into a VGPR.
v_lshlrev_b32_e32 v2, v5, v2              // Shift the accumulator.
v_xor_b32_e32 v2, v2, v4                  // Fold the mask.
buffer_store_dword v2, v0, s[4:7], 0 idxen // Store the folded word.
s_waitcnt_vscnt null, 0                   // Wait for the store.
s_endpgm                                  // End the program.
