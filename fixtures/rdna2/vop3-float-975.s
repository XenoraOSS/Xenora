// SPDX-License-Identifier: GPL-3.0-or-later
// Original Wave32 encodings for the DX9 legacy binary32 multiply (VOP2 and
// VOP3) and the three-input binary32 median. Input modifiers exercise ABS,
// NEG and negative-absolute on every MED3 source position.
.text
v_mul_legacy_f32_e32 v2, v1, v3       // DX9 zero-wins binary32 multiply, VOP2.
v_mul_legacy_f32_e64 v2, -v1, |v3|    // Same operation, VOP3 with modifiers.
v_med3_f32_e64 v2, v1, v3, v4         // Three-input median, unmodified.
v_med3_f32_e64 v2, -v1, |v3|, -|v4|   // Modifiers on all three sources.
s_endpgm                              // End the program.
