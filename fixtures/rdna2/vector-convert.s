// SPDX-License-Identifier: GPL-3.0-or-later
// Wave32 VOP1 integer to binary32 conversions (AMD RDNA 2 ISA guide 12.8).
// Convert the input as a signed and an unsigned integer and each of its four
// bytes, byte 3 in even lanes only, and combine the results with a converted
// literal (2^24 + 1 rounds to nearest even, 2^24) and an inline integer (-16).
.text
buffer_load_dword v1, v0, s[0:3], 0 idxen
s_waitcnt vmcnt(0)
v_cvt_f32_i32_e32 v2, v1
v_cvt_f32_u32_e32 v3, v1
v_cvt_f32_ubyte0_e32 v4, v1
v_cvt_f32_ubyte1_e32 v5, v1
v_cvt_f32_ubyte2_e32 v6, v1
s_mov_b32 exec_lo, 0x55555555
v_cvt_f32_ubyte3_e32 v6, v1
s_mov_b32 exec_lo, -1
v_cvt_f32_u32_e32 v7, 0x1000001
v_cvt_f32_i32_e32 v8, -16
v_xor_b32_e32 v9, v2, v3
v_xor_b32_e32 v9, v4, v9
v_xor_b32_e32 v9, v5, v9
v_xor_b32_e32 v9, v6, v9
v_xor_b32_e32 v9, v7, v9
v_xor_b32_e32 v9, v8, v9
buffer_store_dword v9, v0, s[4:7], 0 idxen
s_waitcnt_vscnt null, 0
s_endpgm
