// SPDX-License-Identifier: GPL-3.0-or-later
// Synthetic Wave32 adapter for the LLVM cross-check in fixtures/llvm-xcheck.
// The four instructions between XCHECK CORE BEGIN/END preserve the opcode,
// constant and dataflow sequence emitted by llc 23.1.6 for xcheck_gfx1030, The surrounding moves and
// memory instructions are Xenora's documented synthetic binding-ABI adapter.
// Because v0 is Xenora's reserved lane-index register, the adapter relocates
// the compiler's input v1 to v3 and its result/intermediate v0 to v4 without
// changing the opcode, constant or dataflow sequence.
.text
buffer_load_dword v1, v0, s[0:3], 0 idxen
s_waitcnt vmcnt(0)
v_mov_b32_e32 v3, v1                    // compiler input register: v1 -> v3
// XCHECK CORE BEGIN
v_mul_u32_u24_e32 v4, 0x123456, v3
v_xor_b32_e32 v4, v4, v3
v_mul_u32_u24_e32 v3, 0x15944c, v4
v_xor_b32_e32 v4, v3, v4
// XCHECK CORE END
v_mov_b32_e32 v1, v4                    // compiler result register: v0 -> v4
buffer_store_dword v1, v0, s[4:7], 0 idxen
s_waitcnt_vscnt null, 0
s_endpgm
