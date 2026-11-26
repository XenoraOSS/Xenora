// SPDX-License-Identifier: GPL-3.0-or-later
// Original SOP1 vectors for the twelve EXEC-writing forms: the ten
// S_*_SAVEEXEC_B32 forms and the two S_*_WREXEC_B32 forms.
//
// Every form is preceded by a full EXEC so that it starts from the same mask,
// and the mask is restored afterwards so that the vector reads that follow are
// not suppressed by the form's own result. The ten SAVEEXEC forms leave the
// previous EXEC_LO in their destination; the two WREXEC forms leave the new
// one. With EXEC = 0xffffffff and S0 = 0xf0f0f0f0 every SAVEEXEC destination
// is therefore 0xffffffff, and the two WREXEC destinations are 0x0f0f0f0f
// (ANDN1) and 0 (ANDN2). The program XORs the twelve destinations together,
// so the stored word is 0x0f0f0f0f (worked out by hand below).
.text
buffer_load_dword v1, v0, s[0:3], 0 idxen
s_waitcnt vmcnt(0)
s_mov_b32 s0, 0xf0f0f0f0
s_mov_b32 exec_lo, 0xffffffff
s_and_saveexec_b32 s10, s0          // s10 = 0xffffffff, EXEC = 0xf0f0f0f0
s_mov_b32 exec_lo, 0xffffffff
s_or_saveexec_b32 s11, s0           // s11 = 0xffffffff, EXEC = 0xffffffff
s_mov_b32 exec_lo, 0xffffffff
s_xor_saveexec_b32 s12, s0          // s12 = 0xffffffff, EXEC = 0x0f0f0f0f
s_mov_b32 exec_lo, 0xffffffff
s_andn2_saveexec_b32 s13, s0        // s13 = 0xffffffff, EXEC = 0
s_mov_b32 exec_lo, 0xffffffff
s_orn2_saveexec_b32 s14, s0         // s14 = 0xffffffff, EXEC = 0xf0f0f0f0
s_mov_b32 exec_lo, 0xffffffff
s_nand_saveexec_b32 s15, s0         // s15 = 0xffffffff, EXEC = 0x0f0f0f0f
s_mov_b32 exec_lo, 0xffffffff
s_nor_saveexec_b32 s16, s0          // s16 = 0xffffffff, EXEC = 0
s_mov_b32 exec_lo, 0xffffffff
s_xnor_saveexec_b32 s17, s0         // s17 = 0xffffffff, EXEC = 0xf0f0f0f0
s_mov_b32 exec_lo, 0xffffffff
s_andn1_saveexec_b32 s18, s0        // s18 = 0xffffffff, EXEC = 0x0f0f0f0f
s_mov_b32 exec_lo, 0xffffffff
s_orn1_saveexec_b32 s19, s0         // s19 = 0xffffffff, EXEC = 0xffffffff
s_mov_b32 exec_lo, 0xffffffff
s_andn1_wrexec_b32 s20, s0          // EXEC = 0x0f0f0f0f, s20 = 0x0f0f0f0f
s_mov_b32 exec_lo, 0xffffffff
s_andn2_wrexec_b32 s21, s0          // EXEC = 0, s21 = 0
s_mov_b32 exec_lo, 0xffffffff
v_mov_b32_e32 v2, s10
v_mov_b32_e32 v3, s11
v_xor_b32_e32 v2, v2, v3
v_mov_b32_e32 v3, s12
v_xor_b32_e32 v2, v2, v3
v_mov_b32_e32 v3, s13
v_xor_b32_e32 v2, v2, v3
v_mov_b32_e32 v3, s14
v_xor_b32_e32 v2, v2, v3
v_mov_b32_e32 v3, s15
v_xor_b32_e32 v2, v2, v3
v_mov_b32_e32 v3, s16
v_xor_b32_e32 v2, v2, v3
v_mov_b32_e32 v3, s17
v_xor_b32_e32 v2, v2, v3
v_mov_b32_e32 v3, s18
v_xor_b32_e32 v2, v2, v3
v_mov_b32_e32 v3, s19
v_xor_b32_e32 v2, v2, v3
v_mov_b32_e32 v3, s20
v_xor_b32_e32 v2, v2, v3
v_mov_b32_e32 v3, s21
v_xor_b32_e32 v2, v2, v3
v_xor_b32_e32 v2, v2, v1            // v2 = 0x0f0f0f0f
buffer_store_dword v2, v0, s[4:7], 0 idxen
s_waitcnt_vscnt null, 0
s_endpgm
