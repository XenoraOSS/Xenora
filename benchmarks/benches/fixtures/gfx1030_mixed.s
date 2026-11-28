// SPDX-License-Identifier: GPL-3.0-or-later
// Original gfx1030 Wave32 instruction stream for metadata identification.
s_mov_b32 s12, 0x89abcdef
v_add_f32_e32 v4, v5, v9
s_cmp_lt_u32 s12, s13
ds_read_b32 v8, v2 offset:12
v_mov_b32_dpp v9, v6 quad_perm:[2,3,0,1] row_mask:0xf bank_mask:0xf
s_add_u32 s16, s17, s18
buffer_store_dword v7, off, s[4:7], 0
v_fma_f32 v5, v8, v10, 0x3f800003
s_movk_i32 s19, 0x2345
global_load_dword v3, v[10:11], off
v_cmp_lt_u32_e32 vcc_lo, v3, v7
s_load_dword s20, s[22:23], 0x20
v_pk_add_f16 v3, v4, v6
s_waitcnt vmcnt(0)
