// SPDX-License-Identifier: GPL-3.0-or-later
// One instruction per line for each family and extension word; checked against llvm-mc.
s_mov_b32 s0, 0x12345678
s_add_u32 s0, s1, 0x1234
s_cmp_eq_u32 s0, 0xdeadbeef
s_movk_i32 s0, 0x1234
s_setreg_imm32_b32 hwreg(HW_REG_MODE), 0x0
s_load_dwordx4 s[0:3], s[4:5], 0x10
s_nop 0
s_endpgm
v_mov_b32 v0, 0x12345678
v_mov_b32_sdwa v0, v1 dst_sel:BYTE_0 dst_unused:UNUSED_PAD src0_sel:DWORD
v_mov_b32_dpp v0, v1 quad_perm:[1,0,3,2] row_mask:0xf bank_mask:0xf
v_mov_b32_dpp v0, v1 dpp8:[1,0,3,2,5,4,7,6]
v_mov_b32_dpp v0, v1 dpp8:[1,0,3,2,5,4,7,6] fi:1
v_add_f32_sdwa v0, v1, v2 dst_sel:DWORD dst_unused:UNUSED_PAD src0_sel:WORD_1 src1_sel:DWORD
v_add_f32_dpp v0, v1, v2 row_shl:1 row_mask:0xf bank_mask:0xf
v_mul_legacy_f32_e32 v0, v1, v2
v_mul_legacy_f32_e64 v0, -v1, |v2|
v_med3_f32 v0, -v1, |v2|, -|v3|
v_cmp_eq_u32_e32 vcc_lo, 0x1234, v1
v_cmp_eq_u32_sdwa s0, v1, v2 src0_sel:DWORD src1_sel:DWORD
v_fmamk_f32 v0, v1, 0x40000000, v2
v_fmaak_f32 v0, v1, v2, 0x40000000
v_fma_f32 v0, v1, v2, 0x3f800001
v_add_co_u32 v0, s0, v1, v2
v_readlane_b32 s0, v1, 0
v_pk_add_f16 v0, v1, v2
v_pk_fma_f16 v0, v1, v2, 0x3c01
v_interp_p1_f32 v0, v1, attr0.x
ds_write_b32 v0, v1
ds_read_b32 v0, v1 offset:16
buffer_load_dword v0, off, s[0:3], 0
tbuffer_load_format_x v0, off, s[0:3], 0 format:[BUF_FMT_32_FLOAT]
tbuffer_load_format_d16_x v0, off, s[0:3], 0 format:[BUF_FMT_32_FLOAT]
tbuffer_store_format_d16_xyzw v[0:1], off, s[0:3], 0 format:[BUF_FMT_32_FLOAT]
image_sample v[0:3], v[0:1], s[0:7], s[8:11] dmask:0xf dim:SQ_RSRC_IMG_2D
image_sample v[0:3], [v0, v2], s[0:7], s[8:11] dmask:0xf dim:SQ_RSRC_IMG_2D
image_sample_c v[0:3], [v0, v2, v4, v6], s[0:7], s[8:11] dmask:0xf dim:SQ_RSRC_IMG_2D_ARRAY
image_msaa_load v[0:3], v[0:2], s[0:7] dmask:0xf dim:SQ_RSRC_IMG_2D_MSAA unorm
flat_load_dword v0, v[0:1]
global_load_dword v0, v[0:1], off
scratch_load_dword v0, v1, off
exp mrt0 v0, v0, v0, v0 done
