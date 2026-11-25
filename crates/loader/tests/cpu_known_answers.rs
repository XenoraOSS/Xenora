// SPDX-License-Identifier: GPL-3.0-or-later

//! Architectural known answers for the guest CPU (issue #155).
//!
//! Expectations were derived from the Intel 64 and IA-32 Architectures Software
//! Developer's Manual, revision 093 (September 2026), not from Xenora. Encodings
//! were produced by Apple clang's x86-64 assembler and read back with `objdump`.
//! Nothing here was compared with x86 hardware. For MXCSR flags that the Intel text
//! states less directly, the AMD64 Architecture Programmer's Manual, Volume 1,
//! revision 3.23 (October 2020) was consulted as well, cited as "APM".
//!
//! Chronology: 700 table cases were written before any Xenora CPU source was
//! opened; one input and a few MXCSR fields were tightened later. The other cases
//! and the sweeps were written after the public interface and small parts of the
//! decoder and DBT had been seen, some of them after the first run. Review
//! follow-ups came after the source behind the first four defects had been read.
//! The author is the same AI model as the author of the code under test.
//!
//! Tests named `defect_*` state what the manual requires where the engines differ
//! today. They are ignored with a reason, so they never count as passed; run them
//! with `--ignored`.
//!
//! # Table format
//!
//! ```text
//! @ assembly text
//!   bytes of each instruction, `!` before the one expected to fault
//!   SDM volume and section
//! case_name: initial state => final state ! fault
//! ```
//!
//! Citations read "volume section": `1 4.8.4` is Volume 1, Section 4.8.4 and
//! `2B SHLD` is the SHLD entry of Volume 2B. All numbers are hexadecimal.
//!
//! * `rax=` and so on set a register. `x3=` sets an XMM register by lane, lane 0
//!   first: `b:`, `w:`, `d:`, `q:`, or only its low 32 (`s:`) or 64 (`r:`) bits.
//! * `m10=` is memory at `D+10`; `fs8=` and `gs8=` are relative to the segment
//!   bases. Bytes are in address order.
//! * `f=CpAz` lists status flags: upper case set, lower case clear. On the right
//!   a flag that is missing is undefined by the manual and is not compared;
//!   `f=*` and `keep=` require the initial value. A case without `f=` on the
//!   left runs with all flags clear and again with all flags set.
//! * `mx=` is MXCSR; `mx=~20:1b88` ignores the bits in `20`. `=?` ignores a value.
//! * `fcw=`, `fsw=` and `ftw=` are the x87 control, status and tag words. Every
//!   case starts with all three away from their FNINIT values.
//! * Symbols: `D` writable data, `U` the unmapped page after it, `R` read-only
//!   data, `S` stack, `C` code, `FSB` and `GSB` segment bases.
//! * Faults: `DE`, `GP`, `UD`, `XM`, and page faults `PFR`, `PFW`, `PFX` with
//!   their linear address. State must be as before the faulting instruction.
//!
//! Anything a case does not mention must keep its initial value.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use xenora_loader::cpu::{CpuState, ExitReason, Interpreter, Mxcsr, RFlags, X87State};
use xenora_loader::dbt::{DbtEngine, is_native};
use xenora_loader::memory::{GuestMemory, MemoryError, MemoryPermissions};
use xenora_loader::x86_decode::decode;

const CODE: u64 = 0x40_0000;
const DATA: u64 = 0x50_0000;
const DATA_LEN: usize = 0x2000;
const UNMAPPED: u64 = DATA + DATA_LEN as u64;
const READ_ONLY: u64 = 0x52_0000;
const STACK_PAGE: u64 = 0x54_0000;
const STACK: u64 = STACK_PAGE + 0x800;
const FS_BASE: u64 = 0x60_0000;
const GS_BASE: u64 = 0x62_0000;
const PAGE: usize = 0x1000;

const CF: u8 = 1;
const PF: u8 = 2;
const AF: u8 = 4;
const ZF: u8 = 8;
const SF: u8 = 16;
const OF: u8 = 32;
const ALL_FLAGS: u8 = 63;

const REGISTERS: [&str; 16] = [
    "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12", "r13",
    "r14", "r15",
];

// ---------------------------------------------------------------------------
// Case tables
// ---------------------------------------------------------------------------

const ADD_SUB_FLAGS: &str = r"
@ add al, bl
  00d8
  SDM 2A ADD; 1 3.4.3.1
add8_signed_overflow: rax=1122334455667f7f rbx=1 => rax=1122334455667f80 f=cpAzSO
add8_wrap_to_zero: rax=11223344556600ff rbx=1 => rax=1122334455660000 f=CPAZso
add8_neg_neg_overflow: rax=80 rbx=80 => rax=0 f=CPaZsO
add8_half_carry_only: rax=0f rbx=1 => rax=10 f=cpAzso
add8_even_parity: rax=1 rbx=2 => rax=3 f=cPazso
add8_both_max_positive: rax=7f rbx=7f => rax=fe f=cpAzSO
add8_carry_and_overflow_to_positive: rax=80 rbx=ff => rax=7f f=CpazsO
@ add ax, bx
  6601d8
  SDM 2A ADD; 1 3.4.1.1
add16_signed_overflow_preserves_upper48: rax=1122334455667fff rbx=1 => rax=1122334455668000 f=cPAzSO
add16_carry_does_not_enter_bit16: rax=112233445566ffff rbx=1 => rax=1122334455660000 f=CPAZso
@ add ax, bx
  6601d8
  SDM 2A ADD; 1 3.4.3.1
add16_parity_is_low_byte_only: rax=100 rbx=0 => rax=100 f=cPazso
@ add eax, ebx
  01d8
  SDM 2A ADD; 1 3.4.1.1
add32_signed_overflow_zero_extends: rax=ffffffff7fffffff rbx=ffffffff00000001 => rax=80000000 f=cPAzSO
@ add eax, ebx
  01d8
  SDM 2A ADD; 1 3.4.3.1
add32_two_minimums: rax=ffffffff80000000 rbx=80000000 => rax=0 f=CPaZsO
@ add rax, rbx
  4801d8
  SDM 2A ADD; 1 3.4.3.1
add64_signed_overflow: rax=7fffffffffffffff rbx=1 => rax=8000000000000000 f=cPAzSO
add64_unsigned_wrap: rax=ffffffffffffffff rbx=1 => rax=0 f=CPAZso
@ add rax, -1
  4883c0ff
  SDM 2A ADD; 2A 2.2.1.5
add64_imm8_is_sign_extended: rax=0 => rax=ffffffffffffffff f=cPazSo
@ add rax, 0xffffffff80000000
  480500000080
  SDM 2A ADD; 2A 2.2.1.5
add64_imm32_is_sign_extended: rax=80000000 => rax=0 f=CPaZso
@ add byte ptr [rdi+1], 0x80
  80470180
  SDM 2A ADD
add8_memory_destination: rdi=D m0=017f02 => m0=01ff02 f=cPazSo
@ add rax, rax
  4801c0
  SDM 2A ADD
add64_register_aliased_doubles: rax=8000000000000001 => rax=2 f=CpazsO
@ sub al, bl
  28d8
  SDM 2B SUB; 1 3.4.3.1
sub8_borrow_from_zero: rax=0 rbx=1 => rax=ff f=CPAzSo
sub8_min_minus_one: rax=80 rbx=1 => rax=7f f=cpAzsO
sub8_positive_minus_negative: rax=7f rbx=ff => rax=80 f=CpazSO
sub8_zero_minus_min: rax=0 rbx=80 => rax=80 f=CpazSO
sub8_half_borrow_only: rax=10 rbx=1 => rax=0f f=cPAzso
@ sub ax, bx
  6629d8
  SDM 2B SUB; 1 3.4.1.1
sub16_borrow_preserves_upper48: rax=1122334455660000 rbx=1 => rax=112233445566ffff f=CPAzSo
@ sub eax, ebx
  29d8
  SDM 2B SUB; 1 3.4.1.1
sub32_borrow_zero_extends: rax=ffffffff00000000 rbx=1 => rax=ffffffff f=CPAzSo
@ sub rax, rbx
  4829d8
  SDM 2B SUB; 1 3.4.3.1
sub64_min_minus_one: rax=8000000000000000 rbx=1 => rax=7fffffffffffffff f=cPAzsO
sub64_zero_minus_min: rax=0 rbx=8000000000000000 => rax=8000000000000000 f=CPazSO
@ sub rax, rax
  4829c0
  SDM 2B SUB
sub64_self_is_zero: rax=deadbeefcafef00d => rax=0 f=cPaZso
@ cmp eax, ebx
  39d8
  SDM 2A CMP
cmp32_writes_no_register: rax=ffffffff00000005 rbx=ffffffff00000005 => f=cPaZso
@ cmp ax, -1
  6683f8ff
  SDM 2A CMP
cmp16_imm8_sign_extended: rax=ffff => f=cPaZso
";

const CARRY_CHAINS: &str = r"
@ adc al, bl
  10d8
  SDM 2A ADC; 1 3.4.3.1
adc8_all_ones_plus_carry: rax=ff rbx=ff f=C => rax=ff f=CPAzSo
adc8_carry_alone_overflows: rax=7f rbx=0 f=C => rax=80 f=cpAzSO
@ adc al, 0xff
  14ff
  SDM 2A ADC; 1 3.4.3.1
adc8_source_plus_carry_wraps: rax=0 f=C => rax=0 f=CPAZso
@ adc al, bl
  10d8
  SDM 2A ADC
adc8_without_carry: rax=ff rbx=0 f= => rax=ff f=cPazSo
@ adc ax, bx
  6611d8
  SDM 2A ADC; 1 3.4.1.1
adc16_carry_wraps: rax=112233445566ffff rbx=0 f=C => rax=1122334455660000 f=CPAZso
@ adc eax, ebx
  11d8
  SDM 2A ADC; 1 3.4.1.1
adc32_carry_wraps_zero_extends: rax=ffffffffffffffff rbx=0 f=C => rax=0 f=CPAZso
@ adc rax, rbx
  4811d8
  SDM 2A ADC; 1 3.4.3.1
adc64_max_plus_max_plus_carry: rax=7fffffffffffffff rbx=7fffffffffffffff f=C => rax=ffffffffffffffff f=cPAzSO
@ adc qword ptr [rdi], 0
  48831700
  SDM 2A ADC
adc64_memory_destination: rdi=D m0=ffffffffffffffff f=C => m0=0000000000000000 f=CPAZso
@ sbb al, bl
  18d8
  SDM 2B SBB; 1 3.4.3.1
sbb8_zero_minus_ff_minus_borrow: rax=0 rbx=ff f=C => rax=0 f=CPAZso
sbb8_borrow_alone: rax=0 rbx=0 f=C => rax=ff f=CPAzSo
sbb8_borrow_alone_overflows: rax=80 rbx=7f f=C => rax=0 f=cPAZsO
@ sbb eax, eax
  19c0
  SDM 2B SBB; 1 3.4.1.1
sbb32_self_broadcasts_borrow: rax=ffffffff00001234 f=C => rax=ffffffff f=CPAzSo
@ sbb eax, eax
  19c0
  SDM 2B SBB
sbb32_self_without_borrow: rax=ffffffff00001234 f= => rax=0 f=cPaZso
@ sbb rax, 0
  4883d800
  SDM 2B SBB; 1 3.4.3.1
sbb64_min_minus_zero_minus_borrow: rax=8000000000000000 f=C => rax=7fffffffffffffff f=cPAzsO
@ add rax, rcx; adc rdx, rbx
  4801c8 4811da
  SDM 2A ADC
add128_carry_into_high_half: rax=ffffffffffffffff rdx=7fffffffffffffff rcx=1 rbx=0 => rax=0 rdx=8000000000000000 f=cPAzSO
@ sub rax, rcx; sbb rdx, rbx
  4829c8 4819da
  SDM 2B SBB
sub128_borrow_from_high_half: rax=0 rdx=8000000000000000 rcx=1 rbx=0 => rax=ffffffffffffffff rdx=7fffffffffffffff f=cPAzsO
@ clc; 1: mov rax, [rsi]; adc [rdi], rax; lea rsi, [rsi+8]; lea rdi, [rdi+8]; dec ecx; jnz 1b
  f8 488b06 481107 488d7608 488d7f08 ffc9 75ee
  SDM 2A ADC; 2A DEC; 2A LEA
add256_loop_dec_preserves_carry: rsi=D+20 rdi=D rcx=4 m0=ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff m20=0100000000000000000000000000000000000000000000000000000000000000 => rsi=D+40 rdi=D+20 rax=0 rcx=0 f=CPaZso m0=0000000000000000000000000000000000000000000000000000000000000000
@ sub rax, r8; sbb rbx, r9; sbb rcx, r10
  4c29c0 4c19cb 4c19d1
  SDM 2B SBB
sub192_mixed_borrow_chain: rax=0 rbx=0 rcx=0 r8=1 r9=0 r10=0 => rax=ffffffffffffffff rbx=ffffffffffffffff rcx=ffffffffffffffff f=CPAzSo
@ inc al
  fec0
  SDM 2A INC
inc8_overflow_keeps_carry_set: rax=7f f=C => rax=80 f=CpAzSO
inc8_wrap_keeps_carry_clear: rax=ff f= => rax=0 f=cPAZso
@ inc eax
  ffc0
  SDM 2A INC; 1 3.4.1.1
inc32_wrap_zero_extends: rax=ffffffffffffffff f=C => rax=0 f=CPAZso
@ dec al
  fec8
  SDM 2A DEC
dec8_overflow_keeps_carry_clear: rax=80 f= => rax=7f f=cpAzsO
@ dec rax
  48ffc8
  SDM 2A DEC
dec64_borrow_keeps_carry_set: rax=0 f=C => rax=ffffffffffffffff f=CPAzSo
@ dec word ptr [rdi]
  66ff0f
  SDM 2A DEC
dec16_memory: rdi=D m0=000000 f= => m0=ffff00 f=cPAzSo
@ neg al
  f6d8
  SDM 2B NEG
neg8_zero: rax=0 => rax=0 f=cPaZso
neg8_min_overflows: rax=80 => rax=80 f=CpazSO
neg8_one: rax=1 => rax=ff f=CPAzSo
@ neg rax
  48f7d8
  SDM 2B NEG
neg64_min: rax=8000000000000000 => rax=8000000000000000 f=CPazSO
@ neg eax
  f7d8
  SDM 2B NEG; 1 3.4.1.1
neg32_zero_extends: rax=ffffffff00000001 => rax=ffffffff f=CPAzSo
@ not eax
  f7d0
  SDM 2B NOT; 1 3.4.1.1
not32_zero_extends_no_flags: rax=ffffffff0000ffff => rax=ffff0000 f=*
@ not ah
  f6d4
  SDM 2B NOT
not8_high_byte: rax=1122334455660f77 => rax=112233445566f077 f=*
@ 1: mov rax, [rsi]; adc [rdi], rax; lea rsi, [rsi+8]; lea rdi, [rdi+8]; dec ecx; jnz 1b
  488b06 481107 488d7608 488d7f08 ffc9 75ee
  SDM 2A ADC; 2A DEC; 2A LEA
add256_loop_without_clc: rsi=D+20 rdi=D rcx=4 f=C m0=ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff m20=0000000000000000000000000000000000000000000000000000000000000000 => rsi=D+40 rdi=D+20 rax=0 rcx=0 f=CPaZso m0=0000000000000000000000000000000000000000000000000000000000000000
";

const LOGIC: &str = r"
@ and rax, -16
  4883e0f0
  SDM 2A AND; 2A 2.2.1.5
and64_imm8_sign_extended: rax=ffffffffffffffff => rax=fffffffffffffff0 f=cPzSo
@ and eax, 0x80000000
  2500000080
  SDM 2A AND; 1 3.4.1.1
and32_imm32_zero_extends: rax=ffffffffffffffff => rax=80000000 f=cPzSo
@ and ax, bx
  6621d8
  SDM 2A AND; 1 3.4.1.1
and16_preserves_upper48: rax=1122334455668001 rbx=7ffe => rax=1122334455660000 f=cPZso
@ or al, bl
  08d8
  SDM 2B OR
or8_sign: rax=1122334455667700 rbx=81 => rax=1122334455667781 f=cPzSo
@ or rax, 0xffffffff80000000
  480d00000080
  SDM 2B OR; 2A 2.2.1.5
or64_imm32_sign_extended: rax=1 => rax=ffffffff80000001 f=cpzSo
@ xor eax, eax
  31c0
  SDM 2B XOR; 1 3.4.1.1
xor32_self_zeroes_all_64: rax=ffffffffffffffff => rax=0 f=cPZso
@ xor al, bl
  30d8
  SDM 2B XOR
xor8_parity_odd: rax=ff rbx=80 => rax=7f f=cpzso
@ test al, bl
  84d8
  SDM 2B TEST
test8_sign_no_write: rax=ff rbx=80 => f=cpzSo
@ test rax, 0xffffffff80000000
  48a900000080
  SDM 2B TEST
test64_imm32_sign_extended: rax=4000000000000000 => f=cPzso
@ test eax, 0xffffffff
  a9ffffffff
  SDM 2B TEST
test32_imm_ignores_upper32: rax=ffffffff00000000 => f=cPZso
";

const PARTIAL_REGISTERS: &str = r"
@ mov ah, 0x12
  b412
  SDM 1 3.4.1.1 Table 3-2
mov_ah_leaves_the_rest: rax=8877665544332211 => rax=8877665544331211 f=*
@ mov spl, 0x12
  40b412
  SDM 1 3.4.1.1 Table 3-2; 2A 2.2.1.2
mov_spl_needs_rex_and_is_not_ah: rsp=S => rsp=S+12 f=*
@ mov sil, 0x5a
  40b65a
  SDM 1 3.4.1.1 Table 3-2
mov_sil_is_low_byte_of_rsi: rsi=8877665544332211 => rsi=887766554433225a f=*
@ mov dh, 0x5a
  b65a
  SDM 1 3.4.1.1 Table 3-2
mov_dh_without_rex_is_not_sil: rdx=8877665544332211 rsi=0102030405060708 => rdx=8877665544335a11 f=*
@ mov bpl, dil
  4088fd
  SDM 1 3.4.1.1 Table 3-2
mov_bpl_is_low_byte_of_rbp: rbp=8877665544332211 rdi=aabbccddeeff0099 => rbp=8877665544332299 f=*
@ mov ch, bh
  88fd
  SDM 1 3.4.1.1 Table 3-2
mov_ch_without_rex_is_not_bpl: rcx=8877665544332211 rbx=aabbccddeeff0099 => rcx=8877665544330011 f=*
@ mov r8b, r15b
  4588f8
  SDM 1 3.4.1.1
mov_r8b_preserves_upper56: r8=8877665544332211 r15=aabbccddeeff0099 => r8=8877665544332299 f=*
@ mov r8w, r15w
  664589f8
  SDM 1 3.4.1.1
mov_r8w_preserves_upper48: r8=8877665544332211 r15=aabbccddeeff0099 => r8=8877665544330099 f=*
@ mov r8d, r15d
  4589f8
  SDM 1 3.4.1.1
mov_r8d_clears_upper32: r8=8877665544332211 r15=aabbccddeeff0099 => r8=eeff0099 f=*
@ mov eax, eax
  89c0
  SDM 1 3.4.1.1
mov_eax_eax_clears_upper32: rax=8877665544332211 => rax=44332211 f=*
@ mov rax, 0xffffffff80000000
  48c7c000000080
  SDM 2B MOV; 2A 2.2.1.5
mov_rax_imm32_sign_extends: rax=0 => rax=ffffffff80000000 f=*
@ mov eax, 0x80000000
  b800000080
  SDM 2B MOV; 1 3.4.1.1
mov_eax_imm32_zero_extends: rax=ffffffffffffffff => rax=80000000 f=*
@ movabs rax, 0x8000000000000001
  48b80100000000000080
  SDM 2B MOV; 2A 2.2.1.5
movabs_imm64: rax=0 => rax=8000000000000001 f=*
@ xchg ah, al
  86e0
  SDM 2B XCHG; 1 3.4.1.1 Table 3-2
xchg_ah_al: rax=8877665544332211 => rax=8877665544331122 f=*
@ xchg eax, ebx
  93
  SDM 2B XCHG; 1 3.4.1.1
xchg_eax_ebx_clears_both_uppers: rax=1111111122222222 rbx=3333333344444444 => rax=44444444 rbx=22222222 f=*
@ xchg r9w, ax
  664191
  SDM 2B XCHG; 1 3.4.1.1
xchg_ax_r9w_preserves_both_uppers: rax=1111111122222222 r9=3333333344445555 => rax=1111111122225555 r9=3333333344442222 f=*
@ xchg rax, r8
  4990
  SDM 2B XCHG; 2A 2.2.1
xchg_rax_r8_is_not_nop: rax=1 r8=2 => rax=2 r8=1 f=*
@ nop
  90
  SDM 2B NOP
nop_leaves_rax_upper_intact: rax=8877665544332211 => f=*
@ xchg [rdi], rax
  488707
  SDM 2B XCHG
xchg_memory: rdi=D rax=0102030405060708 m0=1122334455667788 => rax=8877665544332211 m0=0807060504030201 f=*
@ add bh, cl
  00cf
  SDM 2A ADD; 1 3.4.1.1
add_bh_carry_stays_in_byte: rbx=887766554433ff11 rcx=1 => rbx=8877665544330011 f=CPAZso
@ add ah, al
  00c4
  SDM 2A ADD; 1 3.4.1.1
add_ah_al_same_register: rax=887766554433807f => rax=887766554433ff7f f=cPazSo
@ movzx eax, bh
  0fb6c7
  SDM 2B MOVZX
movzx_eax_bh: rax=ffffffffffffffff rbx=112233445566f788 => rax=f7 f=*
@ movzx ax, bl
  660fb6c3
  SDM 2B MOVZX; 1 3.4.1.1
movzx_ax_bl_preserves_upper48: rax=8877665544332211 rbx=ff => rax=88776655443300ff f=*
@ movsx ax, bl
  660fbec3
  SDM 2B MOVSX; 1 3.4.1.1
movsx_ax_bl_preserves_upper48: rax=8877665544332211 rbx=80 => rax=887766554433ff80 f=*
@ movsx eax, bl
  0fbec3
  SDM 2B MOVSX; 1 3.4.1.1
movsx_eax_bl_zero_extends_upper32: rax=8877665544332211 rbx=80 => rax=ffffff80 f=*
@ movsx rax, bx
  480fbfc3
  SDM 2B MOVSX
movsx_rax_bx: rax=0 rbx=8000 => rax=ffffffffffff8000 f=*
@ movsxd rax, ebx
  4863c3
  SDM 2B MOVSXD
movsxd_rax_ebx: rax=0 rbx=1111111180000000 => rax=ffffffff80000000 f=*
@ movzx r10d, word ptr [rsi]
  440fb716
  SDM 2B MOVZX
movzx_r10d_word_memory: rsi=D m0=feff r10=ffffffffffffffff => r10=fffe f=*
@ cbw
  6698
  SDM 2A CBW
cbw: rax=1122334455667780 => rax=112233445566ff80 f=*
@ cwde
  98
  SDM 2A CBW; 1 3.4.1.1
cwde: rax=1122334455668000 => rax=ffff8000 f=*
@ cdqe
  4898
  SDM 2A CBW
cdqe: rax=1122334480000000 => rax=ffffffff80000000 f=*
@ cwd
  6699
  SDM 2A CWD; 1 3.4.1.1
cwd_preserves_rdx_upper48: rax=8000 rdx=1122334455667788 => rdx=112233445566ffff f=*
@ cdq
  99
  SDM 2A CWD; 1 3.4.1.1
cdq_zero_extends_rdx: rax=ffffffff80000000 rdx=1122334455667788 => rdx=ffffffff f=*
@ cqo
  4899
  SDM 2A CWD
cqo_positive: rax=7fffffffffffffff rdx=1122334455667788 => rdx=0 f=*
@ bswap eax
  0fc8
  SDM 2A BSWAP; 1 3.4.1.1
bswap32_zero_extends: rax=ffffffff01020304 => rax=4030201 f=*
@ bswap r15
  490fcf
  SDM 2A BSWAP
bswap64_r15: r15=0102030405060708 => r15=0807060504030201 f=*
@ setc ah
  0f92c4
  SDM 2B SETcc; 1 3.4.1.1
setc_ah_only_changes_ah: rax=8877665544332211 f=C => rax=8877665544330111 f=*
@ cmovz eax, ebx
  0f44c3
  SDM 2A CMOVcc; 1 3.4.1.1
cmov32_false_still_zero_extends: rax=ffffffff11112222 rbx=3 f= => rax=11112222 f=*
@ cmovz ax, bx
  660f44c3
  SDM 2A CMOVcc; 1 3.4.1.1
cmov16_false_changes_nothing: rax=ffffffff11112222 rbx=3 f= => f=*
@ cmovz rax, rbx
  480f44c3
  SDM 2A CMOVcc
cmov64_true: rax=1 rbx=8000000000000000 f=Z => rax=8000000000000000 f=*
@ lea eax, [rbx+rcx]
  8d040b
  SDM 2A LEA; 1 3.4.1.1
lea32_truncates_and_zero_extends: rax=ffffffffffffffff rbx=ffffffff rcx=1 => rax=0 f=*
@ lea ax, [rbx+rcx]
  668d040b
  SDM 2A LEA; 1 3.4.1.1
lea16_preserves_upper48: rax=1122334455667788 rbx=12345 rcx=1 => rax=1122334455662346 f=*
@ lea rax, [ebx+ecx*8+4]
  67488d44cb04
  SDM 2A LEA (Table 3-56 rows 64/32)
lea64_addr32_truncates_effective_address: rax=0 rbx=ffffffffffffffff rcx=1 => rax=0b f=*
@ lea rax, [rbx+rcx*8-128]
  488d44cb80
  SDM 2A LEA; 2A 2.1.5
lea64_negative_scaled: rbx=100 rcx=ffffffffffffffff => rax=78 f=*
@ lea rax, [rbx+r12*4]
  4a8d04a3
  SDM 2A 2.2.1.2
lea_r12_index_allowed: rbx=10 r12=3 => rax=1c f=*
@ lea rax, [r13]
  498d4500
  SDM 2A 2.2.1.2
lea_r13_base_disp0: r13=1234567812345678 => rax=1234567812345678 f=*
@ lea rax, [rip+0x10]
  488d0510000000
  SDM 2A 2.2.1.6
lea_rip_relative: rax=0 => rax=C+17 f=*
@ mov rax, [rip+0xffff9]
  488b05f9ff0f00
  SDM 2A 2.2.1.6
load_rip_relative: m0=0102030405060708 => rax=0807060504030201 f=*
@ mov dword ptr [rip+0xffff6], 0x11223344
  c705f6ff0f0044332211
  SDM 2A 2.2.1.6
store_rip_relative_with_imm32: m0=00 => m0=44332211 f=*
";

const EXCHANGE: &str = r"
@ xadd eax, ebx
  0fc1d8
  SDM 2B XADD; 1 3.4.1.1
xadd32_reg_reg: rax=ffffffff00000001 rbx=ffffffff00000002 => rax=3 rbx=1 f=cPazso
@ xadd al, ah
  0fc0e0
  SDM 2B XADD
xadd8_al_ah: rax=1122334455660180 => rax=1122334455668081 f=cPazSo
@ xadd rax, rax
  480fc1c0
  SDM 2B XADD
xadd64_same_register_doubles: rax=5 => rax=0a f=cPazso
@ lock xadd [rdi], rbx
  f0480fc11f
  SDM 2B XADD
xadd64_memory: rdi=D rbx=1 m0=ffffffffffffffff => rbx=ffffffffffffffff m0=0000000000000000 f=CPAZso
@ cmpxchg ebx, ecx
  0fb1cb
  SDM 2A CMPXCHG; 1 3.4.1.1
cmpxchg32_equal: rax=ffffffff00000005 rbx=ffffffff00000005 rcx=ffffffff00000009 => rbx=9 f=cPaZso
@ cmpxchg bl, cl
  0fb0cb
  SDM 2A CMPXCHG
cmpxchg8_not_equal_loads_al: rax=1122334455667710 rbx=aabbccddeeff0001 rcx=99 => rax=1122334455667701 f=cPAzso
@ lock cmpxchg [rdi], rcx
  f0480fb10f
  SDM 2A CMPXCHG
cmpxchg64_memory_not_equal: rdi=D rax=1 rcx=7 m0=0200000000000000 => rax=2 f=CPAzSo
@ lock cmpxchg8b [rdi]
  f00fc70f
  SDM 2A CMPXCHG8B; 1 3.4.1.1
cmpxchg8b_equal: rdi=D rdx=ffffffff11111111 rax=ffffffff22222222 rcx=33333333 rbx=44444444 m0=2222222211111111 f=CPASO => m0=4444444433333333 f=CPAZSO
cmpxchg8b_not_equal: rdi=D rdx=ffffffff11111111 rax=ffffffff22222222 rcx=33333333 rbx=44444444 m0=2322222211111111 f=Z => rax=22222223 rdx=11111111 f=cpazso
@ lock cmpxchg16b [rdi]
  f0480fc70f
  SDM 2A CMPXCHG16B
cmpxchg16b_equal: rdi=D rdx=2 rax=1 rcx=4 rbx=3 m0=01000000000000000200000000000000 f= => m0=03000000000000000400000000000000 f=cpaZso
cmpxchg16b_not_equal: rdi=D rdx=2 rax=1 rcx=4 rbx=3 m0=01000000000000000300000000000000 f=ZC => rdx=3 f=Cpazso
@ xadd [rdi], rbx
  480fc11f
  SDM 2B XADD
xadd64_memory_without_lock: rdi=D rbx=1 m0=ffffffffffffffff => rbx=ffffffffffffffff m0=0000000000000000 f=CPAZso
@ cmpxchg [rdi], rcx
  480fb10f
  SDM 2A CMPXCHG
cmpxchg64_memory_not_equal_without_lock: rdi=D rax=1 rcx=7 m0=0200000000000000 => rax=2 f=CPAzSo
cmpxchg64_memory_equal_without_lock: rdi=D rax=2 rcx=7 m0=0200000000000000 => m0=0700000000000000 f=cPaZso
@ cmpxchg8b [rdi]
  0fc70f
  SDM 2A CMPXCHG8B; 1 3.4.1.1
cmpxchg8b_equal_without_lock: rdi=D rdx=ffffffff11111111 rax=ffffffff22222222 rcx=33333333 rbx=44444444 m0=2222222211111111 f=CPASO => m0=4444444433333333 f=CPAZSO
@ cmpxchg16b [rdi]
  480fc70f
  SDM 2A CMPXCHG16B
cmpxchg16b_equal_without_lock: rdi=D rdx=2 rax=1 rcx=4 rbx=3 m0=01000000000000000200000000000000 f= => m0=03000000000000000400000000000000 f=cpaZso
@ cmpxchg16b [rdi+8]
  !480fc74f08
  SDM 2A CMPXCHG16B
cmpxchg16b_unaligned_without_lock_is_gp: rdi=D => ! GP
@ .byte 0x87,0xd8
  87d8
  SDM 2B XCHG (87 /r); 1 3.4.1.1
xchg_modrm_eax_ebx_clears_both_uppers: rax=1111111122222222 rbx=3333333344444444 => rax=44444444 rbx=22222222 f=*
@ .byte 0x66,0x44,0x87,0xc8
  664487c8
  SDM 2B XCHG (87 /r); 1 3.4.1.1
xchg_modrm_ax_r9w_preserves_both_uppers: rax=1111111122222222 r9=3333333344445555 => rax=1111111122225555 r9=3333333344442222 f=*
";

const FLAG_MOVES: &str = r"
@ lahf
  9f
  SDM 2A LAHF
lahf: rax=8877665544332211 f=CPASO => rax=8877665544339711 f=*
lahf_zero: rax=8877665544332211 f=Z => rax=8877665544334211 f=*
lahf_carry_only: rax=ffffffffffffffff f=C => rax=ffffffffffff03ff f=*
lahf_parity_only: rax=ffffffffffffffff f=P => rax=ffffffffffff06ff f=*
lahf_adjust_only: rax=ffffffffffffffff f=A => rax=ffffffffffff12ff f=*
lahf_sign_only: rax=ffffffffffffffff f=S => rax=ffffffffffff82ff f=*
lahf_does_not_store_of: rax=ffffffffffffffff f=O => rax=ffffffffffff02ff f=*
@ rex.w lahf
  489f
  SDM 2A LAHF; 2A 2.2.1.2
lahf_rex_w_changes_nothing: rax=ffffffffffffffff f=CO => rax=ffffffffffff03ff f=*
@ sahf; lahf
  9e 9f
  SDM 2B SAHF
sahf_ignores_bits_1_3_5: rax=ff00 f=O => rax=d700 f=CPAZSO
@ sahf
  9e
  SDM 2B SAHF
sahf_clear_keeps_of: rax=2800 f=OC => f=cpazsO
sahf_carry_only: rax=100 f=O => f=CpazsO
sahf_parity_only: rax=400 f=O => f=cPazsO
sahf_adjust_only: rax=1000 f=O => f=cpAzsO
sahf_zero_only: rax=4000 f=O => f=cpaZsO
sahf_sign_only: rax=8000 f=O => f=cpazSO
sahf_reads_only_ah: rax=ffffffffffff00ff f=CPAZS => f=cpazso
@ stc
  f9
  SDM 2B STC
stc: f=PAZSO => f=CPAZSO
@ clc
  f8
  SDM 2A CLC
clc: f=CPAZSO => f=cPAZSO
@ cmc; cmc; cmc
  f5 f5 f5
  SDM 2A CMC
cmc: f=CZ => f=cpaZso
@ pushfq; add al, al; popfq
  9c 00c0 9d
  SDM 2B PUSHF; 2B POPF
pushfq_popfq_status_round_trip: rax=40 f=CPAZS => rax=80 f=CPAZSo
";

const SHIFT_ROTATE: &str = r"
@ shl al, 1
  d0e0
  SDM 2B SAL/SHL
shl8_by1_overflow: rax=1122334455667781 => rax=1122334455667702 f=CpzsO
shl8_by1_sign_change: rax=40 => rax=80 f=cpzSO
shl8_by1_no_overflow: rax=c0 => rax=80 f=CpzSo
@ shl al, 7
  c0e007
  SDM 2B SAL/SHL
shl8_by7_last_bit_out: rax=3 => rax=80 f=CpzS
@ shl al, 8
  c0e008
  SDM 2B SAL/SHL
shl8_by8_result_zero_cf_undefined: rax=11223344556677ff => rax=1122334455667700 f=PZs
@ shl ax, 16
  66c1e010
  SDM 2B SAL/SHL
shl16_by16_result_zero_cf_undefined: rax=112233445566ffff => rax=1122334455660000 f=PZs
@ shl eax, cl
  d3e0
  SDM 2B SAL/SHL; 1 3.4.1.1
shl32_cl32_is_count_zero: rax=ffffffff80000001 rcx=20 => rax=80000001 f=*
@ shl eax, cl
  d3e0
  SDM 2B SAL/SHL
shl32_cl33_is_count_one: rax=80000001 rcx=21 => rax=2 f=CpzsO
@ shl eax, 31
  c1e01f
  SDM 2B SAL/SHL
shl32_by31: rax=3 => rax=80000000 f=CPzS
@ shl rax, 63
  48c1e03f
  SDM 2B SAL/SHL
shl64_by63: rax=1 => rax=8000000000000000 f=cPzS
@ shl rax, cl
  48d3e0
  SDM 2B SAL/SHL
shl64_cl64_is_count_zero: rax=8000000000000001 rcx=40 => f=*
shl64_cl65_is_count_one: rax=8000000000000001 rcx=ff41 => rax=2 f=CpzsO
@ shl al, cl
  d2e0
  SDM 2B SAL/SHL
shl8_cl_ff_masks_to_31: rax=ff rcx=ff => rax=0 f=PZs
@ shr al, 1
  d0e8
  SDM 2B SHR
shr8_by1_of_is_old_msb: rax=81 => rax=40 f=CpzsO
shr8_by1_positive: rax=7e => rax=3f f=cPzso
@ shr ax, 15
  66c1e80f
  SDM 2B SHR
shr16_by15: rax=112233445566c000 => rax=1122334455660001 f=Cpzs
@ shr eax, 31
  c1e81f
  SDM 2B SHR; 1 3.4.1.1
shr32_by31_zero_extends: rax=ffffffff80000000 => rax=1 f=cpzs
@ shr rax, 63
  48c1e83f
  SDM 2B SHR
shr64_by63_last_bit_out: rax=c000000000000000 => rax=1 f=Cpzs
@ sar al, 1
  d0f8
  SDM 2B SAR
sar8_by1_clears_of: rax=81 => rax=c0 f=CPzSo
@ sar al, 7
  c0f807
  SDM 2B SAR
sar8_by7: rax=80 => rax=ff f=cPzS
@ sar al, 8
  c0f808
  SDM 2B SAR
sar8_by8_cf_is_sign: rax=80 => rax=ff f=CPzS
@ sar al, 31
  c0f81f
  SDM 2B SAR
sar8_by31_positive: rax=7f => rax=0 f=cPZs
@ sar ax, 20
  66c1f814
  SDM 2B SAR
sar16_by20_negative: rax=1122334455668000 => rax=112233445566ffff f=CPzS
@ sar eax, 31
  c1f81f
  SDM 2B SAR; 1 3.4.1.1
sar32_by31_zero_extends: rax=ffffffff80000000 => rax=ffffffff f=cPzS
@ sar rax, 2
  48c1f802
  SDM 2B SAR (-9 by 2 bits example)
sar64_rounds_to_minus_infinity: rax=fffffffffffffff7 => rax=fffffffffffffffd f=CpzS
@ shl qword ptr [rdi], 1
  48d127
  SDM 2B SAL/SHL
shl64_memory_by1: rdi=D m0=0000000000000080 => m0=0000000000000000 f=CPZsO
@ rol al, 1
  d0c0
  SDM 2B ROL
rol8_by1: rax=80 => rax=1 f=CO keep=PAZS
@ rol al, 8
  c0c008
  SDM 2B ROL
rol8_by8_identity_updates_cf: rax=81 f= => rax=81 f=Cpazs
@ rol al, 9
  c0c009
  SDM 2B ROL
rol8_by9_is_rotate_by_one_with_of_undefined: rax=81 f= => rax=3 f=Cpazs
@ rol ax, 16
  66c1c010
  SDM 2B ROL
rol16_by16_identity_updates_cf: rax=1122334455668000 f=C => f=cpazs
@ rol eax, cl
  d3c0
  SDM 2B ROL; 1 3.4.1.1
rol32_cl32_affects_no_flags: rax=ffffffff80000001 rcx=20 => rax=80000001 f=*
@ rol rax, 63
  48c1c03f
  SDM 2B ROL
rol64_by63: rax=1 f= => rax=8000000000000000 f=cpazs
@ ror al, 1
  d0c8
  SDM 2B ROR
ror8_by1: rax=1 => rax=80 f=CO keep=PAZS
ror8_by1_two_top_bits_equal: rax=81 => rax=c0 f=Co keep=PAZS
@ ror al, 8
  c0c808
  SDM 2B ROR
ror8_by8_identity_updates_cf: rax=7f f=C => f=cpazs
@ ror rax, cl
  48d3c8
  SDM 2B ROR
ror64_cl64_affects_no_flags: rax=1 rcx=40 => f=*
ror64_cl65_is_one: rax=1 rcx=41 => rax=8000000000000000 f=CO keep=PAZS
@ rcl al, 1
  d0d0
  SDM 2B RCL
rcl8_by1_clear_carry_in: rax=80 f= => rax=0 f=CpazsO
rcl8_by1_set_carry_in_preserves_szap: rax=80 f=CPAZS => rax=1 f=CPAZSO
@ rcl al, 8
  c0d008
  SDM 2B RCL
rcl8_by8: rax=1 f= => rax=0 f=Cpazs
@ rcl al, 9
  c0d009
  SDM 2B RCL
rcl8_by9_is_identity: rax=a5 f=C => f=Cpazs
@ rcl ax, 17
  66c1d011
  SDM 2B RCL
rcl16_by17_is_identity: rax=112233445566a5a5 f= => f=cpazs
@ rcl ax, 16
  66c1d010
  SDM 2B RCL
rcl16_by16: rax=1122334455660001 f=C => rax=1122334455668000 f=Cpazs
@ rcl eax, 31
  c1d01f
  SDM 2B RCL
rcl32_by31: rax=ffffffff00000003 f=C => rax=c0000000 f=Cpazs
@ rcl rax, 63
  48c1d03f
  SDM 2B RCL
rcl64_by63: rax=3 f= => rax=8000000000000000 f=Cpazs
@ rcl eax, cl
  d3d0
  SDM 2B RCL; 1 3.4.1.1
rcl32_cl32_affects_nothing_but_extension: rax=ffffffff80000001 rcx=20 => rax=80000001 f=*
@ rcr al, 1
  d0d8
  SDM 2B RCR
rcr8_by1_clear_carry_in: rax=1 f= => rax=0 f=Cpazso
rcr8_by1_set_carry_in: rax=1 f=C => rax=80 f=CpazsO
@ rcr al, 10
  c0d80a
  SDM 2B RCR
rcr8_by10_is_by_one_mod_9: rax=2 f=C => rax=81 f=cpazs
@ rcr rax, 1
  48d1d8
  SDM 2B RCR
rcr64_by1: rax=8000000000000001 f= => rax=4000000000000000 f=CpazsO
@ shr rdx, 1; rcr rax, 1
  48d1ea 48d1d8
  SDM 2B SHR; 2B RCR
shr_rcr_128bit_right_shift: rdx=3 rax=0 => rdx=1 rax=8000000000000000
@ shld eax, ebx, 4
  0fa4d804
  SDM 2B SHLD; 1 3.4.1.1
shld32_by4: rax=ffffffff12345678 rbx=9abcdef0 => rax=23456789 f=Cpzs
@ shld eax, ebx, 1
  0fa4d801
  SDM 2B SHLD
shld32_by1_sign_change: rax=40000000 rbx=80000000 => rax=80000001 f=cpzSO
@ shld eax, ebx, cl
  0fa5d8
  SDM 2B SHLD
shld32_cl32_is_count_zero: rax=12345678 rbx=ffffffff rcx=20 => f=*
@ shld rax, rbx, 63
  480fa4d83f
  SDM 2B SHLD
shld64_by63: rax=1 rbx=fffffffffffffffe => rax=ffffffffffffffff f=cPzS
@ shld ax, bx, 16
  660fa4d810
  SDM 2B SHLD (count equal to size)
shld16_by16_takes_source: rax=1122334455660001 rbx=8002 => rax=1122334455668002 f=CpzS
@ shrd eax, ebx, 8
  0facd808
  SDM 2B SHRD
shrd32_by8: rax=123456f0 rbx=9abcde77 => rax=77123456 f=CPzs
@ shrd rax, rbx, 1
  480facd801
  SDM 2B SHRD
shrd64_by1_sign_change: rax=0 rbx=1 => rax=8000000000000000 f=cPzSO
@ shrd rax, rbx, cl
  480fadd8
  SDM 2B SHRD
shrd64_cl64_is_count_zero: rax=5 rbx=1 rcx=40 => f=*
";

const MULTIPLY_DIVIDE: &str = r"
@ mul bl
  f6e3
  SDM 2B MUL
mul8_max: rax=11223344556600ff rbx=ff => rax=112233445566fe01 f=CO
mul8_fits_clears_co: rax=112233445566ff0f rbx=11 => rax=11223344556600ff f=co
@ mul bx
  66f7e3
  SDM 2B MUL; 1 3.4.1.1
mul16_preserves_uppers: rax=112233445566ffff rdx=8877665544332211 rbx=ffff => rax=1122334455660001 rdx=887766554433fffe f=CO
@ mul ebx
  f7e3
  SDM 2B MUL; 1 3.4.1.1
mul32_zero_extends_both: rax=ffffffffffffffff rdx=ffffffffffffffff rbx=ffffffff => rax=1 rdx=fffffffe f=CO
@ mul rbx
  48f7e3
  SDM 2B MUL
mul64_max: rax=ffffffffffffffff rbx=ffffffffffffffff => rax=1 rdx=fffffffffffffffe f=CO
@ mul qword ptr [rsi]
  48f726
  SDM 2B MUL
mul64_memory_high_zero: rsi=D rax=4000000000000000 m0=0300000000000000 => rax=c000000000000000 rdx=0 f=co
@ imul bl
  f6eb
  SDM 2A IMUL
imul8_min_squared: rax=80 rbx=80 => rax=4000 f=CO
imul8_minus_one_squared: rax=aaff rbx=ff => rax=1 f=co
imul8_minus128: rax=80 rbx=1 => rax=ff80 f=co
@ imul rbx
  48f7eb
  SDM 2A IMUL
imul64_one_operand_negative: rax=ffffffffffffffff rbx=1 => rax=ffffffffffffffff rdx=ffffffffffffffff f=co
imul64_one_operand_min_times_minus_one: rax=8000000000000000 rbx=ffffffffffffffff => rax=8000000000000000 rdx=0 f=CO
@ imul rax, rbx
  480fafc3
  SDM 2A IMUL
imul64_two_operand_truncates: rax=4000000000000000 rbx=2 => rax=8000000000000000 f=CO
imul64_two_operand_min_fits: rax=4000000000000000 rbx=fffffffffffffffe => rax=8000000000000000 f=co
@ imul eax, ebx, -3
  6bc3fd
  SDM 2A IMUL; 1 3.4.1.1
imul32_three_operand_imm8: rax=ffffffffffffffff rbx=5 => rax=fffffff1 f=co
@ imul ax, bx, 0x7fff
  6669c3ff7f
  SDM 2A IMUL; 1 3.4.1.1
imul16_three_operand_truncates: rax=1122334455667788 rbx=2 => rax=112233445566fffe f=CO
@ div bl
  f6f3
  SDM 2A DIV
div8: rax=1122334455660105 rbx=10 => rax=1122334455660510
@ div bx
  66f7f3
  SDM 2A DIV
div16: rdx=1122334455660001 rax=1122334455660001 rbx=2 => rax=1122334455668000 rdx=1122334455660001
@ div ebx
  f7f3
  SDM 2A DIV; 1 3.4.1.1
div32_zero_extends: rdx=ffffffff00000001 rax=ffffffff00000003 rbx=4 => rax=40000000 rdx=3
@ div rbx
  48f7f3
  SDM 2A DIV
div64_wide_dividend: rdx=1 rax=3 rbx=2 => rax=8000000000000001 rdx=1
div64_quotient_max: rdx=fffffffffffffffe rax=ffffffffffffffff rbx=ffffffffffffffff => rax=ffffffffffffffff rdx=fffffffffffffffe
@ div rbx
  !48f7f3
  SDM 2A DIV; 3A 7.15 Interrupt 0
div64_quotient_overflow: rdx=2 rax=0 rbx=2 => ! DE
@ mov r8d, 7; div rbx; mov r9d, 8
  41b807000000 !48f7f3 41b908000000
  SDM 2A DIV; 3A 7.15 Interrupt 0
div64_by_zero: rdx=0 rax=1 rbx=0 => r8=7 ! DE
@ div bl
  !f6f3
  SDM 2A DIV; 3A 7.15 Interrupt 0
div8_quotient_overflow: rax=1000 rbx=10 => ! DE
@ div dword ptr [rsi]
  !f736
  SDM 2A DIV; 3A 7.15 Interrupt 0
div32_by_zero_memory: rdx=0 rax=1 rsi=D m0=00000000ffffffff => ! DE
@ idiv bl
  f6fb
  SDM 2A IDIV Table 3-53
idiv8_minus128_fits: rax=0080 rbx=ff => rax=80
@ idiv bl
  !f6fb
  SDM 2A IDIV Table 3-53
idiv8_plus128_overflows: rax=ff80 rbx=ff => ! DE
@ idiv ebx
  f7fb
  SDM 2A IDIV; 2B SAR (-9 / 4 example)
idiv32_truncates_toward_zero: rdx=ffffffff rax=fffffff7 rbx=4 => rax=fffffffe rdx=ffffffff
@ idiv rbx
  48f7fb
  SDM 2A IDIV
idiv64_positive_by_negative: rdx=0 rax=7 rbx=fffffffffffffffe => rax=fffffffffffffffd rdx=1
@ idiv rbx
  !48f7fb
  SDM 2A IDIV; 3A 7.15 Interrupt 0
idiv64_min_by_minus_one_overflows: rdx=ffffffffffffffff rax=8000000000000000 rbx=ffffffffffffffff => ! DE
@ idiv bx
  66f7fb
  SDM 2A IDIV Table 3-53
idiv16_min_result_fits: rdx=ffff rax=8000 rbx=1 => rax=8000 rdx=0
@ idiv rbx
  48f7fb
  SDM 2A IDIV Table 3-53
idiv64_wide_min_fits: rdx=ffffffffffffffff rax=0 rbx=2 => rax=8000000000000000 rdx=0
";

const BIT_OPERATIONS: &str = r"
@ bt eax, ebx
  0fa3d8
  SDM 2A BT
bt32_register_offset_is_modulo: rax=80000000 rbx=3f f=Z => f=CZ
@ bt rax, rbx
  480fa3d8
  SDM 2A BT
bt64_register_offset_is_modulo: rax=1 rbx=40 f= => f=Cz
@ bt dword ptr [rsi], eax
  0fa306
  SDM 2A BT; 2A 3.1.1.9 Bit(BitBase,BitOffset)
bt_memory_positive_offset_beyond_operand: rsi=D rax=41 m0=0000000000000000020000 f= => f=Cz
@ bt qword ptr [rsi], rax
  480fa306
  SDM 2A BT; 2A 3.1.1.9
bt_memory_negative_offset: rsi=D+10 rax=ffffffffffffffff m0=000000000000000000000000000000800000000000000000 f= => f=Cz
@ bts qword ptr [rsi], rax
  480fab06
  SDM 2A BTS; 2A 3.1.1.9
bts_memory_negative_offset: rsi=D+10 rax=fffffffffffffff8 m0=000000000000000000000000000000000000000000000000 f=CZ => m0=000000000000000000000000000000010000000000000000 f=cZ
@ btr word ptr [rsi], ax
  660fb306
  SDM 2A BTR; 2A 3.1.1.9
btr16_memory_negative_16bit_offset: rsi=D+2 rax=7fffffffffffffff m0=ffffffff f= => m0=ff7fffff f=Cz
@ .byte 0x0f,0xba,0x26,0x21
  0fba2621
  SDM 2A BT
bt_imm8_memory_offset_is_masked: rsi=D m0=0200000000000000 f= => f=Cz
@ bts eax, 31
  0fbae81f
  SDM 2A BTS; 1 3.4.1.1
bts32_register_zero_extends: rax=ffffffff00000000 f= => rax=80000000 f=cz
@ btr rax, 63
  480fbaf03f
  SDM 2A BTR
btr64_register: rax=ffffffffffffffff f=Z => rax=7fffffffffffffff f=CZ
@ btc rax, rbx
  480fbbd8
  SDM 2A BTC
btc64_register: rax=0 rbx=7f f= => rax=8000000000000000 f=cz
@ lock btc dword ptr [rdi], 0
  f00fba3f00
  SDM 2A BTC
lock_btc_memory: rdi=D m0=ffffffff f= => m0=feffffff f=Cz
@ bsf rax, rbx
  480fbcc3
  SDM 2A BSF
bsf64: rax=0 rbx=8000000000000000 => rax=3f f=z
@ bsr rax, rbx
  480fbdc3
  SDM 2A BSR
bsr64: rax=0 rbx=8000000000000001 => rax=3f f=z
@ bsf eax, ebx
  0fbcc3
  SDM 2A BSF; 1 3.4.1.1
bsf32_zero_extends: rax=ffffffffffffffff rbx=ffffffff00000010 => rax=4 f=z
@ bsf rax, rbx
  480fbcc3
  SDM 2A BSF
bsf64_zero_source_keeps_destination: rax=1234567812345678 rbx=0 => f=Z
@ bsr rax, rbx
  480fbdc3
  SDM 2A BSR
bsr64_zero_source_keeps_destination: rax=1234567812345678 rbx=0 => f=Z
@ popcnt rax, rbx
  f3480fb8c3
  SDM 2B POPCNT
popcnt64: rax=0 rbx=ffffffffffffffff => rax=40 f=cpazso
@ popcnt eax, ebx
  f30fb8c3
  SDM 2B POPCNT; 1 3.4.1.1
popcnt32_zero: rax=ffffffffffffffff rbx=ffffffff00000000 => rax=0 f=cpaZso
@ tzcnt rax, rbx
  f3480fbcc3
  SDM 2B TZCNT
tzcnt64_zero: rax=0 rbx=0 => rax=40 f=Cz
@ tzcnt eax, ebx
  f30fbcc3
  SDM 2B TZCNT; 1 3.4.1.1
tzcnt32_lsb_set: rax=ffffffffffffffff rbx=1 => rax=0 f=cZ
@ lzcnt eax, ebx
  f30fbdc3
  SDM 2A LZCNT; 1 3.4.1.1
lzcnt32_zero: rax=ffffffffffffffff rbx=ffffffff00000000 => rax=20 f=Cz
@ lzcnt rax, rbx
  f3480fbdc3
  SDM 2A LZCNT
lzcnt64_msb_set: rax=1 rbx=8000000000000000 => rax=0 f=cZ
@ crc32 rax, qword ptr [rsi]; crc32 eax, byte ptr [rsi+8]; not eax
  f2480f38f106 f20f38f04608 f7d0
  SDM 2A CRC32 (polynomial 11EDC6F41H); check value of the digits 1 to 9
crc32c_check_string: rax=ffffffff rsi=D m0=313233343536373839 => rax=e3069283 f=*
@ crc32 eax, byte ptr [rsi]; crc32 eax, byte ptr [rsi+1]; crc32 eax, word ptr [rsi+2]; crc32 eax, dword ptr [rsi+4]; crc32 ebx, dword ptr [rsi+4]; crc32 rcx, qword ptr [rsi]
  f20f38f006 f20f38f04601 66f20f38f14602 f20f38f14604 f20f38f15e04 f2480f38f10e
  SDM 2A CRC32
crc32c_byte_and_qword_forms_agree: rax=0 rbx=0 rcx=0 rsi=D m0=0000000001000000 => f=* rax=dd45aab8 rbx=dd45aab8 rcx=dd45aab8
@ btc dword ptr [rdi], 0
  0fba3f00
  SDM 2A BTC
btc_memory_without_lock: rdi=D m0=ffffffff f= => m0=feffffff f=Cz
";

const CONDITIONS: &str = r"
@ cmp rax, rbx; seto [rdi]; setno [rdi+1]; setb [rdi+2]; setae [rdi+3]; sete [rdi+4]; setne [rdi+5]; setbe [rdi+6]; seta [rdi+7]; sets [rdi+8]; setns [rdi+9]; setp [rdi+10]; setnp [rdi+11]; setl [rdi+12]; setge [rdi+13]; setle [rdi+14]; setg [rdi+15]
  4839d8 0f9007 0f914701 0f924702 0f934703 0f944704 0f954705 0f964706 0f974707 0f984708 0f994709 0f9a470a 0f9b470b 0f9c470c 0f9d470d 0f9e470e 0f9f470f
  SDM 2B SETcc; 2A CMP
setcc_all_equal: rax=5 rbx=5 rdi=D m0=ffffffffffffffffffffffffffffffff => m0=00010001010001000001010000010100 f=cPaZso
setcc_all_unsigned_below_signed_greater: rax=7fffffffffffffff rbx=8000000000000000 rdi=D m0=ffffffffffffffffffffffffffffffff => m0=01000100000101000100010000010001 f=CPazSO
setcc_all_signed_less_unsigned_above: rax=8000000000000000 rbx=1 rdi=D m0=ffffffffffffffffffffffffffffffff => m0=01000001000100010001010001000100 f=cPAzsO
@ cmp eax, ebx; seto [rdi]; setno [rdi+1]; setb [rdi+2]; setae [rdi+3]; sete [rdi+4]; setne [rdi+5]; setbe [rdi+6]; seta [rdi+7]; sets [rdi+8]; setns [rdi+9]; setp [rdi+10]; setnp [rdi+11]; setl [rdi+12]; setge [rdi+13]; setle [rdi+14]; setg [rdi+15]
  39d8 0f9007 0f914701 0f924702 0f934703 0f944704 0f954705 0f964706 0f974707 0f984708 0f994709 0f9a470a 0f9b470b 0f9c470c 0f9d470d 0f9e470e 0f9f470f
  SDM 2B SETcc; 2A CMP
setcc_all_both_less: rax=1 rbx=2 rdi=D m0=ffffffffffffffffffffffffffffffff => m0=00010100000101000100010001000100 f=CPAzSo
@ cmp al, bl; seto [rdi]; setno [rdi+1]; setb [rdi+2]; setae [rdi+3]; sete [rdi+4]; setne [rdi+5]; setbe [rdi+6]; seta [rdi+7]; sets [rdi+8]; setns [rdi+9]; setp [rdi+10]; setnp [rdi+11]; setl [rdi+12]; setge [rdi+13]; setle [rdi+14]; setg [rdi+15]
  38d8 0f9007 0f914701 0f924702 0f934703 0f944704 0f954705 0f964706 0f974707 0f984708 0f994709 0f9a470a 0f9b470b 0f9c470c 0f9d470d 0f9e470e 0f9f470f
  SDM 2B SETcc; 2A CMP
setcc_all_both_greater_odd_parity: rax=3 rbx=2 rdi=D m0=ffffffffffffffffffffffffffffffff => m0=00010001000100010001000100010001 f=cpazso
@ cmp rax, rbx; mov ecx, 0; jo 1f; lea ecx, [rcx+1]; 1: jno 1f; lea ecx, [rcx+2]; 1: jb 1f; lea ecx, [rcx+4]; 1: jae 1f; lea ecx, [rcx+8]; 1: je 1f; lea ecx, [rcx+0x10]; 1: jne 1f; lea ecx, [rcx+0x20]; 1: jbe 1f; lea ecx, [rcx+0x40]; 1: ja 1f; lea ecx, [rcx+0x80]; 1: js 1f; lea ecx, [rcx+0x100]; 1: jns 1f; lea ecx, [rcx+0x200]; 1: jp 1f; lea ecx, [rcx+0x400]; 1: jnp 1f; lea ecx, [rcx+0x800]; 1: jl 1f; lea ecx, [rcx+0x1000]; 1: jge 1f; lea ecx, [rcx+0x2000]; 1: jle 1f; lea ecx, [rcx+0x4000]; 1: jg 1f; lea ecx, [rcx+0x8000]; 1: nop
  4839d8 b900000000 7003 8d4901 7103 8d4902 7203 8d4904 7303 8d4908 7403 8d4910 7503 8d4920 7603 8d4940 7706 8d8980000000 7806 8d8900010000 7906 8d8900020000 7a06 8d8900040000 7b06 8d8900080000 7c06 8d8900100000 7d06 8d8900200000 7e06 8d8900400000 7f06 8d8900800000 90
  SDM 2A Jcc; 2A LEA (no flags)
jcc_all_signed_less_unsigned_above: rax=8000000000000000 rbx=1 => rcx=a956 f=cPAzsO
@ cmp rax, rbx; mov ecx, 0; jo 1f; lea ecx, [rcx+1]; 1: jno 1f; lea ecx, [rcx+2]; 1: jb 1f; lea ecx, [rcx+4]; 1: jae 1f; lea ecx, [rcx+8]; 1: je 1f; lea ecx, [rcx+0x10]; 1: jne 1f; lea ecx, [rcx+0x20]; 1: jbe 1f; lea ecx, [rcx+0x40]; 1: ja 1f; lea ecx, [rcx+0x80]; 1: js 1f; lea ecx, [rcx+0x100]; 1: jns 1f; lea ecx, [rcx+0x200]; 1: jp 1f; lea ecx, [rcx+0x400]; 1: jnp 1f; lea ecx, [rcx+0x800]; 1: jl 1f; lea ecx, [rcx+0x1000]; 1: jge 1f; lea ecx, [rcx+0x2000]; 1: jle 1f; lea ecx, [rcx+0x4000]; 1: jg 1f; lea ecx, [rcx+0x8000]; 1: nop
  4839d8 b900000000 7003 8d4901 7103 8d4902 7203 8d4904 7303 8d4908 7403 8d4910 7503 8d4920 7603 8d4940 7706 8d8980000000 7806 8d8900010000 7906 8d8900020000 7a06 8d8900040000 7b06 8d8900080000 7c06 8d8900100000 7d06 8d8900200000 7e06 8d8900400000 7f06 8d8900800000 90
  SDM 2A Jcc
jcc_all_equal: rax=9 rbx=9 => rcx=99a5 f=cPaZso
@ mov ecx, 1; cmp eax, ebx; mov r8d, 0; mov r9d, 0; cmovb r8, rcx; cmova r9, rcx; mov r10d, 0; mov r11d, 0; cmovl r10, rcx; cmovg r11, rcx; mov r12d, 0; mov r13d, 0; cmovo r12, rcx; cmovs r13, rcx
  b901000000 39d8 41b800000000 41b900000000 4c0f42c1 4c0f47c9 41ba00000000 41bb00000000 4c0f4cd1 4c0f4fd9 41bc00000000 41bd00000000 4c0f40e1 4c0f48e9
  SDM 2A CMOVcc
cmov_all_unsigned_below_signed_greater: rax=1 rbx=80000000 => rcx=1 r8=1 r9=0 r10=0 r11=1 r12=1 r13=1 f=CpazSO
@ mov eax, 0; jrcxz 1f; mov eax, 1; 1: nop
  b800000000 e305 b801000000 90
  SDM 2A Jcc
jrcxz_tests_all_64_bits: rcx=100000000 => rax=1 f=*
jrcxz_sees_bit_63: rcx=8000000000000000 => rax=1 f=*
jrcxz_taken_at_zero_keeps_rcx: rcx=0 => rax=0 rcx=0 f=*
@ mov eax, 0; jecxz 1f; mov eax, 1; 1: nop
  b800000000 67e305 b801000000 90
  SDM 2A Jcc
jecxz_tests_low_32_bits: rcx=100000000 => rax=0 f=*
@ 1: lea rax, [rax+1]; loop 1b
  488d4001 e2fa
  SDM 2A LOOP
loop_decrements_rcx_without_flags: rax=0 rcx=3 => rax=3 rcx=0 f=*
@ 1: lea rax, [rax+1]; loopne 1b
  488d4001 e0fa
  SDM 2A LOOP
loopne_stops_on_zero_flag: rax=0 rcx=5 f=Z => rax=1 rcx=4 f=*
@ mov eax, 0; loop 1f; mov eax, 1; 1: nop
  b800000000 e205 b801000000 90
  SDM 2A LOOP
loop_from_zero_wraps_and_branches: rcx=0 => rax=0 rcx=ffffffffffffffff f=*
loop_at_one_falls_through: rcx=1 => rax=1 rcx=0 f=*
loop_counts_all_64_bits: rcx=100000000 => rax=0 rcx=ffffffff f=*
@ mov eax, 0; loope 1f; mov eax, 1; 1: nop
  b800000000 e105 b801000000 90
  SDM 2A LOOP
loope_taken_with_zf: rcx=2 f=Z => rax=0 rcx=1 f=*
loope_not_taken_without_zf: rcx=2 f=CPASO => rax=1 rcx=1 f=*
loope_count_expires_with_zf: rcx=1 f=Z => rax=1 rcx=0 f=*
@ mov eax, 0; loopne 1f; mov eax, 1; 1: nop
  b800000000 e005 b801000000 90
  SDM 2A LOOP
loopne_taken_without_zf: rcx=2 f=CPASO => rax=0 rcx=1 f=*
loopne_not_taken_with_zf: rcx=2 f=Z => rax=1 rcx=1 f=*
loopne_count_expires_without_zf: rcx=1 => rax=1 rcx=0 f=*
";

const STACK_AND_CALLS: &str = r"
@ push -2; pop rax
  6afe 58
  SDM 2B PUSH
push_imm8_sign_extends: rax=0 => rax=fffffffffffffffe f=*
@ push rsp; pop rax
  54 58
  SDM 2B PUSH
push_rsp_pushes_old_value: rax=0 => rax=S f=*
@ push rax; pop rsp
  50 5c
  SDM 2B POP
pop_rsp_takes_popped_value: rax=S-100 => rsp=S-100 f=*
@ push rax; push 0; pop qword ptr [rsp]; pop rbx
  50 6a00 8f0424 5b
  SDM 2B POP
pop_memory_address_uses_incremented_rsp: rax=77 rbx=1 => rbx=0 f=*
@ push rax; push qword ptr [rsp]; pop rbx; pop rcx
  50 ff3424 5b 59
  SDM 2B PUSH
push_memory_address_uses_old_rsp: rax=77 rbx=1 rcx=1 => rbx=77 rcx=77 f=*
@ push ax; lea rbx, [rsp]; pop cx
  6650 488d1c24 6659
  SDM 2B PUSH
push16_moves_rsp_by_two: rax=1234 rcx=1122334455667788 rbx=0 => rbx=S-2 rcx=1122334455661234 f=*
@ call 1f; jmp 2f; 1: mov rax, [rsp]; ret; 2: nop
  e802000000 eb05 488b0424 c3 90
  SDM 2A CALL; 2B RET
call_ret_round_trip: rax=0 => rax=C+5 f=*
@ lea rax, [rip+1f]; call rax; jmp 2f; 1: mov ebx, 5; ret; 2: nop
  488d0504000000 ffd0 eb06 bb05000000 c3 90
  SDM 2A CALL
call_indirect_register: rbx=0 => rax=C+0b rbx=5 f=*
@ push 1; push 2; call 1f; jmp 2f; 1: ret 16; 2: nop
  6a01 6a02 e802000000 eb03 c21000 90
  SDM 2B RET
ret_imm16_releases_arguments: => f=*
@ enter 0x20, 0; lea rax, [rsp]; mov rcx, rbp; leave
  c8200000 488d0424 4889e9 c9
  SDM 2A ENTER; 2A LEAVE
enter_leave_round_trip: rbp=1234 => rax=S-28 rcx=S-8 f=*
";

const STRINGS: &str = r"
@ cld; movsb
  fc a4
  SDM 2B MOVS
movsb_forward: rsi=D rdi=D+8 m0=aa m8=00 => rsi=D+1 rdi=D+9 m8=aa f=*
@ cld; rep movsb
  fc f3a4
  SDM 2B MOVS; 2B REP
rep_movsq_overlap_propagates: rsi=D rdi=D+1 rcx=4 m0=1122334455 => rsi=D+4 rdi=D+5 rcx=0 m0=1111111111 f=*
@ cld; rep stosd
  fc f3ab
  SDM 2B STOS; 2B REP
rep_stosd_forward: rdi=D rax=ffffffff11223344 rcx=2 m0=000000000000000000 => rdi=D+8 rcx=0 m0=443322114433221100 f=*
@ std; rep movsw; cld
  fd f366a5 fc
  SDM 2B MOVS; 1 3.4.3.2
rep_movsw_backward: rsi=D+2 rdi=D+a rcx=2 m0=11223344 m8=0000000000 => rsi=D-2 rdi=D+6 rcx=0 m8=1122334400 f=*
@ cld; rep movsq
  fc f348a5
  SDM 2B REP
rep_movs_zero_count_touches_nothing: rsi=U rdi=U rcx=0 => f=*
@ cld; lodsw
  fc 66ad
  SDM 2A LODS; 1 3.4.1.1
lodsw_preserves_upper48: rsi=D rax=1122334455667788 m0=feca => rsi=D+2 rax=112233445566cafe f=*
@ cld; lodsd
  fc ad
  SDM 2A LODS; 1 3.4.1.1
lodsd_zero_extends: rsi=D rax=1122334455667788 m0=efbeadde => rsi=D+4 rax=deadbeef f=*
@ cld; repe cmpsb
  fc f3a6
  SDM 2A CMPS; 2B REPE
repe_cmpsb_stops_at_difference: rsi=D rdi=D+8 rcx=8 m0=0102030501020304 m8=0102030401020304 => rsi=D+4 rdi=D+c rcx=4 f=cpazso
repe_cmpsb_zero_count_keeps_flags: rsi=U rdi=U rcx=0 => f=*
@ cld; cmpsb
  fc a6
  SDM 2A CMPS
cmpsb_is_rsi_minus_rdi: rsi=D rdi=D+8 m0=00 m8=01 => rsi=D+1 rdi=D+9 f=CPAzSo
@ cld; repne scasb
  fc f2ae
  SDM 2B SCAS; 2B REPNE
repne_scasb_finds_byte: rdi=D rax=33 rcx=8 m0=1122334455667788 => rdi=D+3 rcx=5 f=cPaZso
@ cld; repne scasd
  fc f2af
  SDM 2B SCAS; 2B REPNE
repne_scasd_exhausts_count: rdi=D rax=2 rcx=2 m0=0100000003000000 => rdi=D+8 rcx=0 f=CPAzSo
@ cld; addr32 rep movsb
  fc 67f3a4
  SDM 2B REP; 2B MOVS
rep_movsb_addr32_uses_32bit_registers: rsi=ffffffff00000000+D rdi=ffffffff00000000+D+8 rcx=ffffffff00000002 m0=aabb m8=0000 => rsi=D+2 rdi=D+a rcx=0 m8=aabb f=*
@ cld; rep movsb byte ptr es:[rdi], byte ptr fs:[rsi]
  fc f364a4
  SDM 2B MOVS; 3A 3.4.4
fs_rep_movsb_overrides_source_only: rsi=8 rdi=D rcx=2 fs8=a1a2 m0=0000 => rsi=a rdi=D+2 rcx=0 m0=a1a2 f=*
@ rep movsb
  f3a4
  SDM 2B MOVS; 2B REP
rep_movsb_overlap_propagates_df_clear: rsi=D rdi=D+1 rcx=4 m0=1122334455 => rsi=D+4 rdi=D+5 rcx=0 m0=1111111111 f=*
@ rep stosd
  f3ab
  SDM 2B STOS; 2B REP
rep_stosd_df_clear: rdi=D rax=ffffffff11223344 rcx=2 m0=000000000000000000 => rdi=D+8 rcx=0 m0=443322114433221100 f=*
@ rep movsq
  f348a5
  SDM 2B REP
rep_movsq_zero_count_df_clear: rsi=U rdi=U rcx=0 => f=*
@ lodsd
  ad
  SDM 2A LODS; 1 3.4.1.1
lodsd_zero_extends_df_clear: rsi=D rax=1122334455667788 m0=efbeadde => rsi=D+4 rax=deadbeef f=*
@ repe cmpsb
  f3a6
  SDM 2A CMPS; 2B REPE
repe_cmpsb_stops_at_difference_df_clear: rsi=D rdi=D+8 rcx=8 m0=0102030501020304 m8=0102030401020304 => rsi=D+4 rdi=D+c rcx=4 f=cpazso
@ repne scasb
  f2ae
  SDM 2B SCAS; 2B REPNE
repne_scasb_finds_byte_df_clear: rdi=D rax=33 rcx=8 m0=1122334455667788 => rdi=D+3 rcx=5 f=cPaZso
@ rep movsb
  !f3a4
  SDM 2B REP; 3A 7.5
rep_movsb_fault_df_clear: rsi=D rdi=U-4 rcx=10 m0=0102030405060708 => rsi=D+4 rdi=U rcx=c m1ffc=01020304 f=* ! PFW U
@ rep stosq
  !f348ab
  SDM 2B REP; 3A 7.5
rep_stosq_fault_df_clear: rdi=U-10 rcx=5 rax=1122334455667788 => rdi=U rcx=3 m1ff0=88776655443322118877665544332211 f=* ! PFW U
@ repe cmpsb
  !f3a6
  SDM 2B REPE (EFLAGS restored on a fault)
repe_cmpsb_fault_restores_flags_df_clear: rsi=D rdi=U-2 rcx=8 m0=0102 m1ffe=0102 => rsi=D+2 rdi=U rcx=6 f=* ! PFR U
";

const FS_GS: &str = r"
@ mov rax, fs:[0x28]
  64488b042528000000
  SDM 3A 3.4.4; 1 3.4.2.1
fs_load_absolute: rax=0 fs28=0102030405060708 => rax=0807060504030201 f=*
@ mov eax, gs:[rbx+rcx*4+8]
  658b448b08
  SDM 3A 3.4.4
gs_load_base_index_disp: rax=ffffffffffffffff rbx=10 rcx=2 gs20=78563412 => rax=12345678 f=*
@ mov byte ptr gs:[rbx], 0x5a
  65c6035a
  SDM 3A 3.4.4
gs_store_negative_effective_address_wraps: rbx=fffffffffffffff0 gsbase=GSB+20 gs10=00 => gs10=5a f=*
@ mov rax, fs:[ebx]
  6467488b03
  SDM 3A 3.4.4; 1 3.3.7
fs_addr32_truncates_before_adding_base: rax=0 rbx=ffffffff00000010 fs10=1122334455667788 => rax=8877665544332211 f=*
@ lea rax, fs:[rbx+4]
  64488d4304
  SDM 2A LEA
lea_ignores_fs_base: rax=0 rbx=10 => rax=14 f=*
@ add dword ptr fs:[rbx], 1
  64830301
  SDM 3A 3.4.4; 2A ADD
fs_rmw_add: rbx=4 fs4=ffffffff => fs4=00000000 f=CPAZso
@ lock xadd gs:[rbx], ecx
  65f00fc10b
  SDM 3A 3.4.4; 2B XADD
gs_lock_xadd: rbx=8 rcx=5 gs8=03000000 => rcx=3 gs8=08000000 f=cpazso
@ movups xmm3, gs:[rbx+1]
  650f105b01
  SDM 3A 3.4.4
gs_movups_load: rbx=0 gs1=00112233445566778899aabbccddeeff => x3=b:00,11,22,33,44,55,66,77,88,99,aa,bb,cc,dd,ee,ff f=*
@ mov rax, fs:[rsp]
  64488b0424
  SDM 3A 3.4.4
fs_override_on_rsp_base: rsp=18 rax=0 fs18=efcdab8967452301 => rax=0123456789abcdef f=*
@ mov eax, ds:[rsi]
  3e8b06
  SDM 3A 3.2.4
ds_override_adds_no_base: rsi=D rax=0 m0=44332211 => rax=11223344 f=*
@ mov eax, cs:[rsi]
  2e8b06
  SDM 3A 3.2.4
cs_override_adds_no_base: rsi=D rax=0 m0=44332211 => rax=11223344 f=*
@ lea rax, [rip+1f]; mov fs:[0], rax; call fs:[0]; jmp 2f; 1: mov ebx, 5; ret; 2: nop
  488d0513000000 644889042500000000 64ff142500000000 eb06 bb05000000 c3 90
  SDM 2A CALL; 3A 3.4.4
fs_call_memory_target: rbx=0 fs0=0000000000000000 => rbx=5 rax=C+1a fs0=1a00400000000000 f=*
@ mov rax, fs:[rbx]
  !64488b03
  SDM 3A 3.4.4; 3A 7.15 Interrupt 14
fs_fault_reports_linear_address: rbx=U-FSB => ! PFR U
@ xadd gs:[rbx], ecx
  650fc10b
  SDM 3A 3.4.4; 2B XADD
gs_xadd_without_lock: rbx=8 rcx=5 gs8=03000000 => rcx=3 gs8=08000000 f=cpazso
@ rep movsb byte ptr es:[rdi], byte ptr fs:[rsi]
  f364a4
  SDM 2B MOVS; 3A 3.4.4
fs_rep_movsb_overrides_source_only_df_clear: rsi=8 rdi=D rcx=2 fs8=a1a2 m0=0000 => rsi=a rdi=D+2 rcx=0 m0=a1a2 f=*
";

/// MOV r/m, imm (opcodes C6 /0 and C7 /0), issue #194.
///
/// Memory-form encodings came from Apple clang's assembler. The register forms
/// of C6/C7 are the long alternatives to B0+r and B8+r, which that assembler
/// never emits, so they were written by hand from the opcode map (ModRM
/// `11 000 rm`). Expected values follow SDM 2B MOV, where the imm32 of the
/// 64-bit form is sign-extended, and were computed by hand before the first run,
/// without reading how the interpreter or the DBT execute C6 and C7. The author is
/// an AI model, as for the rest of this file. Both engines already agreed with
/// every case, so this group closes a coverage gap and fixes no defect.
const IMMEDIATE_STORES: &str = r"
@ mov byte ptr [rdi], 0x80
  c60780
  SDM 2B MOV (C6 /0)
store_byte_base: rdi=D m0=aaaaaaaa => m0=80aaaaaa f=*
@ mov word ptr [rdi], 0x8001
  66c7070180
  SDM 2B MOV (C7 /0, 66 prefix)
store_word_base: rdi=D m0=aaaaaaaa => m0=0180aaaa f=*
@ mov dword ptr [rdi], 0x80000001
  c70701000080
  SDM 2B MOV (C7 /0)
store_dword_base_is_not_sign_extended: rdi=D m0=aaaaaaaaaaaaaaaa => m0=01000080aaaaaaaa f=*
@ mov qword ptr [rdi], -0x7fffffff
  48c70701000080
  SDM 2B MOV (REX.W C7 /0, imm32 sign-extended to 64 bits)
store_qword_negative_immediate_sign_extends: rdi=D m0=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa => m0=01000080ffffffffaaaaaaaaaaaaaaaa f=*
@ mov qword ptr [rdi], 1
  48c70701000000
  SDM 2B MOV (REX.W C7 /0)
store_qword_positive_immediate_clears_upper_half: rdi=D m0=ffffffffffffffffaaaaaaaaaaaaaaaa => m0=0100000000000000aaaaaaaaaaaaaaaa f=*
@ mov qword ptr [rdi+rcx*8+0x10], -2
  48c744cf10feffffff
  SDM 2B MOV; 2A 2.1.5 (SIB, disp8)
store_qword_sib_scale_8_disp8: rdi=D rcx=2 m20=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa => m20=feffffffffffffffaaaaaaaaaaaaaaaa f=*
@ mov dword ptr [rdi+0x7f], 0x11223344
  c7477f44332211
  SDM 2B MOV; 2A 2.1.5 (disp8, largest positive)
store_dword_disp8_positive_limit: rdi=D m7e=aaaaaaaaaaaaaaaaaaaa => m7e=aa44332211aaaaaaaaaa f=*
@ mov dword ptr [rdi-0x80], 0x11223344
  c7478044332211
  SDM 2B MOV; 2A 2.1.5 (disp8 is sign-extended)
store_dword_disp8_negative_limit: rdi=D+80 m0=aaaaaaaaaaaaaaaa => m0=44332211aaaaaaaa f=*
@ mov dword ptr [rdi+0x100], 0x11223344
  c7870001000044332211
  SDM 2B MOV; 2A 2.1.5 (disp32)
store_dword_disp32: rdi=D m100=aaaaaaaaaaaaaaaa => m100=44332211aaaaaaaa f=*
@ mov byte ptr [r9+r10*2+1], 7
  43c644510107
  SDM 2B MOV; 2A 2.1.5 (REX.X and REX.B)
store_byte_rex_base_and_index: r9=D+10 r10=8 m20=aaaaaaaa => m20=aa07aaaa f=*
@ mov qword ptr [r12], 5
  49c7042405000000
  SDM 2B MOV; 2A 2.1.5 (R12 needs a SIB byte)
store_qword_r12_base: r12=D+30 m30=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa => m30=0500000000000000aaaaaaaaaaaaaaaa f=*
@ mov qword ptr [r13], 5
  49c7450005000000
  SDM 2B MOV; 2A 2.1.5 (R13 with mod 00 would be RIP-relative, so disp8 0)
store_qword_r13_base: r13=D+40 m40=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa => m40=0500000000000000aaaaaaaaaaaaaaaa f=*
@ mov word ptr [rsp+8], 0x1234
  66c74424083412
  SDM 2B MOV; 2A 2.1.5 (RSP needs a SIB byte)
store_word_rsp_base: rsp=D+58 m60=aaaaaaaa => m60=3412aaaa f=*
@ mov byte ptr [rbp+2], 0x7f
  c645027f
  SDM 2B MOV
store_byte_rbp_disp8: rbp=D+70 m72=aaaa => m72=7faa f=*
@ mov byte ptr [rdi+rcx], -1
  c6040fff
  SDM 2B MOV
store_byte_base_plus_index: rdi=D+80 rcx=5 m84=aaaaaaaa => m84=aaffaaaa f=*
@ mov word ptr [r8+r9*4+0x20], 0x7ffe
  6643c7448820fe7f
  SDM 2B MOV; 2A 2.1.5
store_word_r8_r9_scaled: r8=D r9=10 m60=aaaaaaaa => m60=fe7faaaa f=*
@ mov eax, 0x80000001
  c7c001000080
  SDM 2B MOV (C7 /0, register form); 1 3.4.1.1
register_dword_zero_extends: rax=ffffffffffffffff => rax=0000000080000001 f=*
@ mov ax, 0x8001
  66c7c00180
  SDM 2B MOV; 1 3.4.1.1
register_word_keeps_upper_bits: rax=1122334455667788 => rax=1122334455668001 f=*
@ mov rax, -2
  48c7c0feffffff
  SDM 2B MOV (REX.W C7 /0, register form)
register_qword_sign_extends: rax=0 => rax=fffffffffffffffe f=*
@ mov r10, 0x7fffffff
  49c7c2ffffff7f
  SDM 2B MOV
register_qword_rex_b_positive: r10=ffffffffffffffff => r10=000000007fffffff f=*
@ mov r8d, -1
  41c7c0ffffffff
  SDM 2B MOV; 1 3.4.1.1
register_dword_rex_b_zero_extends: r8=1122334455667788 => r8=00000000ffffffff f=*
@ mov al, 0x80
  c6c080
  SDM 2B MOV (C6 /0, register form)
register_byte_al: rax=1122334455667788 => rax=1122334455667780 f=*
@ mov ah, 5
  c6c405
  SDM 2B MOV; 1 3.4.1.1 (no REX: ModRM 4 is AH)
register_byte_ah: rax=1122334455667788 => rax=1122334455660588 f=*
@ mov sil, 3
  40c6c603
  SDM 2B MOV; 1 3.4.1.1 (a REX prefix makes ModRM 6 SIL)
register_byte_sil: rsi=1122334455667788 => rsi=1122334455667703 f=*
@ mov r9b, 4
  41c6c104
  SDM 2B MOV
register_byte_r9b: r9=1122334455667788 => r9=1122334455667704 f=*
@ mov byte ptr [rdi], 0x80
  !c60780
  SDM 3A 7.15 Interrupt 14
store_byte_unmapped: rdi=U => ! PFW U
store_byte_readonly: rdi=R+8 => ! PFW R+8
@ mov word ptr [rdi], 0x8001
  !66c7070180
  SDM 3A 7.15 Interrupt 14
store_word_unmapped: rdi=U => ! PFW U
store_word_readonly: rdi=R+8 => ! PFW R+8
@ mov dword ptr [rdi], 0x80000001
  !c70701000080
  SDM 3A 7.15 Interrupt 14
store_dword_unmapped: rdi=U => ! PFW U
store_dword_readonly: rdi=R+8 => ! PFW R+8
@ mov qword ptr [rdi], -0x7fffffff
  !48c70701000080
  SDM 3A 7.15 Interrupt 14
store_qword_unmapped: rdi=U => ! PFW U
store_qword_readonly: rdi=R+8 => ! PFW R+8
@ C6 with ModRM reg field 1 (no such instruction)
  !c60f01
  SDM 2B MOV (C6 is defined only as /0); 3A 7.15 Interrupt 6
c6_reg_field_1_is_ud: rdi=D m0=aaaaaaaa => ! UD
@ C7 with ModRM reg field 1 (no such instruction)
  !c70f01000000
  SDM 2B MOV (C7 is defined only as /0); 3A 7.15 Interrupt 6
c7_reg_field_1_is_ud: rdi=D m0=aaaaaaaa => ! UD
";

/// MOV between the accumulator and a 64-bit absolute address, `A0`-`A3`
/// (#428). The address follows the opcode in place of ModRM, eight bytes with
/// the default address size, and FS or GS may override the segment.
const MOV_MOFFS: &str = r"
@ movabs rax, [0x500000]
  48a10000500000000000
  SDM 2B MOV; 2A 2.2.1.4
mov_moffs64_load: rax=0 m0=0102030405060708 => rax=0807060504030201 f=*
@ movabs [0x500003], al
  a20300500000000000
  SDM 2B MOV; 2A 2.2.1.4
mov_moffs8_store: rax=77 m0=00000000 => m0=00000077 f=*
@ movabs al, byte ptr [0x500001]
  a00100500000000000
  SDM 2B MOV; 1 3.4.1.1
moffs8_load_keeps_upper_bits: rax=1122334455667788 m0=00ab => rax=11223344556677ab f=*
@ movabs ax, word ptr [0x500000]
  66a10000500000000000
  SDM 2B MOV; 1 3.4.1.1
moffs16_load_keeps_upper_bits: rax=1122334455667788 m0=3412 => rax=1122334455661234 f=*
@ movabs eax, dword ptr [0x500000]
  a10000500000000000
  SDM 2B MOV; 1 3.4.1.1
moffs32_load_zero_extends: rax=ffffffffffffffff m0=78563412 => rax=12345678 f=*
@ movabs byte ptr [0x500003], al
  a20300500000000000
  SDM 2B MOV
moffs8_store_writes_one_byte: rax=ffffffffffffff5a m0=1111111111 => m0=1111115a11 f=*
@ movabs word ptr [0x500002], ax
  66a30200500000000000
  SDM 2B MOV
moffs16_store: rax=1122334455667788 m0=0000000000000000 => m0=0000887700000000 f=*
@ movabs dword ptr [0x500004], eax
  a30400500000000000
  SDM 2B MOV
moffs32_store: rax=1122334455667788 m0=0000000000000000 => m0=0000000088776655 f=*
@ movabs qword ptr [0x500001], rax
  48a30100500000000000
  SDM 2B MOV
moffs64_store_unaligned: rax=1122334455667788 m0=ff00000000000000ff => m0=ff8877665544332211 f=*
@ .byte 0x49,0xa1,0x00,0x00,0x50,0x00,0x00,0x00,0x00,0x00
  49a10000500000000000
  SDM 2A 2.2.1.2; 2B MOV
moffs_rex_b_selects_no_register: rax=0 r8=5 m0=0102030405060708 => rax=0807060504030201 f=*
@ movabs rax, qword ptr fs:[0x28]
  6448a12800000000000000
  SDM 3A 3.4.4; 2B MOV
fs_moffs64_load: rax=0 fs28=0102030405060708 => rax=0807060504030201 f=*
@ movabs eax, dword ptr gs:[0x10]
  65a11000000000000000
  SDM 3A 3.4.4; 2B MOV
gs_moffs32_load: rax=ffffffffffffffff gs10=78563412 => rax=12345678 f=*
@ movabs byte ptr gs:[0xfffffffffffffff0], al
  65a2f0ffffffffffffff
  SDM 3A 3.4.4; 2B MOV
gs_moffs8_store_wraps_below_the_base: rax=5a gsbase=GSB+20 gs10=00 => gs10=5a f=*
@ movabs eax, dword ptr [0x502000]
  !a10020500000000000
  SDM 3A 7.15 Interrupt 14
moffs_load_unmapped: rax=5 => ! PFR U
@ movabs byte ptr [0x520000], al
  !a20000520000000000
  SDM 3A 7.15 Interrupt 14
moffs_store_readonly_page: rax=5a => ! PFW R
@ movabs rax, qword ptr [0x501ffc]
  !48a1fc1f500000000000
  SDM 3A 5.7 (the address with no translation); 3A 7.15 Interrupt 14
moffs_load_crossing_into_unmapped_page: rax=5 => ! PFR U
@ movabs rax, qword ptr [0x800000000000]
  !48a10000000000800000
  SDM 1 3.3.7.1; 2B MOV
moffs_noncanonical_load_faults: rax=5 => ! GP
@ movabs qword ptr [0x800000000000], rax
  !48a30000000000800000
  SDM 1 3.3.7.1; 2B MOV
moffs_noncanonical_store_faults: rax=5 => ! GP
";

const DECODING: &str = r"
@ mov eax, [ebx]
  678b03
  SDM 1 3.3.7
addr32_load_truncates_effective_address: rbx=ffffffff00000000+D rax=0 m0=44332211 => rax=11223344 f=*
@ mov eax, [ebx+ecx]
  678b040b
  SDM 1 3.3.7
addr32_sum_wraps_at_32_bits: rbx=ffff0000 rcx=10000+D rax=0 m0=44332211 => rax=11223344 f=*
@ .byte 0x48,0x66,0x01,0xd8
  486601d8
  SDM 2A 2.2.1
rex_not_adjacent_to_opcode_is_ignored: rax=112233445566ffff rbx=1 => rax=1122334455660000 f=CPAZso
@ .byte 0x41,0x66,0x01,0xd8
  416601d8
  SDM 2A 2.2.1
ignored_rex_b_does_not_select_r8: rax=112233445566ffff rbx=1 r8=5 => rax=1122334455660000 f=CPAZso
@ .byte 0x48,0x64,0x8b,0x03
  48648b03
  SDM 2A 2.2.1; 1 3.4.1.1
ignored_rex_w_before_segment_prefix_loads_32_bits: rax=ffffffffffffffff rbx=8 fs8=1122334455667788 => rax=44332211 f=*
@ .byte 0x41,0x66,0x41,0x01,0xd8
  41664101d8
  SDM 2A 2.2.1
rex_after_ignored_rex_applies: rax=7 rbx=1 r8=112233445566ffff => r8=1122334455660000 f=CPAZso
@ .byte 0x66,0x48,0x01,0xd8
  664801d8
  SDM 2A 2.2.1.2
rexw_overrides_66: rax=112233445566ffff rbx=1 => rax=1122334455670000 f=cPAzso
@ .byte 0x67,0x64,0xf0,0x48,0x81,0x84,0x58,0x00,0x01,0x00,0x00,0x44,0x33,0x22,0x11
  6764f0488184580001000044332211
  SDM 2A 2.2.1 (15-byte limit)
fifteen_byte_instruction_executes: rax=ffffffff00000010 rbx=8 fs120=0000000000000000 => fs120=4433221100000000 f=cPazso
@ ud2
  !0f0b
  SDM 2B UD; 3A 7.15 Interrupt 6
ud2: => ! UD
@ .byte 0x06
  !06
  SDM 2B PUSH (PUSH ES invalid in 64-bit mode)
invalid_opcode_06_in_64bit_mode: => ! UD
@ .byte 0xf0,0x01,0xd8
  !f001d8
  SDM 2A LOCK
lock_on_register_destination_is_ud: => ! UD
@ .byte 0xf0,0x89,0x07
  !f08907
  SDM 2A LOCK
lock_on_mov_is_ud: rdi=D => ! UD
@ .byte 0xf0,0x39,0x07
  !f03907
  SDM 2A LOCK
lock_on_cmp_is_ud: rdi=D => ! UD
@ .byte 0x48,0x8d,0xc3
  !488dc3
  SDM 2A LEA
lea_register_source_is_ud: => ! UD
@ add qword ptr fs:[rax+rbx*2+0x100], 0x11223344
  64488184580001000044332211
  SDM 3A 3.4.4; 2A 2.2.1
thirteen_byte_fs_add_imm32: rax=10 rbx=8 fs120=0000000000000000 => fs120=4433221100000000 f=cPazso
";

/// Single-thread baseline for the fences (#199). Each engine completes every
/// load and store of an instruction before the next instruction starts, so the
/// cases only require that a fence changes nothing and that accesses on either
/// side of it happen in program order. Nothing here involves a second thread.
const FENCES: &str = r"
@ mov [rdi], rax; mfence; mov rbx, [rdi]
  488907 0faef0 488b1f
  SDM 2B MFENCE
mfence_store_is_visible_to_a_later_load: rdi=D rax=1122334455667788 m0=0000000000000000 => rbx=1122334455667788 m0=8877665544332211 f=*
@ mov [rdi], rax; sfence; mov rbx, [rdi+8]
  488907 0faef8 488b5f08
  SDM 2B SFENCE
sfence_after_a_store_keeps_other_memory: rdi=D rax=1 m0=0000000000000000 m8=aabbccddeeff0011 => rbx=1100ffeeddccbbaa m0=0100000000000000 f=*
@ mov rbx, [rdi]; lfence; mov [rdi], rax
  488b1f 0faee8 488907
  SDM 2B LFENCE
lfence_load_precedes_a_later_store: rdi=D rax=5 m0=0200000000000000 => rbx=2 m0=0500000000000000 f=*
@ mfence; lfence; sfence
  0faef0 0faee8 0faef8
  SDM 2B MFENCE; 2B LFENCE; 2B SFENCE
fences_change_no_register_or_flag: rax=1122334455667788 rbx=99aabbccddeeff00 => f=*
@ .byte 0x0f,0xae,0xf5; .byte 0x0f,0xae,0xeb; .byte 0x0f,0xae,0xff
  0faef5 0faeeb 0faeff
  SDM 2B MFENCE; 2B LFENCE; 2B SFENCE (the r/m field is ignored)
fences_ignore_the_rm_field: rax=1122334455667788 => f=*
";

/// FNINIT, the only x87 instruction (#378). The three words it sets are all the
/// x87 state there is: no data register, pointer or last opcode is modelled, and
/// nothing can read the words back yet, so the cases look at guest state.
const X87_INIT: &str = r"
@ fninit
  dbe3
  SDM 2A FINIT/FNINIT (Operation); 1 8.1.5 (control word 037FH); 1 8.1.7 (tag FFFFH)
fninit_sets_control_status_and_tag: fcw=0040 fsw=3a41 ftw=0000 => fcw=037f fsw=0 ftw=ffff f=*
fninit_from_every_bit_set: fcw=ffff fsw=ffff ftw=0000 => fcw=037f fsw=0 ftw=ffff f=*
fninit_from_every_bit_clear: fcw=0 fsw=0 ftw=0 => fcw=037f fsw=0 ftw=ffff f=*
fninit_twice_is_the_same: fcw=037f fsw=0 ftw=ffff => fcw=037f fsw=0 ftw=ffff f=*
fninit_from_the_harness_default: => fcw=037f fsw=0 ftw=ffff f=*
@ fninit
  dbe3
  SDM 2A FINIT/FNINIT (the x87 words only; Flags Affected: C0 to C3 cleared, which are status-word bits)
fninit_keeps_registers_flags_xmm_and_mxcsr: rax=1122334455667788 rbx=99aabbccddeeff00 x3=q:0123456789abcdef,fedcba9876543210 mx=5fbf => fcw=037f fsw=0 ftw=ffff f=*
@ mov [rdi], rax; fninit; mov rbx, [rdi]
  488907 dbe3 488b1f
  SDM 2A FINIT/FNINIT
fninit_touches_no_memory: rdi=D rax=1122334455667788 m0=0000000000000000 => rbx=1122334455667788 m0=8877665544332211 fcw=037f fsw=0 ftw=ffff f=*
@ fninit; add al, bl
  dbe3 00d8
  SDM 2A FINIT/FNINIT; 2A ADD
fninit_then_integer_code_runs_on: rax=7f rbx=1 => rax=80 fcw=037f fsw=0 ftw=ffff f=cpAzSO
@ add al, bl
  00d8
  SDM 2A ADD (no x87 state is named)
integer_code_keeps_the_x87_words: rax=7f rbx=1 fcw=1a2b fsw=3c4d ftw=5e6f => rax=80 f=cpAzSO
";

const MEMORY_FAULTS: &str = r"
@ mov r8d, 1; add al, bl; mov rcx, [rsi]; mov r9d, 2
  41b801000000 00d8 !488b0e 41b902000000
  SDM 3A 7.5; 3A 7.15 Interrupt 14
load_unmapped_is_precise: rsi=U rax=7f rbx=1 => r8=1 rax=80 f=cpAzSO ! PFR U
@ mov [rdi], rax
  !488907
  SDM 3A 7.15 Interrupt 14
store_unmapped: rdi=U => ! PFW U
store_readonly_page: rdi=R+8 => ! PFW R+8
@ add [rdi], rax
  !480107
  SDM 3A 7.5
rmw_readonly_page_keeps_flags: rdi=R+8 rax=1 => ! PFW R+8
@ inc dword ptr [rdi]
  !ff07
  SDM 3A 7.5
inc_readonly_page_keeps_flags: rdi=R => ! PFW R
@ xchg [rdi], rax
  !488707
  SDM 3A 7.5
xchg_readonly_page_keeps_register: rdi=R rax=5 => ! PFW R
@ cmpxchg [rdi], rcx
  !480fb10f
  SDM 2A CMPXCHG
cmpxchg_mismatch_still_writes: rdi=R rax=0102030405060709 rcx=1 => ! PFW R
@ xadd [rdi], rcx
  !480fc10f
  SDM 3A 7.5
xadd_readonly_page_keeps_register: rdi=R rcx=1 => ! PFW R
@ setz byte ptr [rdi]
  !0f9407
  SDM 3A 7.5
setcc_unmapped: rdi=U => ! PFW U
@ push rax
  !50
  SDM 3A 7.5
push_unmapped_keeps_rsp: rsp=U+8 => ! PFW U
@ pop rax
  !58
  SDM 3A 7.5
pop_unmapped_keeps_rsp: rsp=U => ! PFR U
@ pop qword ptr [rdi]
  !8f07
  SDM 3A 7.5; 2B POP
pop_to_readonly_keeps_rsp: rsp=D rdi=R => ! PFW R
@ push qword ptr [rsi]
  !ff36
  SDM 3A 7.5
push_from_unmapped_keeps_rsp: rsi=U => ! PFR U
@ call 1f; 1: mov eax, 1
  !e800000000 b801000000
  SDM 3A 7.5; 2A CALL
call_unmapped_stack_keeps_rip: rsp=U+8 => ! PFW U
@ ret
  !c3
  SDM 3A 7.5; 2B RET
ret_unmapped_stack_keeps_rip: rsp=U => ! PFR U
@ cmovz rax, [rsi]
  !480f4406
  SDM 2A CMOVcc
cmov_false_still_reads_memory: rsi=U f= => ! PFR U
@ shl qword ptr [rdi], cl
  48d327
  SDM 2B SAL/SHL (read-modify-write of DEST)
shift_zero_count_leaves_memory_and_flags: rdi=D rcx=40 m0=0102030405060708 => f=*
@ shl qword ptr [rdi], cl
  !48d327
  SDM 2B SAL/SHL (tempDEST := DEST for every count; #PF)
shift_zero_count_still_reads_memory: rdi=U rcx=40 => ! PFR U
@ mul qword ptr [rsi]
  !48f726
  SDM 3A 7.5
mul_unmapped_keeps_rax_rdx: rsi=U => ! PFR U
@ movaps xmm0, [rsi+1]
  !0f284601
  SDM 2B MOVAPS; 1 11.6 / 2A 2.4 (exception type 1)
movaps_load_unaligned_is_gp: rsi=D => ! GP
@ movaps [rdi+8], xmm0
  !0f294708
  SDM 2B MOVAPS
movaps_store_unaligned_is_gp: rdi=D => ! GP
@ movdqa xmm1, [rsi+4]
  !660f6f4e04
  SDM 2B MOVDQA
movdqa_load_unaligned_is_gp: rsi=D => ! GP
@ movntdq [rdi+8], xmm0
  !660fe74708
  SDM 2B MOVNTDQ
movntdq_unaligned_is_gp: rdi=D => ! GP
@ paddd xmm0, [rsi+8]
  !660ffe4608
  SDM 2B PADDD; 2A 2.4 (type 4)
paddd_memory_unaligned_is_gp: rsi=D => ! GP
@ addps xmm0, [rsi+8]
  !0f584608
  SDM 2A ADDPS; 2A 2.4 (type 2)
addps_memory_unaligned_is_gp: rsi=D => ! GP
@ addss xmm0, [rsi+1]
  f30f584601
  SDM 2A ADDSS; 2A 2.4 (type 3)
addss_memory_unaligned_is_allowed: rsi=D x0=s:3f800000 m0=000000803f => x0=s:40000000 f=*
@ lock cmpxchg16b [rdi+8]
  !f0480fc74f08
  SDM 2A CMPXCHG16B
cmpxchg16b_unaligned_is_gp: rdi=D => ! GP
@ movss [rdi], xmm0
  !f30f1107
  SDM 3A 7.5
movss_store_readonly: rdi=R+c => ! PFW R+c
@ cld; rep movsb
  fc !f3a4
  SDM 2B REP; 3A 7.5
rep_movsb_fault_stops_at_failing_iteration: rsi=D rdi=U-4 rcx=10 m0=0102030405060708 => rsi=D+4 rdi=U rcx=c m1ffc=01020304 f=* ! PFW U
@ cld; rep stosq
  fc !f348ab
  SDM 2B REP; 3A 7.5
rep_stosq_fault_stops_at_failing_iteration: rdi=U-10 rcx=5 rax=1122334455667788 => rdi=U rcx=3 m1ff0=88776655443322118877665544332211 f=* ! PFW U
@ cld; rep lodsb
  fc !f3ac
  SDM 2B REP; 3A 7.5
rep_lodsb_source_fault: rsi=U-2 rcx=8 rax=0 m1ffe=aabb => rsi=U rcx=6 rax=bb f=* ! PFR U
@ cld; repe cmpsb
  fc !f3a6
  SDM 2B REPE (EFLAGS restored on a fault)
repe_cmpsb_fault_restores_flags: rsi=D rdi=U-2 rcx=8 m0=0102 m1ffe=0102 => rsi=D+2 rdi=U rcx=6 f=* ! PFR U
@ bt qword ptr [rsi], rax
  !480fa306
  SDM 2A BT
bt_memory_register_offset_can_fault: rsi=U-8 rax=40 f= => ! PFR U
@ mov eax, 1; jmp rbx
  b801000000 ffe3
  SDM 3A 7.15 Interrupt 14
jump_to_unmapped_faults_at_target: rbx=U => rax=1 rip=U f=* ! PFX U
@ jmp rbx
  ffe3
  SDM 3A 5.6 (XD); 3A 7.15 Interrupt 14
jump_to_data_page_faults_at_target: rbx=D+40 => rip=D+40 f=* ! PFX D+40
";

const STRADDLING_FAULT_ADDRESS: &str = r"
@ mov rcx, [rsi-4]
  !488b4efc
  SDM 3A 5.7 (the address with no translation); 3A 7.15 Interrupt 14
load_crossing_into_unmapped_page: rsi=U => ! PFR U
@ mov [rdi-4], rax
  !488947fc
  SDM 3A 5.7 (the address with no translation); 3A 7.5
store8_crossing_into_unmapped_page_writes_nothing: rdi=U rax=1122334455667788 => ! PFW U
@ movups xmm0, [rsi-8]
  !0f1046f8
  SDM 3A 5.7 (the address with no translation); 3A 7.5
movups_load_crossing_into_unmapped: rsi=U => ! PFR U
";

const NONCANONICAL_ADDRESS: &str = r"
@ movzx eax, byte ptr [rsi]
  !0fb606
  SDM 1 3.3.7.1; 2A MOVZX
noncanonical_byte_load_faults: rsi=0000800000000000 => ! GP
@ mov ax, [rsi]
  !668b06
  SDM 1 3.3.7.1; 2A MOV
noncanonical_word_load_faults: rsi=0000800000000000 => ! GP
@ mov eax, [rsi]
  !8b06
  SDM 1 3.3.7.1; 2A MOV
noncanonical_dword_load_faults: rsi=0000800000000000 => ! GP
@ mov rax, [rsi]
  !488b06
  SDM 1 3.3.7.1; 2A MOV
noncanonical_qword_load_faults: rsi=0000800000000000 => ! GP
@ mov byte ptr [rdi], al
  !8807
  SDM 1 3.3.7.1; 2A MOV
noncanonical_byte_store_faults: rdi=ffff7fffffffffff => ! GP
@ mov word ptr [rdi], ax
  !668907
  SDM 1 3.3.7.1; 2A MOV
noncanonical_word_store_faults: rdi=ffff7fffffffffff => ! GP
@ mov dword ptr [rdi], eax
  !8907
  SDM 1 3.3.7.1; 2A MOV
noncanonical_dword_store_faults: rdi=ffff7fffffffffff => ! GP
@ mov qword ptr [rdi], rax
  !488907
  SDM 1 3.3.7.1; 2A MOV
noncanonical_qword_store_faults: rdi=ffff7fffffffffff => ! GP
@ mov rax, [rsi]
  !488b06
  SDM 1 3.3.7.1; 2A MOV
qword_crossing_into_lower_hole_faults: rsi=00007fffffffffff => ! GP
qword_ending_at_last_lower_canonical_byte_is_unmapped: rsi=00007ffffffffff8 => ! PFR 00007ffffffffff8
@ movzx eax, byte ptr [rsi]
  !0fb606
  SDM 1 3.3.7.1; 2A MOVZX
last_lower_canonical_byte_is_unmapped: rsi=00007fffffffffff => ! PFR 00007fffffffffff
first_upper_canonical_byte_is_unmapped: rsi=ffff800000000000 => ! PFR ffff800000000000
@ movups xmm0, [rsi]
  !0f1006
  SDM 1 3.3.7.1; 2B MOVUPS
noncanonical_vector_load_faults: rsi=0000800000000000 => ! GP
vector_crossing_into_lower_hole_faults: rsi=00007fffffffffff => ! GP
vector_ending_at_last_lower_canonical_byte_is_unmapped: rsi=00007ffffffffff0 => ! PFR 00007ffffffffff0
@ movups [rdi], xmm0
  !0f1107
  SDM 1 3.3.7.1; 2B MOVUPS
noncanonical_vector_store_faults: rdi=ffff7fffffffffff => ! GP
vector_store_at_first_upper_canonical_byte_is_unmapped: rdi=ffff800000000000 => ! PFR ffff800000000000
@ movdqu xmm0, [rsi]
  !f30f6f06
  SDM 1 3.3.7.1; 2B MOVDQU
movdqu_crossing_into_lower_hole_faults: rsi=00007fffffffffff => ! GP
movdqu_at_first_upper_canonical_byte_is_unmapped: rsi=ffff800000000000 => ! PFR ffff800000000000
@ movdqu [rdi], xmm0
  !f30f7f07
  SDM 1 3.3.7.1; 2B MOVDQU
movdqu_store_crossing_into_lower_hole_faults: rdi=00007fffffffffff => ! GP
@ mov rax, gs:[rsi]
  !65488b06
  SDM 1 3.3.7.1, 3A 4.2; 2A MOV
wrapped_segment_address_is_checked: gsbase=0000800000000000 rsi=ffffffffffffffff => ! GP
noncanonical_segment_base_faults: gsbase=0000800000000000 rsi=0 => ! GP
canonical_upper_segment_base_is_unmapped: gsbase=ffff800000000000 rsi=0 => ! PFR ffff800000000000
";

const PACKED_INTEGER_SSE2: &str = r"
@ paddb xmm1, xmm2
  660ffcca
  SDM 2B PADDB
paddb_wraps: x1=b:ff,80,7f,00,01,fe,55,aa,ff,80,7f,00,01,fe,55,aa x2=b:01,80,01,00,ff,03,55,56,02,81,7f,ff,fe,ff,ab,aa => x1=b:00,00,80,00,00,01,aa,00,01,01,fe,ff,ff,fd,00,54 f=*
@ paddw xmm1, xmm2
  660ffdca
  SDM 2B PADDW
paddw_wraps: x1=w:ffff,8000,7fff,00ff,1234,0,fffe,8001 x2=w:1,8000,1,1,edcc,0,3,7fff => x1=w:0,0,8000,100,0,0,1,0 f=*
@ paddd xmm8, xmm15
  66450ffec7
  SDM 2B PADDD
paddd_no_cross_lane_carry: x8=d:ffffffff,7fffffff,80000000,0 x15=d:1,1,80000000,ffffffff => x8=d:0,80000000,0,ffffffff f=*
@ paddq xmm1, xmm2
  660fd4ca
  SDM 2B PADDQ
paddq_carries_within_quadword: x1=q:ffffffff,ffffffffffffffff x2=q:1,1 => x1=q:100000000,0 f=*
@ psubb xmm1, xmm2
  660ff8ca
  SDM 2B PSUBB
psubb_wraps: x1=b:00,80,7f,00,01,fe,55,aa,0,0,0,0,0,0,0,0 x2=b:01,01,ff,80,01,ff,56,aa,ff,80,7f,01,0,10,f0,aa => x1=b:ff,7f,80,80,00,ff,ff,00,01,80,81,ff,00,f0,10,56 f=*
@ psubw xmm1, xmm2; psubd xmm3, xmm4; psubq xmm5, xmm6
  660ff9ca 660ffadc 660ffbee
  SDM 2B PSUBW/PSUBD; 2B PSUBQ
psubw_psubd_psubq: x1=w:0,8000,7fff,1,ffff,0,0,5 x2=w:1,1,ffff,1,ffff,8000,ffff,3 x3=d:0,80000000,1,5 x4=d:1,1,ffffffff,5 x5=q:0,8000000000000000 x6=q:1,1 => x1=w:ffff,7fff,8000,0,0,8000,1,2 x3=d:ffffffff,7fffffff,2,0 x5=q:ffffffffffffffff,7fffffffffffffff f=*
@ paddsb xmm1, xmm2
  660fecca
  SDM 2B PADDSB
paddsb_saturates: x1=b:7f,80,7f,80,40,c0,01,ff,0,7f,80,70,90,10,f0,64 x2=b:01,ff,7f,80,40,c0,fe,ff,0,ff,01,10,f0,70,90,c8 => x1=b:7f,80,7f,80,7f,80,ff,fe,0,7e,81,7f,80,7f,80,2c f=*
@ paddsw xmm1, xmm2
  660fedca
  SDM 2B PADDSW
paddsw_saturates: x1=w:7fff,8000,7fff,8000,4000,c000,1,ffff x2=w:1,ffff,8000,8000,4000,c000,fffe,ffff => x1=w:7fff,8000,ffff,8000,7fff,8000,ffff,fffe f=*
@ paddusb xmm1, xmm2
  660fdcca
  SDM 2B PADDUSB
paddusb_saturates: x1=b:ff,80,7f,00,01,fe,55,aa,0,0,0,0,0,0,0,0 x2=b:01,80,80,00,ff,01,ab,55,ff,80,7f,0,0,0,0,1 => x1=b:ff,ff,ff,00,ff,ff,ff,ff,ff,80,7f,0,0,0,0,1 f=*
@ paddusw xmm1, xmm2; psubusw xmm3, xmm4
  660fddca 660fd9dc
  SDM 2B PADDUSW; 2B PSUBUSW
paddusw_psubusw_saturate: x1=w:ffff,8000,7fff,1,0,fffe,1234,8001 x2=w:1,8000,8000,1,0,1,1,7fff x3=w:0,1,8000,7fff,ffff,5,1234,0 x4=w:1,1,7fff,8000,1,5,ffff,ffff => x1=w:ffff,ffff,ffff,2,0,ffff,1235,ffff x3=w:0,0,1,0,fffe,0,0,0 f=*
@ psubsb xmm1, xmm2
  660fe8ca
  SDM 2B PSUBSB
psubsb_saturates: x1=b:80,7f,80,7f,00,00,ff,01,0,0,0,0,0,0,0,0 x2=b:01,ff,7f,80,80,7f,ff,01,80,7f,01,ff,0,0,0,0 => x1=b:80,7f,80,7f,7f,81,00,00,7f,81,ff,01,0,0,0,0 f=*
@ psubsw xmm1, xmm2
  660fe9ca
  SDM 2B PSUBSW
psubsw_saturates: x1=w:8000,7fff,0,0,ffff,8000,7fff,1234 x2=w:1,ffff,8000,7fff,ffff,8000,7fff,0234 => x1=w:8000,7fff,7fff,8001,0,0,0,1000 f=*
@ psubusb xmm1, xmm2
  660fd8ca
  SDM 2B PSUBUSB
psubusb_saturates: x1=b:00,01,ff,80,7f,10,20,30,0,0,0,0,ff,ff,ff,ff x2=b:01,01,01,7f,80,0f,21,30,0,1,ff,80,0,1,ff,80 => x1=b:00,00,fe,01,00,01,00,00,0,0,0,0,ff,fe,00,7f f=*
@ pmullw xmm1, xmm2
  660fd5ca
  SDM 2B PMULLW
pmullw_low_half: x1=w:8000,ffff,7fff,100,ffff,1234,8000,3 x2=w:8000,ffff,7fff,100,2,0,2,5 => x1=w:0,1,1,0,fffe,0,0,f f=*
@ pmulhw xmm1, xmm2
  660fe5ca
  SDM 2B PMULHW
pmulhw_signed_high_half: x1=w:8000,ffff,7fff,100,ffff,4000,8000,8000 x2=w:8000,ffff,7fff,100,2,4,7fff,1 => x1=w:4000,0,3fff,1,ffff,1,c000,ffff f=*
@ pmulhuw xmm1, xmm2
  660fe4ca
  SDM 2B PMULHUW
pmulhuw_unsigned_high_half: x1=w:8000,ffff,7fff,100,ffff,4000,8000,8000 x2=w:8000,ffff,7fff,100,2,4,7fff,1 => x1=w:4000,fffe,3fff,1,1,1,3fff,0 f=*
@ pmuludq xmm1, xmm2
  660ff4ca
  SDM 2B PMULUDQ
pmuludq_uses_even_dwords: x1=d:ffffffff,11111111,80000000,22222222 x2=d:ffffffff,33333333,2,44444444 => x1=q:fffffffe00000001,100000000 f=*
@ pmaddwd xmm1, xmm2
  660ff5ca
  SDM 2B PMADDWD
pmaddwd_all_minimums_wrap: x1=w:8000,8000,7fff,7fff,ffff,1,8000,0 x2=w:8000,8000,7fff,7fff,1,ffff,7fff,1234 => x1=d:80000000,7ffe0002,fffffffe,c0008000 f=*
@ psadbw xmm1, xmm2
  660ff6ca
  SDM 2B PSADBW
psadbw: x1=b:ff,0,ff,0,ff,0,ff,0,1,2,3,4,5,6,7,8 x2=b:0,ff,0,ff,0,ff,0,ff,8,7,6,5,4,3,2,1 => x1=q:7f8,20 f=*
@ pavgb xmm1, xmm2; pavgw xmm3, xmm4
  660fe0ca 660fe3dc
  SDM 2B PAVGB/PAVGW
pavgb_pavgw_round_up: x1=b:ff,ff,0,1,fe,80,7f,2,0,0,0,0,0,0,0,0 x2=b:ff,0,1,2,ff,81,80,2,0,0,0,0,0,0,0,1 x3=w:ffff,ffff,0,1,fffe,8000,7fff,2 x4=w:ffff,0,1,2,ffff,8001,8000,2 => x1=b:ff,80,1,2,ff,81,80,2,0,0,0,0,0,0,0,1 x3=w:ffff,8000,1,2,ffff,8001,8000,2 f=*
@ movdqa xmm3, xmm1; pminub xmm1, xmm2; pmaxub xmm3, xmm2; movdqa xmm6, xmm4; pminsw xmm4, xmm5; pmaxsw xmm6, xmm5
  660f6fd9 660fdaca 660fdeda 660f6ff4 660feae5 660feef5
  SDM 2B PMINUB; 2B PMAXUB; 2B PMINSW; 2B PMAXSW
pmin_pmax_ub_sw: x1=b:80,7f,ff,0,1,fe,0,0,0,0,0,0,0,0,0,0 x2=b:7f,80,0,ff,1,ff,0,0,0,0,0,0,0,0,0,1 x4=w:8000,7fff,ffff,0,1,0,0,0 x5=w:7fff,8000,0,ffff,1,0,0,0 => x1=b:7f,7f,0,0,1,fe,0,0,0,0,0,0,0,0,0,0 x3=b:80,80,ff,ff,1,ff,0,0,0,0,0,0,0,0,0,1 x4=w:8000,8000,ffff,ffff,1,0,0,0 x6=w:7fff,7fff,0,0,1,0,0,0 f=*
@ pcmpeqb xmm1, xmm2; pcmpeqw xmm3, xmm4; pcmpeqd xmm5, xmm6
  660f74ca 660f75dc 660f76ee
  SDM 2B PCMPEQB/W/D
pcmpeqb_w_d: x1=b:0,1,2,3,4,5,6,7,8,9,a,b,c,d,e,f x2=b:0,0,2,0,4,0,6,0,8,0,a,0,c,0,e,0 x3=w:1,100,ffff,0,8000,7fff,5,6 x4=w:1,1,ffff,0,8000,8000,5,7 x5=d:1,2,80000000,ffffffff x6=d:1,3,80000000,7fffffff => x1=b:ff,0,ff,0,ff,0,ff,0,ff,0,ff,0,ff,0,ff,0 x3=w:ffff,0,ffff,ffff,ffff,0,ffff,0 x5=d:ffffffff,0,ffffffff,0 f=*
@ pcmpgtb xmm1, xmm2; pcmpgtw xmm3, xmm4; pcmpgtd xmm5, xmm6
  660f64ca 660f65dc 660f66ee
  SDM 2B PCMPGTB/W/D
pcmpgt_is_signed: x1=b:7f,80,0,ff,1,0,5,fb,0,0,0,0,0,0,0,0 x2=b:80,7f,ff,0,0,1,5,fa,0,0,0,0,0,0,0,0 x3=w:7fff,8000,0,ffff,1,0,5,0 x4=w:8000,7fff,ffff,0,0,1,5,0 x5=d:7fffffff,80000000,0,1 x6=d:80000000,7fffffff,ffffffff,1 => x1=b:ff,0,ff,0,ff,0,0,ff,0,0,0,0,0,0,0,0 x3=w:ffff,0,ffff,0,ffff,0,0,0 x5=d:ffffffff,0,ffffffff,0 f=*
@ pandn xmm1, xmm2
  660fdfca
  SDM 2B PANDN
pandn_inverts_first_operand: x1=q:ffff0000ffff0000,0 x2=q:ff00ff00ff00ff00,0123456789abcdef => x1=q:0000ff000000ff00,0123456789abcdef f=*
@ pand xmm1, xmm2; por xmm3, xmm2; pxor xmm4, xmm2; pxor xmm5, xmm5
  660fdbca 660febda 660fefe2 660fefed
  SDM 2B PAND; 2B POR; 2B PXOR
pand_por_pxor: x1=q:ffff0000ffff0000,f x2=q:ff00ff00ff00ff00,3 x3=q:00ff000000ff0000,4 x4=q:ffffffffffffffff,3 => x1=q:ff000000ff000000,3 x3=q:ffffff00ffffff00,7 x4=q:00ff00ff00ff00ff,0 x5=q:0,0 f=*
@ psllw xmm1, 15; psllw xmm2, 16; psllw xmm3, 0
  660f71f10f 660f71f210 660f71f300
  SDM 2B PSLLW
psllw_imm_15_and_16: x1=w:1,2,3,ffff,8000,0,5,6 x2=w:1,2,3,ffff,8000,0,5,6 x3=w:1,2,3,4,5,6,7,8 => x1=w:8000,0,8000,8000,0,0,8000,0 x2=w:0,0,0,0,0,0,0,0 x3=w:1,2,3,4,5,6,7,8 f=*
@ pslld xmm1, 31; pslld xmm2, 32; psllq xmm3, 63; psllq xmm4, 64; psllq xmm5, 255
  660f72f11f 660f72f220 660f73f33f 660f73f440 660f73f5ff
  SDM 2B PSLLD; 2B PSLLQ
pslld_psllq_boundary: x1=d:1,2,3,ffffffff x2=d:1,2,3,ffffffff x3=q:1,2 x4=q:1,ffffffffffffffff x5=q:1,ffffffffffffffff => x1=d:80000000,0,80000000,80000000 x2=q:0,0 x3=q:8000000000000000,0 x4=q:0,0 x5=q:0,0 f=*
@ psllw xmm1, xmm7; pslld xmm2, xmm7; psllq xmm3, xmm7
  660ff1cf 660ff2d7 660ff3df
  SDM 2B PSLLW/PSLLD/PSLLQ
psll_register_count_uses_all_low_64_bits: x1=w:1,2,3,4,5,6,7,8 x2=d:1,2,3,4 x3=q:1,2 x7=q:100000001,0 => x1=q:0,0 x2=q:0,0 x3=q:0,0 f=*
psll_register_count_ignores_high_64_bits: x1=w:1,2,3,4,5,6,7,8 x2=d:1,2,3,4 x3=q:1,8000000000000000 x7=q:1,ffffffffffffffff => x1=w:2,4,6,8,a,c,e,10 x2=d:2,4,6,8 x3=q:2,0 f=*
@ psrlw xmm1, 15; psrlw xmm2, 16; psrld xmm3, 31; psrld xmm4, 32; psrlq xmm5, 63; psrlq xmm6, 64
  660f71d10f 660f71d210 660f72d31f 660f72d420 660f73d53f 660f73d640
  SDM 2B PSRLW/PSRLD/PSRLQ
psrl_boundary: x1=w:8000,7fff,ffff,0,1,c000,0,ffff x2=q:ffffffffffffffff,ffffffffffffffff x3=d:80000000,7fffffff,ffffffff,1 x4=q:ffffffffffffffff,ffffffffffffffff x5=q:8000000000000000,7fffffffffffffff x6=q:ffffffffffffffff,1 => x1=w:1,0,1,0,0,1,0,1 x2=q:0,0 x3=d:1,0,1,0 x4=q:0,0 x5=q:1,0 x6=q:0,0 f=*
@ psraw xmm1, 16; psraw xmm2, 255; psrad xmm3, 32; psrad xmm4, xmm7
  660f71e110 660f71e2ff 660f72e320 660fe2e7
  SDM 2B PSRAW/PSRAD
psraw_psrad_fill_with_sign_past_width: x1=w:8000,7fff,ffff,1,c000,0,fffe,4000 x2=w:8000,7fff,ffff,1,c000,0,fffe,4000 x3=d:80000000,7fffffff,ffffffff,1 x4=d:80000000,7fffffff,c0000000,1 x7=q:8000000000000000,0 => x1=w:ffff,0,ffff,0,ffff,0,ffff,0 x2=w:ffff,0,ffff,0,ffff,0,ffff,0 x3=d:ffffffff,0,ffffffff,0 x4=d:ffffffff,0,ffffffff,0 f=*
@ psraw xmm1, 15; psrad xmm2, 1
  660f71e10f 660f72e201
  SDM 2B PSRAW/PSRAD
psraw_by_15_and_1: x1=w:8000,7fff,ffff,1,c000,0,fffe,4000 x2=d:80000001,7fffffff,ffffffff,1 => x1=w:ffff,0,ffff,0,ffff,0,ffff,0 x2=d:c0000000,3fffffff,ffffffff,0 f=*
@ pslldq xmm1, 1; pslldq xmm2, 15; pslldq xmm3, 16; psrldq xmm4, 1; psrldq xmm5, 15; psrldq xmm6, 255
  660f73f901 660f73fa0f 660f73fb10 660f73dc01 660f73dd0f 660f73deff
  SDM 2B PSLLDQ; 2B PSRLDQ
pslldq_psrldq_boundaries: x1=b:1,2,3,4,5,6,7,8,9,a,b,c,d,e,f,10 x2=b:1,2,3,4,5,6,7,8,9,a,b,c,d,e,f,10 x3=b:1,2,3,4,5,6,7,8,9,a,b,c,d,e,f,10 x4=b:1,2,3,4,5,6,7,8,9,a,b,c,d,e,f,10 x5=b:1,2,3,4,5,6,7,8,9,a,b,c,d,e,f,10 x6=b:1,2,3,4,5,6,7,8,9,a,b,c,d,e,f,10 => x1=b:0,1,2,3,4,5,6,7,8,9,a,b,c,d,e,f x2=b:0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,1 x3=q:0,0 x4=b:2,3,4,5,6,7,8,9,a,b,c,d,e,f,10,0 x5=b:10,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0 x6=q:0,0 f=*
@ packsswb xmm1, xmm2
  660f63ca
  SDM 2B PACKSSWB
packsswb_saturates_signed: x1=w:7f,80,ff80,ff7f,0,7fff,8000,ffff x2=w:1,ffff,100,ff00,7e,81,ff81,fe7f => x1=b:7f,7f,80,80,0,7f,80,ff,1,ff,7f,80,7e,7f,81,80 f=*
@ packuswb xmm1, xmm2
  660f67ca
  SDM 2B PACKUSWB
packuswb_saturates_unsigned: x1=w:ff,100,ffff,8000,7fff,0,1,80 x2=w:fe,101,ff00,0,200,7f,81,ffff => x1=b:ff,ff,0,0,ff,0,1,80,fe,ff,0,0,ff,7f,81,0 f=*
@ packssdw xmm1, xmm1
  660f6bc9
  SDM 2B PACKSSDW
packssdw_same_register: x1=d:7fff,8000,ffff8000,ffff7fff => x1=w:7fff,7fff,8000,8000,7fff,7fff,8000,8000 f=*
@ movdqa xmm3, xmm1; punpcklbw xmm1, xmm2; punpckhbw xmm3, xmm2
  660f6fd9 660f60ca 660f68da
  SDM 2B PUNPCKLBW; 2B PUNPCKHBW
punpckl_h_bw: x1=b:0,1,2,3,4,5,6,7,8,9,a,b,c,d,e,f x2=b:10,11,12,13,14,15,16,17,18,19,1a,1b,1c,1d,1e,1f => x1=b:0,10,1,11,2,12,3,13,4,14,5,15,6,16,7,17 x3=b:8,18,9,19,a,1a,b,1b,c,1c,d,1d,e,1e,f,1f f=*
@ movdqa xmm3, xmm1; movdqa xmm4, xmm1; movdqa xmm5, xmm1; movdqa xmm6, xmm1; movdqa xmm7, xmm1; punpcklwd xmm1, xmm2; punpckhwd xmm3, xmm2; punpckldq xmm4, xmm2; punpckhdq xmm5, xmm2; punpcklqdq xmm6, xmm2; punpckhqdq xmm7, xmm2
  660f6fd9 660f6fe1 660f6fe9 660f6ff1 660f6ff9 660f61ca 660f69da 660f62e2 660f6aea 660f6cf2 660f6dfa
  SDM 2B PUNPCKL*; 2B PUNPCKH*
punpck_wd_dq_qdq: x1=w:0,1,2,3,4,5,6,7 x2=w:10,11,12,13,14,15,16,17 => x1=w:0,10,1,11,2,12,3,13 x3=w:4,14,5,15,6,16,7,17 x4=w:0,1,10,11,2,3,12,13 x5=w:4,5,14,15,6,7,16,17 x6=w:0,1,2,3,10,11,12,13 x7=w:4,5,6,7,14,15,16,17 f=*
@ pshufd xmm1, xmm2, 0x1b; pshuflw xmm3, xmm2, 0x93; pshufhw xmm4, xmm2, 0x39
  660f70ca1b f20f70da93 f30f70e239
  SDM 2B PSHUFD; 2B PSHUFLW; 2B PSHUFHW
pshufd_pshuflw_pshufhw: x2=w:0,1,2,3,4,5,6,7 => x1=w:6,7,4,5,2,3,0,1 x3=w:3,0,1,2,4,5,6,7 x4=w:0,1,2,3,5,6,7,4 f=*
@ shufps xmm1, xmm2, 0x1b; shufpd xmm3, xmm4, 1; shufps xmm5, xmm5, 0x4e
  0fc6ca1b 660fc6dc01 0fc6ed4e
  SDM 2B SHUFPS; 2B SHUFPD
shufps_shufpd: x1=d:a0,a1,a2,a3 x2=d:b0,b1,b2,b3 x3=q:1,2 x4=q:3,4 x5=d:c0,c1,c2,c3 => x1=d:a3,a2,b1,b0 x3=q:2,3 x5=d:c2,c3,c0,c1 f=*
@ movaps xmm3, xmm1; movaps xmm4, xmm1; movaps xmm5, xmm1; unpcklps xmm1, xmm2; unpckhps xmm3, xmm2; unpcklpd xmm4, xmm2; unpckhpd xmm5, xmm2
  0f28d9 0f28e1 0f28e9 0f14ca 0f15da 660f14e2 660f15ea
  SDM 2B UNPCK*
unpcklps_unpckhps_unpcklpd_unpckhpd: x1=d:a0,a1,a2,a3 x2=d:b0,b1,b2,b3 => x1=d:a0,b0,a1,b1 x3=d:a2,b2,a3,b3 x4=d:a0,a1,b0,b1 x5=d:a2,a3,b2,b3 f=*
@ pextrw eax, xmm1, 7; pextrw ecx, xmm1, 0x0a; pinsrw xmm2, ebx, 0x0b
  660fc5c107 660fc5c90a 660fc4d30b
  SDM 2B PEXTRW; 2B PINSRW
pextrw_pinsrw_index_mask: x1=w:0,1,f002,3,4,5,6,f007 x2=w:0,0,0,0,0,0,0,0 rax=ffffffffffffffff rcx=ffffffffffffffff rbx=ffff1234 => rax=f007 rcx=f002 x2=w:0,0,0,1234,0,0,0,0 f=*
@ pmovmskb eax, xmm1; movmskps ecx, xmm1; movmskpd edx, xmm1
  660fd7c1 0f50c9 660f50d1
  SDM 2B PMOVMSKB; 2B MOVMSKPS; 2B MOVMSKPD
pmovmskb_movmskps_movmskpd: x1=b:80,7f,0,ff,0,0,0,80,0,0,0,0,80,0,0,80 rax=ffffffffffffffff rcx=ffffffffffffffff rdx=ffffffffffffffff => rax=9089 rcx=b rdx=3 f=*
@ movd xmm1, eax; movq xmm2, rax; movd ecx, xmm3; movq rdx, xmm3; movq xmm4, xmm3
  660f6ec8 66480f6ed0 660f7ed9 66480f7eda f30f7ee3
  SDM 2B MOVD/MOVQ
movd_movq_zero_upper_bits: rax=1122334455667788 rcx=ffffffffffffffff x3=q:8877665544332211,5555555555555555 => x1=d:55667788,0,0,0 x2=q:1122334455667788,0 rcx=44332211 rdx=8877665544332211 x4=q:8877665544332211,0 f=*
@ movss xmm1, xmm2; movss xmm3, dword ptr [rsi]
  f30f10ca f30f101e
  SDM 2B MOVSS
movss_register_merges_memory_zeroes: x1=d:1,2,3,4 x2=d:5,6,7,8 x3=d:9,a,b,c rsi=D m0=44332211 => x1=d:5,2,3,4 x3=d:11223344,0,0,0 f=*
@ movsd xmm1, xmm2; movsd xmm3, qword ptr [rsi]
  f20f10ca f20f101e
  SDM 2B MOVSD
movsd_register_merges_memory_zeroes: x1=q:1,2 x2=q:3,4 x3=q:5,6 rsi=D m0=8877665544332211 => x1=q:3,2 x3=q:1122334455667788,0 f=*
@ movlps xmm1, [rsi]; movhps xmm2, [rsi]; movhlps xmm3, xmm4; movlhps xmm5, xmm4; movhps [rdi], xmm4
  0f120e 0f1616 0f12dc 0f16ec 0f1727
  SDM 2B MOVLPS; 2B MOVHPS; 2B MOVHLPS; 2B MOVLHPS
movlps_movhps_movhlps_movlhps: x1=q:1,2 x2=q:3,4 x3=q:5,6 x4=q:7,8 x5=q:9,a rsi=D rdi=D+8 m0=efcdab8967452301 m8=0000000000000000 => x1=q:0123456789abcdef,2 x2=q:3,0123456789abcdef x3=q:8,6 x5=q:9,7 m8=0800000000000000 f=*
@ movdqa [rdi], xmm1; movaps xmm2, [rdi]; movntdq [rdi+0x10], xmm1; movnti [rdi+0x20], rax
  660f7f0f 0f2817 660fe74f10 480fc34720
  SDM 2B MOVDQA; 2B MOVAPS
movdqa_movaps_aligned_store_and_load: x1=q:1122334455667788,99aabbccddeeff00 rdi=D rax=0102030405060708 m0=00 m10=00 m20=00 => x2=q:1122334455667788,99aabbccddeeff00 m0=887766554433221100ffeeddccbbaa99887766554433221100ffeeddccbbaa990807060504030201 f=*
@ maskmovdqu xmm1, xmm2
  660ff7ca
  SDM 2B MASKMOVDQU
maskmovdqu_selects_bytes: x1=b:0,1,2,3,4,5,6,7,8,9,a,b,c,d,e,f x2=b:80,7f,ff,0,80,0,0,0,0,0,0,0,0,0,0,81 rdi=D+1 m0=ffffffffffffffffffffffffffffffffffff => m0=ff00ff02ff04ffffffffffffffffffff0fff f=*
@ andnps xmm1, xmm2; xorps xmm3, xmm3; orps xmm4, xmm2; andpd xmm5, xmm2
  0f55ca 0f57db 0f56e2 660f54ea
  SDM 2A ANDNPS; 2B XORPS; 2B ORPS
andnps_xorps_orps: x1=q:ffffffff00000000,0 x2=q:123456789abcdef0,7 x4=q:8000000000000000,8 x5=q:ff,5 => x1=q:9abcdef0,7 x3=q:0,0 x4=q:923456789abcdef0,f x5=q:f0,5 f=*
@ paddd xmm9, xmmword ptr [r12+r13*4+0x10]
  66470ffe4cac10
  SDM 2B PADDD; 2A 2.2.1.2
paddd_memory_source_high_registers: x9=d:1,2,3,4 r12=D r13=4 m20=0a0000000b0000000c0000000d000000 => x9=d:b,d,f,11 f=*
@ movdqa [rdi], xmm1; movaps xmm2, [rdi]; movntdq [rdi+0x10], xmm1
  660f7f0f 0f2817 660fe74f10
  SDM 2B MOVDQA; 2B MOVAPS; 2B MOVNTDQ
movdqa_movaps_movntdq_round_trip: x1=q:1122334455667788,99aabbccddeeff00 rdi=D m0=00 => x2=q:1122334455667788,99aabbccddeeff00 m0=887766554433221100ffeeddccbbaa99887766554433221100ffeeddccbbaa99 f=*
";

const PACKED_INTEGER_BEYOND_SSE2: &str = r"
@ pshufb xmm1, xmm2
  660f3800ca
  SDM 2B PSHUFB
pshufb: x1=b:10,11,12,13,14,15,16,17,18,19,1a,1b,1c,1d,1e,1f x2=b:f,e,80,0,ff,1,10,1f,7f,81,2,3,4,5,6,7 => x1=b:1f,1e,0,10,0,11,10,1f,1f,0,12,13,14,15,16,17 f=*
@ palignr xmm1, xmm2, 5; palignr xmm3, xmm2, 17; palignr xmm4, xmm2, 32
  660f3a0fca05 660f3a0fda11 660f3a0fe220
  SDM 2B PALIGNR
palignr: x1=b:10,11,12,13,14,15,16,17,18,19,1a,1b,1c,1d,1e,1f x2=b:0,1,2,3,4,5,6,7,8,9,a,b,c,d,e,f x3=b:10,11,12,13,14,15,16,17,18,19,1a,1b,1c,1d,1e,1f => x1=b:5,6,7,8,9,a,b,c,d,e,f,10,11,12,13,14 x3=b:11,12,13,14,15,16,17,18,19,1a,1b,1c,1d,1e,1f,0 x4=q:0,0 f=*
@ pabsb xmm1, xmm2; pabsw xmm3, xmm4; pabsd xmm5, xmm6
  660f381cca 660f381ddc 660f381eee
  SDM 2B PABSB/PABSW/PABSD
pabs_min_is_unchanged: x2=b:80,81,7f,ff,0,1,fe,c0,0,0,0,0,0,0,0,0 x4=w:8000,8001,7fff,ffff,0,1,fffe,c000 x6=d:80000000,ffffffff,7fffffff,0 => x1=b:80,7f,7f,1,0,1,2,40,0,0,0,0,0,0,0,0 x3=w:8000,7fff,7fff,1,0,1,2,4000 x5=d:80000000,1,7fffffff,0 f=*
@ phaddw xmm1, xmm2; phaddd xmm3, xmm4; phsubw xmm5, xmm6
  660f3801ca 660f3802dc 660f3805ee
  SDM 2B PHADDW/PHADDD; 2B PHSUBW
phaddw_phaddd_phsubw: x1=w:1,2,7fff,1,ffff,1,8000,8000 x2=w:3,4,5,6,7,8,9,a x3=d:ffffffff,1,7fffffff,1 x4=d:2,3,4,5 x5=w:1,2,8000,1,5,5,0,ffff x6=w:9,1,0,0,0,1,1,0 => x1=w:3,8000,0,0,7,b,f,13 x3=d:0,80000000,5,9 x5=w:ffff,7fff,0,1,8,0,ffff,1 f=*
@ phaddsw xmm1, xmm2; pmaddubsw xmm3, xmm4
  660f3803ca 660f3804dc
  SDM 2B PHADDSW; 2B PMADDUBSW
phaddsw_pmaddubsw_saturate: x1=w:7fff,1,8000,ffff,1,2,0,0 x2=w:4000,4000,c000,bfff,5,6,ffff,ffff x3=b:ff,ff,ff,ff,1,2,80,80,0,0,0,0,0,0,0,0 x4=b:7f,7f,80,80,ff,3,1,ff,0,0,0,0,0,0,0,0 => x1=w:7fff,8000,3,0,7fff,8000,b,fffe x3=w:7fff,8000,5,0,0,0,0,0 f=*
@ pmulhrsw xmm1, xmm2
  660f380bca
  SDM 2B PMULHRSW
pmulhrsw: x1=w:8000,7fff,4000,1,ffff,8000,0,4000 x2=w:8000,7fff,4000,4000,4000,7fff,0,1 => x1=w:8000,7ffe,2000,1,0,8001,0,1 f=*
@ psignb xmm1, xmm2; psignw xmm3, xmm4; psignd xmm5, xmm6
  660f3808ca 660f3809dc 660f380aee
  SDM 2B PSIGNB/PSIGNW/PSIGND
psign: x1=b:80,7f,1,5,0,0,0,0,0,0,0,0,0,0,0,0 x2=b:ff,80,0,1,ff,0,1,0,0,0,0,0,0,0,0,0 x3=w:8000,7fff,1,5,0,0,0,0 x4=w:ffff,8000,0,1,ffff,0,1,0 x5=d:80000000,5,5,5 x6=d:ffffffff,0,80000000,1 => x1=b:80,81,0,5,0,0,0,0,0,0,0,0,0,0,0,0 x3=w:8000,8001,0,5,0,0,0,0 x5=d:80000000,0,fffffffb,5 f=*
@ pmulld xmm1, xmm2; pmuldq xmm3, xmm4
  660f3840ca 660f3828dc
  SDM 2B PMULLD; 2B PMULDQ
pmulld_pmuldq: x1=d:ffffffff,80000000,10000,7fffffff x2=d:ffffffff,2,10000,2 x3=d:ffffffff,11111111,80000000,0 x4=d:2,22222222,80000000,0 => x1=d:1,0,0,fffffffe x3=q:fffffffffffffffe,4000000000000000 f=*
@ pminsb xmm1, xmm0; pmaxsb xmm2, xmm0; pminuw xmm3, xmm0; pmaxuw xmm4, xmm0; pminsd xmm5, xmm0; pmaxud xmm6, xmm0
  660f3838c8 660f383cd0 660f383ad8 660f383ee0 660f3839e8 660f383ff0
  SDM 2B PMINSB; 2B PMAXSB; 2B PMINUW; 2B PMAXUW; 2B PMINSD; 2B PMAXUD
pmin_pmax_sse41: x0=d:80000000,7fffffff,0,ffffffff x1=q:0,0 x2=q:0,0 x3=q:1000100010001000,1000100010001000 x4=q:1000100010001000,1000100010001000 x5=d:0,0,0,0 x6=d:1,1,1,1 => x1=b:0,0,0,80,ff,ff,ff,0,0,0,0,0,ff,ff,ff,ff x2=b:0,0,0,0,0,0,0,7f,0,0,0,0,0,0,0,0 x3=w:0,1000,1000,1000,0,0,1000,1000 x4=w:1000,8000,ffff,7fff,1000,1000,ffff,ffff x5=d:80000000,0,0,ffffffff x6=d:80000000,7fffffff,1,ffffffff f=*
@ packusdw xmm1, xmm2
  660f382bca
  SDM 2B PACKUSDW
packusdw: x1=d:ffff,10000,ffffffff,0 x2=d:80000000,7fffffff,1,8000 => x1=w:ffff,ffff,0,0,0,ffff,1,8000 f=*
@ pcmpeqq xmm1, xmm2; pcmpgtq xmm3, xmm4
  660f3829ca 660f3837dc
  SDM 2B PCMPEQQ; 2B PCMPGTQ
pcmpeqq_pcmpgtq: x1=q:1,ffffffff00000000 x2=q:1,ffffffff00000001 x3=q:7fffffffffffffff,8000000000000000 x4=q:8000000000000000,7fffffffffffffff => x1=q:ffffffffffffffff,0 x3=q:ffffffffffffffff,0 f=*
@ pmovsxbw xmm1, xmm0; pmovzxbd xmm2, xmm0; pmovsxwq xmm3, xmm0; pmovzxdq xmm4, xmm0; pmovsxdq xmm5, xmm0
  660f3820c8 660f3831d0 660f3824d8 660f3835e0 660f3825e8
  SDM 2B PMOVSX; 2B PMOVZX
pmovsx_pmovzx: x0=b:80,7f,ff,1,0,80,ff,ff,0,0,0,0,0,0,0,0 => x1=w:ff80,7f,ffff,1,0,ff80,ffff,ffff x2=d:80,7f,ff,1 x3=q:7f80,1ff x4=q:01ff7f80,ffff8000 x5=q:01ff7f80,ffffffffffff8000 f=*
@ pextrb eax, xmm1, 0x1f; pextrd ecx, xmm1, 2; pextrq rdx, xmm1, 1; pinsrb xmm2, ebx, 3; pinsrd xmm3, ebx, 1; pinsrq xmm4, rbx, 1
  660f3a14c81f 660f3a16c902 66480f3a16ca01 660f3a20d303 660f3a22db01 66480f3a22e301
  SDM 2B PEXTRB/PEXTRD/PEXTRQ; 2B PINSRB/PINSRD/PINSRQ
pextr_pinsr_sse41: x1=q:1,f123456789abcdef x2=q:0,0 x3=q:0,0 x4=q:0,0 rbx=1122334455667788 rax=ffffffffffffffff rcx=ffffffffffffffff => rax=f1 rcx=89abcdef rdx=f123456789abcdef x2=q:88000000,0 x3=q:5566778800000000,0 x4=q:0,1122334455667788 f=*
@ ptest xmm1, xmm2
  660f3817ca
  SDM 2B PTEST
ptest_flags: x1=q:ff00,0 x2=q:00ff,0 => f=cpaZso
ptest_carry_when_source_is_subset: x1=q:ff0f,1 x2=q:0f0f,1 => f=Cpazso
@ pblendvb xmm1, xmm2
  660f3810ca
  SDM 2B PBLENDVB
pblendvb_uses_xmm0_msb: x0=b:80,0,ff,7f,0,0,0,0,0,0,0,0,0,0,0,80 x1=b:1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1 x2=b:2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2 => x1=b:2,1,2,1,1,1,1,1,1,1,1,1,1,1,1,2 f=*
@ pblendw xmm1, xmm2, 0x81; blendps xmm3, xmm2, 5
  660f3a0eca81 660f3a0cda05
  SDM 2B PBLENDW; 2A BLENDPS
pblendw_blendps: x1=w:0,0,0,0,0,0,0,0 x2=w:1,2,3,4,5,6,7,8 x3=d:0,0,0,0 => x1=w:1,0,0,0,0,0,0,8 x3=w:1,2,0,0,5,6,0,0 f=*
@ insertps xmm1, xmm2, 0x98
  660f3a21ca98
  SDM 2A INSERTPS
insertps_zero_mask: x1=d:1,2,3,4 x2=d:5,6,7,8 => x1=d:1,7,3,0 f=*
@ phminposuw xmm1, xmm2
  660f3841ca
  SDM 2B PHMINPOSUW
phminposuw: x2=w:5,3,ffff,3,9,8,7,6 => x1=w:3,1,0,0,0,0,0,0 f=*
@ mpsadbw xmm1, xmm2, 5
  660f3a42ca05
  SDM 2B MPSADBW
mpsadbw: x1=b:0,1,2,3,4,5,6,7,8,9,a,b,c,d,e,f x2=b:0,0,0,0,4,5,6,7,0,0,0,0,0,0,0,0 => x1=w:0,4,8,c,10,14,18,1c f=*
";

const FP_ROUNDING: &str = r"
@ addss xmm1, xmm2
  f30f58ca
  SDM 2A ADDSS; 1 4.8.4 Table 4-9
addss_tie_nearest: x1=s:3f800000 x2=s:33800000 mx=1f80 => x1=s:3f800000 mx=1fa0 f=*
addss_tie_down: x1=s:3f800000 x2=s:33800000 mx=3f80 => x1=s:3f800000 mx=3fa0 f=*
addss_tie_up: x1=s:3f800000 x2=s:33800000 mx=5f80 => x1=s:3f800001 mx=5fa0 f=*
addss_tie_zero: x1=s:3f800000 x2=s:33800000 mx=7f80 => x1=s:3f800000 mx=7fa0 f=*
addss_tie_odd_nearest: x1=s:3f800001 x2=s:33800000 mx=1f80 => x1=s:3f800002 mx=1fa0 f=*
addss_tie_odd_down: x1=s:3f800001 x2=s:33800000 mx=3f80 => x1=s:3f800001 mx=3fa0 f=*
addss_tie_odd_up: x1=s:3f800001 x2=s:33800000 mx=5f80 => x1=s:3f800002 mx=5fa0 f=*
addss_tie_odd_zero: x1=s:3f800001 x2=s:33800000 mx=7f80 => x1=s:3f800001 mx=7fa0 f=*
@ subss xmm1, xmm2
  f30f5cca
  SDM 2B SUBSS; 1 4.8.4 Table 4-9
subss_negative_tie_nearest: x1=s:bf800000 x2=s:33800000 mx=1f80 => x1=s:bf800000 mx=1fa0 f=*
subss_negative_tie_down: x1=s:bf800000 x2=s:33800000 mx=3f80 => x1=s:bf800001 mx=3fa0 f=*
subss_negative_tie_up: x1=s:bf800000 x2=s:33800000 mx=5f80 => x1=s:bf800000 mx=5fa0 f=*
subss_negative_tie_zero: x1=s:bf800000 x2=s:33800000 mx=7f80 => x1=s:bf800000 mx=7fa0 f=*
@ divsd xmm1, xmm2
  f20f5eca
  SDM 2A DIVSD; 1 4.8.4 Table 4-9
divsd_one_third_nearest: x1=r:3ff0000000000000 x2=r:4008000000000000 mx=1f80 => x1=r:3fd5555555555555 mx=1fa0 f=*
divsd_one_third_down: x1=r:3ff0000000000000 x2=r:4008000000000000 mx=3f80 => x1=r:3fd5555555555555 mx=3fa0 f=*
divsd_one_third_up: x1=r:3ff0000000000000 x2=r:4008000000000000 mx=5f80 => x1=r:3fd5555555555556 mx=5fa0 f=*
divsd_one_third_zero: x1=r:3ff0000000000000 x2=r:4008000000000000 mx=7f80 => x1=r:3fd5555555555555 mx=7fa0 f=*
@ divss xmm1, xmm2
  f30f5eca
  SDM 2A DIVSS; 1 4.8.4 Table 4-9
divss_one_third_nearest: x1=s:3f800000 x2=s:40400000 mx=1f80 => x1=s:3eaaaaab mx=1fa0 f=*
divss_one_third_down: x1=s:3f800000 x2=s:40400000 mx=3f80 => x1=s:3eaaaaaa mx=3fa0 f=*
divss_one_third_up: x1=s:3f800000 x2=s:40400000 mx=5f80 => x1=s:3eaaaaab mx=5fa0 f=*
divss_one_third_zero: x1=s:3f800000 x2=s:40400000 mx=7f80 => x1=s:3eaaaaaa mx=7fa0 f=*
@ divsd xmm1, xmm2
  f20f5eca
  SDM 2A DIVSD; 1 4.8.4 Table 4-9
divsd_minus_two_thirds_nearest: x1=r:c000000000000000 x2=r:4008000000000000 mx=1f80 => x1=r:bfe5555555555555 mx=1fa0 f=*
divsd_minus_two_thirds_down: x1=r:c000000000000000 x2=r:4008000000000000 mx=3f80 => x1=r:bfe5555555555556 mx=3fa0 f=*
divsd_minus_two_thirds_up: x1=r:c000000000000000 x2=r:4008000000000000 mx=5f80 => x1=r:bfe5555555555555 mx=5fa0 f=*
divsd_minus_two_thirds_zero: x1=r:c000000000000000 x2=r:4008000000000000 mx=7f80 => x1=r:bfe5555555555555 mx=7fa0 f=*
@ mulsd xmm1, xmm2
  f20f59ca
  SDM 2B MULSD; 1 4.8.4 Table 4-9
mulsd_tenth_squared_nearest: x1=r:3fb999999999999a x2=r:3fb999999999999a mx=1f80 => x1=r:3f847ae147ae147c mx=1fa0 f=*
mulsd_tenth_squared_down: x1=r:3fb999999999999a x2=r:3fb999999999999a mx=3f80 => x1=r:3f847ae147ae147b mx=3fa0 f=*
mulsd_tenth_squared_up: x1=r:3fb999999999999a x2=r:3fb999999999999a mx=5f80 => x1=r:3f847ae147ae147c mx=5fa0 f=*
mulsd_tenth_squared_zero: x1=r:3fb999999999999a x2=r:3fb999999999999a mx=7f80 => x1=r:3f847ae147ae147b mx=7fa0 f=*
@ addsd xmm1, xmm2
  f20f58ca
  SDM 2A ADDSD; 1 4.8.4 Table 4-9
addsd_tenths_nearest: x1=r:3fb999999999999a x2=r:3fc999999999999a mx=1f80 => x1=r:3fd3333333333334 mx=1fa0 f=*
addsd_tenths_down: x1=r:3fb999999999999a x2=r:3fc999999999999a mx=3f80 => x1=r:3fd3333333333333 mx=3fa0 f=*
addsd_tenths_up: x1=r:3fb999999999999a x2=r:3fc999999999999a mx=5f80 => x1=r:3fd3333333333334 mx=5fa0 f=*
addsd_tenths_zero: x1=r:3fb999999999999a x2=r:3fc999999999999a mx=7f80 => x1=r:3fd3333333333333 mx=7fa0 f=*
@ sqrtsd xmm1, xmm2
  f20f51ca
  SDM 2B SQRTSD; 1 4.8.4 Table 4-9
sqrtsd_two_nearest: x2=r:4000000000000000 mx=1f80 => x1=r:3ff6a09e667f3bcd mx=1fa0 f=*
sqrtsd_two_down: x2=r:4000000000000000 mx=3f80 => x1=r:3ff6a09e667f3bcc mx=3fa0 f=*
sqrtsd_two_up: x2=r:4000000000000000 mx=5f80 => x1=r:3ff6a09e667f3bcd mx=5fa0 f=*
sqrtsd_two_zero: x2=r:4000000000000000 mx=7f80 => x1=r:3ff6a09e667f3bcc mx=7fa0 f=*
@ sqrtss xmm1, xmm2
  f30f51ca
  SDM 2B SQRTSS; 1 4.8.4 Table 4-9
sqrtss_three_nearest: x2=s:40400000 mx=1f80 => x1=s:3fddb3d7 mx=1fa0 f=*
sqrtss_three_down: x2=s:40400000 mx=3f80 => x1=s:3fddb3d7 mx=3fa0 f=*
sqrtss_three_up: x2=s:40400000 mx=5f80 => x1=s:3fddb3d8 mx=5fa0 f=*
sqrtss_three_zero: x2=s:40400000 mx=7f80 => x1=s:3fddb3d7 mx=7fa0 f=*
@ subsd xmm1, xmm1
  f20f5cc9
  SDM 2B SUBSD; 1 4.8.3.1
subsd_exact_zero_sign_nearest: x1=r:3ff0000000000000 mx=1f80 => x1=r:0000000000000000 f=*
subsd_exact_zero_sign_down: x1=r:3ff0000000000000 mx=3f80 => x1=r:8000000000000000 f=*
subsd_exact_zero_sign_up: x1=r:3ff0000000000000 mx=5f80 => x1=r:0000000000000000 f=*
subsd_exact_zero_sign_zero: x1=r:3ff0000000000000 mx=7f80 => x1=r:0000000000000000 f=*
";

const FP_OVERFLOW_UNDERFLOW: &str = r"
@ mulss xmm1, xmm2
  f30f59ca
  SDM 2B MULSS; 1 4.9.1.4 Table 4-11
mulss_positive_overflow_nearest: x1=s:7f7fffff x2=s:40000000 mx=1f80 => x1=s:7f800000 mx=1fa8 f=*
mulss_positive_overflow_down: x1=s:7f7fffff x2=s:40000000 mx=3f80 => x1=s:7f7fffff mx=3fa8 f=*
mulss_positive_overflow_up: x1=s:7f7fffff x2=s:40000000 mx=5f80 => x1=s:7f800000 mx=5fa8 f=*
mulss_positive_overflow_zero: x1=s:7f7fffff x2=s:40000000 mx=7f80 => x1=s:7f7fffff mx=7fa8 f=*
mulss_negative_overflow_nearest: x1=s:ff7fffff x2=s:40000000 mx=1f80 => x1=s:ff800000 mx=1fa8 f=*
mulss_negative_overflow_down: x1=s:ff7fffff x2=s:40000000 mx=3f80 => x1=s:ff800000 mx=3fa8 f=*
mulss_negative_overflow_up: x1=s:ff7fffff x2=s:40000000 mx=5f80 => x1=s:ff7fffff mx=5fa8 f=*
mulss_negative_overflow_zero: x1=s:ff7fffff x2=s:40000000 mx=7f80 => x1=s:ff7fffff mx=7fa8 f=*
@ addsd xmm1, xmm2
  f20f58ca
  SDM 2A ADDSD; 1 4.9.1.4 Table 4-11
addsd_overflow_nearest: x1=r:7fefffffffffffff x2=r:7fefffffffffffff mx=1f80 => x1=r:7ff0000000000000 mx=1fa8 f=*
addsd_overflow_down: x1=r:7fefffffffffffff x2=r:7fefffffffffffff mx=3f80 => x1=r:7fefffffffffffff mx=3fa8 f=*
addsd_overflow_up: x1=r:7fefffffffffffff x2=r:7fefffffffffffff mx=5f80 => x1=r:7ff0000000000000 mx=5fa8 f=*
addsd_overflow_zero: x1=r:7fefffffffffffff x2=r:7fefffffffffffff mx=7f80 => x1=r:7fefffffffffffff mx=7fa8 f=*
@ addss xmm1, xmm2
  f30f58ca
  SDM 2A ADDSS; 1 4.9.1.4 Table 4-11
addss_rounds_below_overflow_threshold: x1=s:7f7fffff x2=s:72ffffff => x1=s:7f7fffff mx=1fa0 f=*
addss_rounds_at_overflow_threshold: x1=s:7f7fffff x2=s:73000000 => x1=s:7f800000 mx=1fa8 f=*
@ addss xmm1, xmm2; mulss xmm3, xmm4; divss xmm5, xmm6
  f30f58ca f30f59dc f30f5eee
  SDM 1 4.8.3.3
infinity_arithmetic_is_exact: x1=s:7f800000 x2=s:ff7fffff x3=s:ff800000 x4=s:c0000000 x5=s:3f800000 x6=s:ff800000 => x1=s:7f800000 x3=s:7f800000 x5=s:80000000 f=*
@ mulss xmm1, xmm2
  f30f59ca
  SDM 1 4.9.1.5; 1 11.5.2.5
mulss_exact_denormal_sets_no_flags: x1=s:00800000 x2=s:3f000000 => x1=s:00400000 mx=1f80 f=*
mulss_inexact_denormal_nearest: x1=s:00800001 x2=s:3f000000 mx=1f80 => x1=s:00400000 mx=1fb0 f=*
mulss_inexact_denormal_down: x1=s:00800001 x2=s:3f000000 mx=3f80 => x1=s:00400000 mx=3fb0 f=*
mulss_inexact_denormal_up: x1=s:00800001 x2=s:3f000000 mx=5f80 => x1=s:00400001 mx=5fb0 f=*
mulss_inexact_denormal_zero: x1=s:00800001 x2=s:3f000000 mx=7f80 => x1=s:00400000 mx=7fb0 f=*
@ mulss xmm1, xmm2
  f30f59ca
  SDM 1 4.9.1.5; 1 11.5.2.5; 1 4.9.1.2
mulss_min_denormal_halved_nearest: x1=s:00000001 x2=s:3f000000 mx=1f80 => x1=s:00000000 mx=1fb2 f=*
mulss_min_denormal_halved_down: x1=s:00000001 x2=s:3f000000 mx=3f80 => x1=s:00000000 mx=3fb2 f=*
mulss_min_denormal_halved_up: x1=s:00000001 x2=s:3f000000 mx=5f80 => x1=s:00000001 mx=5fb2 f=*
mulss_min_denormal_halved_zero: x1=s:00000001 x2=s:3f000000 mx=7f80 => x1=s:00000000 mx=7fb2 f=*
mulss_negative_min_denormal_halved_nearest: x1=s:80000001 x2=s:3f000000 mx=1f80 => x1=s:80000000 mx=1fb2 f=*
mulss_negative_min_denormal_halved_down: x1=s:80000001 x2=s:3f000000 mx=3f80 => x1=s:80000001 mx=3fb2 f=*
mulss_negative_min_denormal_halved_up: x1=s:80000001 x2=s:3f000000 mx=5f80 => x1=s:80000000 mx=5fb2 f=*
mulss_negative_min_denormal_halved_zero: x1=s:80000001 x2=s:3f000000 mx=7f80 => x1=s:80000000 mx=7fb2 f=*
@ mulss xmm1, xmm2
  f30f59ca
  SDM 1 4.9.1.5; 1 11.5.2.5 Table 4-12 note 1
mulss_tininess_is_after_rounding_not_tiny_nearest: x1=s:20000400 x2=s:1ffff800 mx=1f80 => x1=s:00800000 mx=1fa0 f=*
mulss_tininess_is_after_rounding_not_tiny_down: x1=s:20000400 x2=s:1ffff800 mx=3f80 => x1=s:007fffff mx=3fb0 f=*
mulss_tininess_is_after_rounding_not_tiny_up: x1=s:20000400 x2=s:1ffff800 mx=5f80 => x1=s:00800000 mx=5fa0 f=*
mulss_tininess_is_after_rounding_not_tiny_zero: x1=s:20000400 x2=s:1ffff800 mx=7f80 => x1=s:007fffff mx=7fb0 f=*
mulss_tiny_rounds_up_to_min_normal_nearest: x1=s:00800000 x2=s:3f7fffff mx=1f80 => x1=s:00800000 mx=1fb0 f=*
mulss_tiny_rounds_up_to_min_normal_down: x1=s:00800000 x2=s:3f7fffff mx=3f80 => x1=s:007fffff mx=3fb0 f=*
mulss_tiny_rounds_up_to_min_normal_up: x1=s:00800000 x2=s:3f7fffff mx=5f80 => x1=s:00800000 mx=5fb0 f=*
mulss_tiny_rounds_up_to_min_normal_zero: x1=s:00800000 x2=s:3f7fffff mx=7f80 => x1=s:007fffff mx=7fb0 f=*
@ subsd xmm1, xmm2
  f20f5cca
  SDM 1 4.9.1.5; 1 11.5.2.5
subsd_exact_denormal_result: x1=r:0010000000000001 x2=r:0010000000000000 => x1=r:0000000000000001 mx=1f80 f=*
@ divsd xmm1, xmm2
  f20f5eca
  SDM 1 4.9.1.5; 1 11.5.2.5
divsd_min_normal_by_three: x1=r:0010000000000000 x2=r:4008000000000000 => x1=r:0005555555555555 mx=1fb0 f=*
";

const FP_FTZ_DAZ_DENORMALS: &str = r"
@ mulss xmm1, xmm2
  f30f59ca
  SDM 1 10.2.3.3; 1 11.5.2.5
ftz_flushes_inexact_tiny: x1=s:00800001 x2=s:3f000000 mx=9f80 => x1=s:00000000 mx=9fb0 f=*
ftz_flush_keeps_sign_of_true_result: x1=s:80800001 x2=s:3f000000 mx=9f80 => x1=s:80000000 mx=9fb0 f=*
ftz_round_up_still_flushes_to_zero: x1=s:00800001 x2=s:3f000000 mx=df80 => x1=s:00000000 mx=dfb0 f=*
@ mulss xmm1, xmm2
  f30f59ca
  SDM 1 10.2.3.3; 1 11.5.2.5; 1 11.5.2.2
ftz_does_not_flush_denormal_operand: x1=s:00400000 x2=s:40800000 mx=9f80 => x1=s:01000000 mx=9f82 f=*
@ mulss xmm1, xmm2
  f30f59ca
  SDM 1 10.2.3.3; 1 11.5.2.5
ftz_result_rounded_to_min_normal_is_kept: x1=s:20000400 x2=s:1ffff800 mx=9f80 => x1=s:00800000 mx=9fa0 f=*
ftz_tiny_that_would_round_to_min_normal_is_flushed: x1=s:00800000 x2=s:3f7fffff mx=9f80 => x1=s:00000000 mx=9fb0 f=*
@ movaps xmm1, xmm2; andps xmm1, xmm2
  0f28ca 0f54ca
  SDM 1 10.2.3.3
ftz_ignored_by_moves_and_logic: x2=d:1,80000001,00400000,7f800001 mx=9fc0 => x1=d:1,80000001,00400000,7f800001 f=*
@ mulss xmm1, xmm2
  !f30f59ca
  SDM 1 10.2.3.3; 1 4.9.1.5
ftz_ignored_when_underflow_unmasked: x1=s:00800000 x2=s:3f000000 mx=9780 => mx=9790 f=* ! XM
@ mulsd xmm1, xmm2
  f20f59ca
  SDM 1 10.2.3.3; 1 11.5.2.6; APM 1 4.2.2 (the flush itself is inexact, so UE and PE)
ftz_exact_tiny_result_is_zero: x1=r:8010000000000000 x2=r:3fe0000000000000 mx=9f80 => x1=r:8000000000000000 mx=9fb0 f=*
@ addss xmm1, xmm2
  f30f58ca
  SDM 1 10.2.3.4
daz_add_denormals: x1=s:00000001 x2=s:007fffff mx=1fc0 => x1=s:00000000 f=*
@ mulss xmm1, xmm2
  f30f59ca
  SDM 1 10.2.3.4
daz_keeps_operand_sign: x1=s:80000001 x2=s:3f800000 mx=1fc0 => x1=s:80000000 f=*
@ divss xmm1, xmm2
  f30f5eca
  SDM 1 10.2.3.4; 1 11.5.2.3
daz_divide_by_denormal_is_divide_by_zero: x1=s:3f800000 x2=s:80000001 mx=1fc0 => x1=s:ff800000 mx=1fc4 f=*
@ divss xmm1, xmm2
  f30f5eca
  SDM 1 10.2.3.4; 1 11.5.2.1
daz_denormal_over_denormal_is_invalid: x1=s:00000001 x2=s:00000001 mx=1fc0 => x1=s:ffc00000 mx=1fc1 f=*
@ mulsd xmm1, xmm2
  f20f59ca
  SDM 1 10.2.3.4; 1 11.5.2.1
daz_infinity_times_denormal_is_invalid: x1=r:7ff0000000000000 x2=r:0000000000000001 mx=1fc0 => x1=r:fff8000000000000 mx=1fc1 f=*
@ sqrtss xmm1, xmm2
  f30f51ca
  SDM 1 10.2.3.4; 2B SQRTSS
daz_sqrt_negative_denormal_is_negative_zero: x2=s:80000001 mx=1fc0 => x1=s:80000000 f=*
@ comiss xmm1, xmm2
  0f2fca
  SDM 1 10.2.3.4; 2A COMISS
daz_compare_denormal_equals_zero: x1=s:00000001 x2=s:80000000 mx=1fc0 => f=cpaZso
@ minss xmm1, xmm2
  f30f5dca
  SDM 1 10.2.3.4; 2B MINSS
daz_min_returns_second_operand_as_zero: x1=s:00000001 x2=s:80000001 mx=1fc0 => x1=s:80000000 f=*
@ cvtss2si eax, xmm1
  f30f2dc1
  SDM 1 10.2.3.4
daz_cvtss2si_round_up_is_zero: x1=s:00000001 mx=5fc0 => rax=0 f=*
@ cvtss2sd xmm1, xmm2
  f30f5aca
  SDM 1 10.2.3.4
daz_cvtss2sd_is_zero: x2=s:80000001 mx=1fc0 => x1=r:8000000000000000 f=*
@ mulss xmm1, xmm2
  f30f59ca
  SDM 1 10.2.3.4
daz_does_not_flush_results: x1=s:00800000 x2=s:3f000000 mx=1fc0 => x1=s:00400000 f=*
@ addss xmm1, xmm2
  f30f58ca
  SDM 1 4.9.1.2; 1 11.5.2.2
denormal_operand_sets_de: x1=s:00000001 x2=s:00000001 => x1=s:00000002 mx=1f82 f=*
denormal_add_to_normal_sets_de_pe: x1=s:3f800000 x2=s:00000001 => x1=s:3f800000 mx=1fa2 f=*
denormal_sum_to_min_normal_is_exact: x1=s:007fffff x2=s:00000001 => x1=s:00800000 mx=1f82 f=*
@ cvtss2si eax, xmm1
  f30f2dc1
  SDM 1 11.5.2.2
cvtss2si_denormal_no_de: x1=s:00000001 => rax=0 mx=1fa0 f=*
@ cvtss2si eax, xmm1
  f30f2dc1
  SDM 1 11.5.2.2; 2A CVTSS2SI
cvtss2si_denormal_round_up_is_one: x1=s:00000001 mx=5f80 => rax=1 mx=5fa0 f=*
@ cvtss2sd xmm1, xmm2
  f30f5aca
  SDM 1 4.9.1.2; 1 11.5.2.2
cvtss2sd_denormal_is_exact_and_sets_de: x2=s:00000001 => x1=r:36a0000000000000 mx=1f82 f=*
@ rcpss xmm1, xmm2; rsqrtss xmm3, xmm2
  f30f53ca f30f52da
  SDM 1 11.5.2.2
rcpss_rsqrtss_signal_nothing: x2=s:7f800001 => x1=s:7fc00001 x3=s:7fc00001 mx=1f80 f=*
@ rcpps xmm1, xmm2; rsqrtps xmm3, xmm2
  0f53ca 0f52da
  SDM 2B RCPPS Table (0, inf)
rcpps_special_values: x2=d:00000000,80000000,7f800000,7f800000 => x1=d:7f800000,ff800000,00000000,00000000 x3=d:7f800000,ff800000,00000000,00000000 mx=1f80 f=*
@ rcpss xmm1, xmm2; mulss xmm1, xmm2; subss xmm1, xmm3; andps xmm1, xmm4; comiss xmm1, xmm5
  f30f53ca f30f59ca f30f5ccb 0f54cc 0f2fcd
  SDM 2B RCPSS (
rcpss_relative_error_bound: x2=s:40400000 x3=s:3f800000 x4=d:7fffffff,0,0,0 x5=d:39c01000,0,0,0 => x1=? mx=? f=Cpz
@ divss xmm1, xmm2
  f30f5eca
  SDM 1 D.4.2.3 Table D-14 (DE not asserted; see the undefined list)
denormal_divided_by_zero_sets_ze: x1=s:00000001 x2=s:80000000 => x1=s:ff800000 mx=~2:1f84 f=*
";

const FP_NAN_INVALID_ZERO_DIVIDE: &str = r"
@ addss xmm1, xmm2
  f30f58ca
  SDM 1 4.8.3.5 Table 4-8
addss_qnan_qnan: x1=s:7fc00123 x2=s:ffc00456 => x1=s:7fc00123 mx=1f80 f=*
addss_qnan_snan: x1=s:7fc00123 x2=s:ff800abc => x1=s:7fc00123 mx=1f81 f=*
addss_snan_qnan: x1=s:7f800789 x2=s:ffc00456 => x1=s:7fc00789 mx=1f81 f=*
addss_snan_snan: x1=s:7f800789 x2=s:ff800abc => x1=s:7fc00789 mx=1f81 f=*
addss_qnan_value: x1=s:ffc00456 x2=s:3f800000 => x1=s:ffc00456 mx=1f80 f=*
addss_value_qnan: x1=s:3f800000 x2=s:ffc00456 => x1=s:ffc00456 mx=1f80 f=*
addss_snan_value: x1=s:ff800abc x2=s:3f800000 => x1=s:ffc00abc mx=1f81 f=*
addss_value_snan: x1=s:3f800000 x2=s:7f800789 => x1=s:7fc00789 mx=1f81 f=*
@ subsd xmm1, xmm2
  f20f5cca
  SDM 1 4.8.3.5 Table 4-8
subsd_qnan_snan_returns_first_operand: x1=r:7ff8000000000123 x2=r:fff0000000000456 => x1=r:7ff8000000000123 mx=1f81 f=*
subsd_two_qnans_returns_first_operand: x1=r:fff8000000000123 x2=r:7ff8000000000456 => x1=r:fff8000000000123 f=*
@ mulsd xmm1, xmm2
  f20f59ca
  SDM 1 4.8.3.5 Table 4-8
mulsd_qnan_snan_returns_first_operand: x1=r:7ff8000000000123 x2=r:fff0000000000456 => x1=r:7ff8000000000123 mx=1f81 f=*
mulsd_two_qnans_returns_first_operand: x1=r:fff8000000000123 x2=r:7ff8000000000456 => x1=r:fff8000000000123 f=*
@ divsd xmm1, xmm2
  f20f5eca
  SDM 1 4.8.3.5 Table 4-8
divsd_qnan_snan_returns_first_operand: x1=r:7ff8000000000123 x2=r:fff0000000000456 => x1=r:7ff8000000000123 mx=1f81 f=*
divsd_two_qnans_returns_first_operand: x1=r:fff8000000000123 x2=r:7ff8000000000456 => x1=r:fff8000000000123 f=*
@ sqrtss xmm1, xmm2
  f30f51ca
  SDM 1 4.8.3.5 Table 4-8
sqrtss_snan_is_quieted: x2=s:ff800abc => x1=s:ffc00abc mx=1f81 f=*
@ sqrtsd xmm1, xmm2
  f20f51ca
  SDM 1 4.8.3.5 Table 4-8
sqrtsd_negative_qnan_passes_through: x2=r:fff8000000000abc => x1=r:fff8000000000abc f=*
@ divss xmm1, xmm2
  f30f5eca
  SDM 1 4.9.2
qnan_over_zero_is_not_divide_by_zero: x1=s:7fc00123 x2=s:00000000 => x1=s:7fc00123 f=*
snan_over_zero_is_invalid_only: x1=s:7f800789 x2=s:00000000 => x1=s:7fc00789 mx=1f81 f=*
@ addss xmm1, xmm2
  f30f58ca
  SDM 1 4.9.2 (priority 2 before 4)
qnan_with_denormal_sets_no_de: x1=s:7fc00123 x2=s:00000001 => x1=s:7fc00123 f=*
@ addss xmm1, xmm2
  f30f58ca
  SDM 1 4.9.2 (priority 1 before 4)
snan_with_denormal_sets_only_ie: x1=s:00000001 x2=s:7f800789 => x1=s:7fc00789 mx=1f81 f=*
@ subss xmm1, xmm2
  f30f5cca
  SDM 1 11.5.2.1 Table 11-1; 1 4.8.3.7
inf_minus_inf_is_default_nan: x1=s:7f800000 x2=s:7f800000 => x1=s:ffc00000 mx=1f81 f=*
@ addss xmm1, xmm2
  f30f58ca
  SDM 1 11.5.2.1 Table 11-1; 1 4.8.3.7
inf_plus_neg_inf_is_default_nan: x1=s:7f800000 x2=s:ff800000 => x1=s:ffc00000 mx=1f81 f=*
@ mulss xmm1, xmm2
  f30f59ca
  SDM 1 11.5.2.1 Table 11-1; 1 4.8.3.7
zero_times_inf_is_default_nan: x1=s:80000000 x2=s:7f800000 => x1=s:ffc00000 mx=1f81 f=*
@ divss xmm1, xmm2
  f30f5eca
  SDM 1 11.5.2.1 Table 11-1; 1 4.8.3.7
zero_over_zero_is_default_nan: x1=s:00000000 x2=s:80000000 => x1=s:ffc00000 mx=1f81 f=*
inf_over_inf_is_default_nan: x1=s:ff800000 x2=s:7f800000 => x1=s:ffc00000 mx=1f81 f=*
@ sqrtsd xmm1, xmm2
  f20f51ca
  SDM 1 11.5.2.1 Table 11-1; 1 4.8.3.7
sqrtsd_negative_is_default_nan: x2=r:bff0000000000000 => x1=r:fff8000000000000 mx=1f81 f=*
@ sqrtsd xmm1, xmm2
  f20f51ca
  SDM 2B SQRTSD; 1 11.5.2.1 (except zero)
sqrtsd_negative_zero_is_negative_zero: x2=r:8000000000000000 => x1=r:8000000000000000 f=*
@ divss xmm1, xmm2; divsd xmm3, xmm4
  f30f5eca f20f5edc
  SDM 1 4.9.1.3
divide_by_zero_signs: x1=s:3f800000 x2=s:80000000 x3=r:c000000000000000 x4=r:8000000000000000 => x1=s:ff800000 x3=r:7ff0000000000000 mx=1f84 f=*
@ divss xmm1, xmm2
  f30f5eca
  SDM 1 11.5.2.3 (finite non-zero operand only)
infinity_over_zero_is_exact: x1=s:7f800000 x2=s:80000000 => x1=s:ff800000 f=*
";

const FP_MIN_MAX_COMPARE: &str = r"
@ minss xmm1, xmm2; maxss xmm3, xmm4; minss xmm5, xmm6; maxss xmm7, xmm8
  f30f5dca f30f5fdc f30f5dee f3410f5ff8
  SDM 2B MINSS; 2B MAXSS; 1 11.5.2.1 Table 11-1
min_max_zeros_return_second: x1=s:00000000 x2=s:80000000 x3=s:80000000 x4=s:00000000 x5=s:80000000 x6=s:00000000 x7=s:00000000 x8=s:80000000 => x1=s:80000000 x3=s:00000000 x5=s:00000000 x7=s:80000000 f=*
@ minss xmm1, xmm2
  f30f5dca
  SDM 2B MINSS; 2B MAXSS; 1 11.5.2.1 Table 11-1
minss_qnan_first_returns_second: x1=s:7fc00123 x2=s:3f800000 => x1=s:3f800000 mx=1f81 f=*
@ maxss xmm1, xmm2
  f30f5fca
  SDM 2B MINSS; 2B MAXSS; 1 11.5.2.1 Table 11-1
maxss_qnan_second_returns_second: x1=s:3f800000 x2=s:ffc00456 => x1=s:ffc00456 mx=1f81 f=*
maxss_snan_second_is_not_quieted: x1=s:7fc00123 x2=s:7f800789 => x1=s:7f800789 mx=1f81 f=*
@ minpd xmm1, xmm2; maxpd xmm3, xmm2
  660f5dca 660f5fda
  SDM 2B MINPD; 2B MAXPD; 1 11.5.2.1 Table 11-1
minpd_lanes: x1=q:3ff0000000000000,7ff8000000000001 x2=q:bff0000000000000,4000000000000000 x3=q:3ff0000000000000,7ff0000000000000 => x1=q:bff0000000000000,4000000000000000 x3=q:3ff0000000000000,7ff0000000000000 mx=1f81 f=*
@ comiss xmm1, xmm2
  0f2fca
  SDM 2A COMISS; 2B UCOMISS
comiss_greater: x1=s:40000000 x2=s:3f800000 => f=cpazso
@ ucomiss xmm1, xmm2
  0f2eca
  SDM 2A COMISS; 2B UCOMISS
ucomiss_greater: x1=s:40000000 x2=s:3f800000 => f=cpazso
@ comiss xmm1, xmm2
  0f2fca
  SDM 2A COMISS; 2B UCOMISS
comiss_less: x1=s:3f800000 x2=s:40000000 => f=Cpazso
@ ucomiss xmm1, xmm2
  0f2eca
  SDM 2A COMISS; 2B UCOMISS
ucomiss_less: x1=s:3f800000 x2=s:40000000 => f=Cpazso
@ comiss xmm1, xmm2
  0f2fca
  SDM 2A COMISS; 2B UCOMISS
comiss_equal: x1=s:3f800000 x2=s:3f800000 => f=cpaZso
@ ucomiss xmm1, xmm2
  0f2eca
  SDM 2A COMISS; 2B UCOMISS
ucomiss_equal: x1=s:3f800000 x2=s:3f800000 => f=cpaZso
@ comiss xmm1, xmm2
  0f2fca
  SDM 2A COMISS; 2B UCOMISS
comiss_zeros_equal: x1=s:00000000 x2=s:80000000 => f=cpaZso
@ ucomiss xmm1, xmm2
  0f2eca
  SDM 2A COMISS; 2B UCOMISS
ucomiss_zeros_equal: x1=s:00000000 x2=s:80000000 => f=cpaZso
@ comiss xmm1, xmm2
  0f2fca
  SDM 2A COMISS; 2B UCOMISS
comiss_infinities: x1=s:ff800000 x2=s:7f800000 => f=Cpazso
@ ucomiss xmm1, xmm2
  0f2eca
  SDM 2A COMISS; 2B UCOMISS
ucomiss_infinities: x1=s:ff800000 x2=s:7f800000 => f=Cpazso
@ comiss xmm1, xmm2
  0f2fca
  SDM 2A COMISS; 2B UCOMISS
comiss_qnan_sets_ie: x1=s:7fc00123 x2=s:3f800000 => f=CPaZso mx=1f81
@ ucomiss xmm1, xmm2
  0f2eca
  SDM 2A COMISS; 2B UCOMISS
ucomiss_qnan_sets_nothing: x1=s:7fc00123 x2=s:3f800000 => f=CPaZso
ucomiss_snan_sets_ie: x1=s:3f800000 x2=s:7f800789 => f=CPaZso mx=1f81
@ ucomisd xmm1, xmm2
  660f2eca
  SDM 2A COMISD; 2B UCOMISD; 1 4.9.1.2; 1 11.5.2.2
ucomisd_denormal_sets_de: x1=r:0000000000000001 x2=r:0000000000000000 => f=cpazso mx=1f82
@ comisd xmm1, xmm2
  !660f2fca
  SDM 2A COMISD (EFLAGS not updated)
comisd_qnan_unmasked_invalid_keeps_eflags: x1=r:7ff8000000000001 x2=r:3ff0000000000000 mx=1f00 => mx=1f01 f=* ! XM
@ cmpps xmm1, xmm2, 0
  0fc2ca00
  SDM 2A CMPSS Table 3-1 predicates 0-7; 1 11.5.2.1
cmpps_eq_less_equal_greater_qnan: x1=d:3f800000,80000000,40000000,7fc00123 x2=d:40000000,00000000,3f800000,3f800000 => x1=d:0,ffffffff,0,0 mx=1f80 f=*
@ cmpsd xmm1, xmm2, 0
  f20fc2ca00
  SDM 2A CMPSS Table 3-1 predicates 0-7; 1 11.5.2.1
cmpsd_eq_snan_always_invalid: x1=r:3ff0000000000000 x2=r:7ff0000000000001 => x1=r:0000000000000000 mx=1f81 f=*
@ cmpps xmm1, xmm2, 1
  0fc2ca01
  SDM 2A CMPSS Table 3-1 predicates 0-7; 1 11.5.2.1
cmpps_lt_less_equal_greater_qnan: x1=d:3f800000,80000000,40000000,7fc00123 x2=d:40000000,00000000,3f800000,3f800000 => x1=d:ffffffff,0,0,0 mx=1f81 f=*
@ cmpsd xmm1, xmm2, 1
  f20fc2ca01
  SDM 2A CMPSS Table 3-1 predicates 0-7; 1 11.5.2.1
cmpsd_lt_snan_always_invalid: x1=r:3ff0000000000000 x2=r:7ff0000000000001 => x1=r:0000000000000000 mx=1f81 f=*
@ cmpps xmm1, xmm2, 2
  0fc2ca02
  SDM 2A CMPSS Table 3-1 predicates 0-7; 1 11.5.2.1
cmpps_le_less_equal_greater_qnan: x1=d:3f800000,80000000,40000000,7fc00123 x2=d:40000000,00000000,3f800000,3f800000 => x1=d:ffffffff,ffffffff,0,0 mx=1f81 f=*
@ cmpsd xmm1, xmm2, 2
  f20fc2ca02
  SDM 2A CMPSS Table 3-1 predicates 0-7; 1 11.5.2.1
cmpsd_le_snan_always_invalid: x1=r:3ff0000000000000 x2=r:7ff0000000000001 => x1=r:0000000000000000 mx=1f81 f=*
@ cmpps xmm1, xmm2, 3
  0fc2ca03
  SDM 2A CMPSS Table 3-1 predicates 0-7; 1 11.5.2.1
cmpps_unord_less_equal_greater_qnan: x1=d:3f800000,80000000,40000000,7fc00123 x2=d:40000000,00000000,3f800000,3f800000 => x1=d:0,0,0,ffffffff mx=1f80 f=*
@ cmpsd xmm1, xmm2, 3
  f20fc2ca03
  SDM 2A CMPSS Table 3-1 predicates 0-7; 1 11.5.2.1
cmpsd_unord_snan_always_invalid: x1=r:3ff0000000000000 x2=r:7ff0000000000001 => x1=r:ffffffffffffffff mx=1f81 f=*
@ cmpps xmm1, xmm2, 4
  0fc2ca04
  SDM 2A CMPSS Table 3-1 predicates 0-7; 1 11.5.2.1
cmpps_neq_less_equal_greater_qnan: x1=d:3f800000,80000000,40000000,7fc00123 x2=d:40000000,00000000,3f800000,3f800000 => x1=d:ffffffff,0,ffffffff,ffffffff mx=1f80 f=*
@ cmpsd xmm1, xmm2, 4
  f20fc2ca04
  SDM 2A CMPSS Table 3-1 predicates 0-7; 1 11.5.2.1
cmpsd_neq_snan_always_invalid: x1=r:3ff0000000000000 x2=r:7ff0000000000001 => x1=r:ffffffffffffffff mx=1f81 f=*
@ cmpps xmm1, xmm2, 5
  0fc2ca05
  SDM 2A CMPSS Table 3-1 predicates 0-7; 1 11.5.2.1
cmpps_nlt_less_equal_greater_qnan: x1=d:3f800000,80000000,40000000,7fc00123 x2=d:40000000,00000000,3f800000,3f800000 => x1=d:0,ffffffff,ffffffff,ffffffff mx=1f81 f=*
@ cmpsd xmm1, xmm2, 5
  f20fc2ca05
  SDM 2A CMPSS Table 3-1 predicates 0-7; 1 11.5.2.1
cmpsd_nlt_snan_always_invalid: x1=r:3ff0000000000000 x2=r:7ff0000000000001 => x1=r:ffffffffffffffff mx=1f81 f=*
@ cmpps xmm1, xmm2, 6
  0fc2ca06
  SDM 2A CMPSS Table 3-1 predicates 0-7; 1 11.5.2.1
cmpps_nle_less_equal_greater_qnan: x1=d:3f800000,80000000,40000000,7fc00123 x2=d:40000000,00000000,3f800000,3f800000 => x1=d:0,0,ffffffff,ffffffff mx=1f81 f=*
@ cmpsd xmm1, xmm2, 6
  f20fc2ca06
  SDM 2A CMPSS Table 3-1 predicates 0-7; 1 11.5.2.1
cmpsd_nle_snan_always_invalid: x1=r:3ff0000000000000 x2=r:7ff0000000000001 => x1=r:ffffffffffffffff mx=1f81 f=*
@ cmpps xmm1, xmm2, 7
  0fc2ca07
  SDM 2A CMPSS Table 3-1 predicates 0-7; 1 11.5.2.1
cmpps_ord_less_equal_greater_qnan: x1=d:3f800000,80000000,40000000,7fc00123 x2=d:40000000,00000000,3f800000,3f800000 => x1=d:ffffffff,ffffffff,ffffffff,0 mx=1f80 f=*
@ cmpsd xmm1, xmm2, 7
  f20fc2ca07
  SDM 2A CMPSS Table 3-1 predicates 0-7; 1 11.5.2.1
cmpsd_ord_snan_always_invalid: x1=r:3ff0000000000000 x2=r:7ff0000000000001 => x1=r:0000000000000000 mx=1f81 f=*
";

const FP_CONVERSIONS: &str = r"
@ cvtss2si eax, xmm1
  f30f2dc1
  SDM 2A CVTSS2SI; 2A CVTSD2SI; 1 4.8.4
cvtss2si_3f000000_nearest: x1=s:3f000000 rax=ffffffffffffffff mx=1f80 => rax=0 mx=1fa0 f=*
cvtss2si_3f000000_down: x1=s:3f000000 rax=ffffffffffffffff mx=3f80 => rax=0 mx=3fa0 f=*
cvtss2si_3f000000_up: x1=s:3f000000 rax=ffffffffffffffff mx=5f80 => rax=1 mx=5fa0 f=*
cvtss2si_3f000000_zero: x1=s:3f000000 rax=ffffffffffffffff mx=7f80 => rax=0 mx=7fa0 f=*
cvtss2si_3fc00000_nearest: x1=s:3fc00000 rax=ffffffffffffffff mx=1f80 => rax=2 mx=1fa0 f=*
cvtss2si_3fc00000_down: x1=s:3fc00000 rax=ffffffffffffffff mx=3f80 => rax=1 mx=3fa0 f=*
cvtss2si_3fc00000_up: x1=s:3fc00000 rax=ffffffffffffffff mx=5f80 => rax=2 mx=5fa0 f=*
cvtss2si_3fc00000_zero: x1=s:3fc00000 rax=ffffffffffffffff mx=7f80 => rax=1 mx=7fa0 f=*
cvtss2si_40200000_nearest: x1=s:40200000 rax=ffffffffffffffff mx=1f80 => rax=2 mx=1fa0 f=*
cvtss2si_40200000_down: x1=s:40200000 rax=ffffffffffffffff mx=3f80 => rax=2 mx=3fa0 f=*
cvtss2si_40200000_up: x1=s:40200000 rax=ffffffffffffffff mx=5f80 => rax=3 mx=5fa0 f=*
cvtss2si_40200000_zero: x1=s:40200000 rax=ffffffffffffffff mx=7f80 => rax=2 mx=7fa0 f=*
cvtss2si_bf000000_nearest: x1=s:bf000000 rax=ffffffffffffffff mx=1f80 => rax=0 mx=1fa0 f=*
cvtss2si_bf000000_down: x1=s:bf000000 rax=ffffffffffffffff mx=3f80 => rax=ffffffff mx=3fa0 f=*
cvtss2si_bf000000_up: x1=s:bf000000 rax=ffffffffffffffff mx=5f80 => rax=0 mx=5fa0 f=*
cvtss2si_bf000000_zero: x1=s:bf000000 rax=ffffffffffffffff mx=7f80 => rax=0 mx=7fa0 f=*
cvtss2si_bfc00000_nearest: x1=s:bfc00000 rax=ffffffffffffffff mx=1f80 => rax=fffffffe mx=1fa0 f=*
cvtss2si_bfc00000_down: x1=s:bfc00000 rax=ffffffffffffffff mx=3f80 => rax=fffffffe mx=3fa0 f=*
cvtss2si_bfc00000_up: x1=s:bfc00000 rax=ffffffffffffffff mx=5f80 => rax=ffffffff mx=5fa0 f=*
cvtss2si_bfc00000_zero: x1=s:bfc00000 rax=ffffffffffffffff mx=7f80 => rax=ffffffff mx=7fa0 f=*
@ cvttss2si eax, xmm1; cvttss2si rcx, xmm2
  f30f2cc1 f3480f2cca
  SDM 2A CVTTSS2SI; 1 4.8.4.2
cvttss2si_truncates_regardless_of_rc: x1=s:3fffffff x2=s:bfffffff mx=5f80 => rax=1 rcx=ffffffffffffffff mx=5fa0 f=*
@ cvtss2si eax, xmm1
  f30f2dc1
  SDM 2A CVTSS2SI; 2A CVTSD2SI; 1 4.8.4
cvtss2si_two_pow_31_is_indefinite: x1=s:4f000000 rax=ffffffffffffffff => rax=80000000 mx=1f81 f=*
cvtss2si_minus_two_pow_31_is_exact: x1=s:cf000000 => rax=80000000 f=*
@ cvtsd2si eax, xmm1
  f20f2dc1
  SDM 2A CVTSS2SI; 2A CVTSD2SI; 1 4.8.4
cvtsd2si_rounds_out_of_range: x1=r:41dfffffffe00000 => rax=80000000 mx=1f81 f=*
@ cvttsd2si eax, xmm1
  f20f2cc1
  SDM 2A CVTTSD2SI
cvttsd2si_same_value_is_in_range: x1=r:41dfffffffe00000 => rax=7fffffff mx=1fa0 f=*
@ cvtsd2si eax, xmm1
  f20f2dc1
  SDM 2A CVTSS2SI; 2A CVTSD2SI; 1 4.8.4
cvtsd2si_negative_tie_in_range: x1=r:c1e0000000100000 => rax=80000000 mx=1fa0 f=*
cvtsd2si_negative_round_down_out_of_range: x1=r:c1e0000000100000 mx=3f80 => rax=80000000 mx=3f81 f=*
@ cvttsd2si eax, xmm1
  f20f2cc1
  SDM 2A CVTTSD2SI
cvttsd2si_negative_fraction_in_range: x1=r:c1e00000001fffff => rax=80000000 mx=1fa0 f=*
@ cvttsd2si eax, xmm1; cvttsd2si rcx, xmm1
  f20f2cc1 f2480f2cc9
  SDM 2A CVTTSD2SI
cvttsd2si_minus_two_pow_31_minus_one: x1=r:c1e0000000200000 => rax=80000000 rcx=ffffffff7fffffff mx=1f81 f=*
@ cvtsd2si rax, xmm1
  f2480f2dc1
  SDM 2A CVTSS2SI; 2A CVTSD2SI; 1 4.8.4
cvtsd2si64_two_pow_63_is_indefinite: x1=r:43e0000000000000 => rax=8000000000000000 mx=1f81 f=*
@ cvttsd2si rax, xmm1
  f2480f2cc1
  SDM 2A CVTTSD2SI
cvttsd2si64_largest_below_two_pow_63: x1=r:43dfffffffffffff => rax=7ffffffffffffc00 f=*
@ cvtsd2si rax, xmm1
  f2480f2dc1
  SDM 2A CVTSS2SI; 2A CVTSD2SI; 1 4.8.4
cvtsd2si64_minus_two_pow_63_exact: x1=r:c3e0000000000000 => rax=8000000000000000 f=*
@ cvttss2si rax, xmm1
  f3480f2cc1
  SDM 1 11.5.2.1 Table 11-1
cvttss2si_qnan_is_indefinite: x1=s:7fc00123 => rax=8000000000000000 mx=1f81 f=*
@ cvtsd2si eax, xmm1
  f20f2dc1
  SDM 1 11.5.2.1 Table 11-1
cvtsd2si_infinity_is_indefinite: x1=r:7ff0000000000000 rax=ffffffffffffffff => rax=80000000 mx=1f81 f=*
@ cvtsi2ss xmm1, eax
  f30f2ac8
  SDM 2A CVTSI2SS; 1 4.8.4 Table 4-9
cvtsi2ss_16777217_nearest: rax=1000001 mx=1f80 => x1=s:4b800000 mx=1fa0 f=*
cvtsi2ss_16777217_down: rax=1000001 mx=3f80 => x1=s:4b800000 mx=3fa0 f=*
cvtsi2ss_16777217_up: rax=1000001 mx=5f80 => x1=s:4b800001 mx=5fa0 f=*
cvtsi2ss_16777217_zero: rax=1000001 mx=7f80 => x1=s:4b800000 mx=7fa0 f=*
cvtsi2ss_minus_16777219_nearest: rax=12345678fefffffd mx=1f80 => x1=s:cb800002 mx=1fa0 f=*
cvtsi2ss_minus_16777219_down: rax=12345678fefffffd mx=3f80 => x1=s:cb800002 mx=3fa0 f=*
cvtsi2ss_minus_16777219_up: rax=12345678fefffffd mx=5f80 => x1=s:cb800001 mx=5fa0 f=*
cvtsi2ss_minus_16777219_zero: rax=12345678fefffffd mx=7f80 => x1=s:cb800001 mx=7fa0 f=*
@ cvtsi2sd xmm1, rax
  f2480f2ac8
  SDM 2A CVTSI2SD; 1 4.8.4 Table 4-9
cvtsi2sd64_two_pow_53_plus_one_nearest: rax=20000000000001 mx=1f80 => x1=r:4340000000000000 mx=1fa0 f=*
cvtsi2sd64_two_pow_53_plus_one_down: rax=20000000000001 mx=3f80 => x1=r:4340000000000000 mx=3fa0 f=*
cvtsi2sd64_two_pow_53_plus_one_up: rax=20000000000001 mx=5f80 => x1=r:4340000000000001 mx=5fa0 f=*
cvtsi2sd64_two_pow_53_plus_one_zero: rax=20000000000001 mx=7f80 => x1=r:4340000000000000 mx=7fa0 f=*
@ cvtsi2ss xmm1, rax
  f3480f2ac8
  SDM 2A CVTSI2SS; 1 4.8.4 Table 4-9
cvtsi2ss64_int64_max_nearest: rax=7fffffffffffffff mx=1f80 => x1=s:5f000000 mx=1fa0 f=*
cvtsi2ss64_int64_max_down: rax=7fffffffffffffff mx=3f80 => x1=s:5effffff mx=3fa0 f=*
cvtsi2ss64_int64_max_up: rax=7fffffffffffffff mx=5f80 => x1=s:5f000000 mx=5fa0 f=*
cvtsi2ss64_int64_max_zero: rax=7fffffffffffffff mx=7f80 => x1=s:5effffff mx=7fa0 f=*
@ cvtsi2sd xmm1, eax
  f20f2ac8
  SDM 2A CVTSI2SD
cvtsi2sd32_min_is_exact: rax=80000000 => x1=r:c1e0000000000000 f=*
@ cvtsi2sd xmm1, rax
  f2480f2ac8
  SDM 2A CVTSI2SD
cvtsi2sd_zero_is_positive_zero_in_round_down: rax=0 mx=3f80 => x1=r:0000000000000000 f=*
@ cvtsd2ss xmm1, xmm2
  f20f5aca
  SDM 2A CVTSD2SS; 1 4.8.4 Table 4-9
cvtsd2ss_pi_nearest: x2=r:400921fb54442d18 mx=1f80 => x1=s:40490fdb mx=1fa0 f=*
cvtsd2ss_pi_down: x2=r:400921fb54442d18 mx=3f80 => x1=s:40490fda mx=3fa0 f=*
cvtsd2ss_pi_up: x2=r:400921fb54442d18 mx=5f80 => x1=s:40490fdb mx=5fa0 f=*
cvtsd2ss_pi_zero: x2=r:400921fb54442d18 mx=7f80 => x1=s:40490fda mx=7fa0 f=*
@ cvtsd2ss xmm1, xmm2
  f20f5aca
  SDM 2A CVTSD2SS; 1 4.9.1.4 Table 4-11
cvtsd2ss_overflow: x2=r:c7f0000000000000 => x1=s:ff800000 mx=1fa8 f=*
@ cvtsd2ss xmm1, xmm2
  f20f5aca
  SDM 1 4.9.1.5; 1 11.5.2.5
cvtsd2ss_exact_denormal: x2=r:3800000000000000 => x1=s:00400000 mx=1f80 f=*
@ cvtsd2ss xmm1, xmm2
  f20f5aca
  SDM 1 10.2.3.3; 1 11.5.2.5
cvtsd2ss_ftz_tiny: x2=r:b690000000000001 mx=9f80 => x1=s:80000000 mx=9fb0 f=*
@ cvtsd2ss xmm1, xmm2
  f20f5aca
  SDM 1 4.9.1.2; 1 11.5.2.2; 1 4.9.1.5; 1 11.5.2.5
cvtsd2ss_double_denormal_sets_de: x2=r:0000000000000001 => x1=s:00000000 mx=1fb2 f=*
@ cvtsd2ss xmm1, xmm2
  f20f5aca
  SDM 1 D.4.2.2 Table D-12
cvtsd2ss_snan_is_quieted: x2=r:7ff0000020000000 => x1=s:7fc00001 mx=1f81 f=*
@ cvtss2sd xmm1, xmm2
  f30f5aca
  SDM 1 D.4.2.2 Table D-11
cvtss2sd_snan_is_quieted: x2=s:ff800001 => x1=r:fff8000020000000 mx=1f81 f=*
@ cvtpd2ps xmm1, xmm2
  660f5aca
  SDM 2A CVTPD2PS
cvtpd2ps_zeroes_upper_half: x2=q:3ff0000000000000,c000000000000000 => x1=d:3f800000,c0000000,0,0 f=*
@ cvtps2pd xmm1, xmm2
  0f5aca
  SDM 2A CVTPS2PD
cvtps2pd_uses_low_pair: x2=d:3f800000,c0000000,7f800001,00000001 => x1=q:3ff0000000000000,c000000000000000 mx=1f80 f=*
@ cvtdq2ps xmm1, xmm2
  0f5bca
  SDM 2A CVTDQ2PS
cvtdq2ps_rounds: x2=d:1000001,80000000,7fffffff,ffffffff => x1=d:4b800000,cf000000,4f000000,bf800000 mx=1fa0 f=*
@ cvtdq2pd xmm1, xmm2
  f30fe6ca
  SDM 2A CVTDQ2PD
cvtdq2pd_is_exact: x2=d:80000000,7fffffff,1,2 => x1=q:c1e0000000000000,41dfffffffc00000 f=*
@ cvtps2dq xmm1, xmm2
  660f5bca
  SDM 2A CVTPS2DQ
cvtps2dq_mixed: x2=d:40200000,4f000000,7fc00123,bfc00000 => x1=d:2,80000000,80000000,fffffffe mx=1fa1 f=*
@ cvttps2dq xmm1, xmm2
  f30f5bca
  SDM 2A CVTTPS2DQ
cvttps2dq_mixed: x2=d:bfc00000,cf000000,ff800000,4effffff => x1=d:ffffffff,80000000,80000000,7fffff80 mx=1fa1 f=*
@ cvtpd2dq xmm1, xmm2
  f20fe6ca
  SDM 2A CVTPD2DQ
cvtpd2dq_zeroes_upper_half: x2=q:4004000000000000,c004000000000000 => x1=d:2,fffffffe,0,0 mx=1fa0 f=*
@ cvttpd2dq xmm1, xmm2
  660fe6ca
  SDM 2A CVTTPD2DQ
cvttpd2dq_zeroes_upper_half: x2=q:400fffffffffffff,c1e0000000200000 => x1=d:3,80000000,0,0 mx=1fa1 f=*
@ cvtsd2ss xmm1, xmm2
  f20f5aca
  SDM 1 D.4.2.2 Table D-12
cvtsd2ss_qnan_payload_is_truncated: x2=r:fffc0000ffffffff => x1=s:ffe00007 f=*
@ cvtss2sd xmm1, xmm2
  f30f5aca
  SDM 1 D.4.2.2 Table D-11
cvtss2sd_qnan_payload_is_extended: x2=s:7fc00001 => x1=r:7ff8000020000000 f=*
";

const FP_PACKED_AND_STICKY: &str = r"
@ mulps xmm1, xmm2
  0f59ca
  SDM 1 11.5.3.1 Figure 11-9
mulps_ors_lane_flags: x1=d:00400000,3f800000,7f7fffff,00800001 x2=d:40800000,40000000,40000000,3f000000 => x1=d:01000000,40000000,7f800000,00400000 mx=1fba f=*
@ addps xmm1, xmm2
  0f58ca
  SDM 2A ADDPS
addps_lanes_are_independent: x1=d:3f800000,bf800000,7f800000,00000000 x2=d:3f800000,3f800000,3f800000,80000000 => x1=d:40000000,00000000,7f800000,00000000 f=*
@ divpd xmm1, xmm2
  660f5eca
  SDM 2A DIVPD
divpd_mixed_flags: x1=q:3ff0000000000000,0 x2=q:0,0 => x1=q:7ff0000000000000,fff8000000000000 mx=1f85 f=*
@ sqrtps xmm1, xmm2
  0f51ca
  SDM 2B SQRTPS
sqrtps_lanes: x2=d:40800000,80000000,bf800000,7f800000 => x1=d:40000000,80000000,ffc00000,7f800000 mx=1f81 f=*
@ divss xmm1, xmm2; addss xmm3, xmm4
  f30f5eca f30f58dc
  SDM 1 10.2.3.1
exception_flags_are_sticky: x1=s:3f800000 x2=s:00000000 x3=s:3f800000 x4=s:3f800000 mx=1fa0 => x1=s:7f800000 x3=s:40000000 mx=1fa4 f=*
@ addsd xmm1, xmm2; mulss xmm3, xmm4
  f20f58ca f30f59dc
  SDM 2A ADDSD; 2B MULSS
scalar_ops_preserve_upper_lanes: x1=q:3ff0000000000000,7ff0000000000001 x2=q:3ff0000000000000,7ff0000000000001 x3=d:40000000,7f800001,00000001,ffffffff x4=d:40000000,7f800001,00000001,ffffffff => x1=q:4000000000000000,7ff0000000000001 x3=d:40800000,7f800001,00000001,ffffffff mx=1f80 f=*
";

const FP_MXCSR_AND_UNMASKED: &str = r"
@ ldmxcsr [rsi]; stmxcsr [rdi]
  0fae16 0fae1f
  SDM 2A LDMXCSR; 2B STMXCSR
stmxcsr_after_ldmxcsr: rsi=D rdi=D+8 m0=ffff0000 m8=00000000 => m8=ffff0000 mx=ffff f=*
@ ldmxcsr [rsi]
  !0fae16
  SDM 2A LDMXCSR; 1 10.2.3
ldmxcsr_reserved_bit_is_gp: rsi=D m0=801f0100 => ! GP
@ ldmxcsr [rsi]; addss xmm1, xmm2
  0fae16 f30f58ca
  SDM 1 10.2.3.1; 2A LDMXCSR
ldmxcsr_unmasked_flag_does_not_trap: rsi=D m0=01000000 x1=s:3f800000 x2=s:3f800000 => x1=s:40000000 mx=1 f=*
@ divss xmm1, xmm2; ldmxcsr [rsi]; divss xmm3, xmm2
  f30f5eca 0fae16 f30f5eda
  SDM 2A LDMXCSR; 1 4.8.4 Table 4-9
ldmxcsr_changes_following_rounding: rsi=D m0=803f0000 x1=s:3f800000 x2=s:40400000 x3=s:3f800000 => x1=s:3eaaaaab x3=s:3eaaaaaa mx=3fa0 f=*
@ divss xmm1, xmm2
  !f30f5eca
  SDM 1 11.5.3.2; 3A 7.15 Interrupt 19
unmasked_invalid_faults_before_write: x1=s:00000000 x2=s:00000000 mx=1f00 => mx=1f01 f=* ! XM
unmasked_divide_by_zero_faults_before_write: x1=s:3f800000 x2=s:00000000 mx=1d80 => mx=1d84 f=* ! XM
@ addss xmm1, xmm2
  !f30f58ca
  SDM 1 11.5.3.2; 3A 7.15 Interrupt 19
unmasked_denormal_faults_before_write: x1=s:00000001 x2=s:3f800000 mx=1e80 => mx=1e82 f=* ! XM
@ divss xmm1, xmm2
  !f30f5eca
  SDM 1 11.5.3.2; 3A 7.15 Interrupt 19
unmasked_precision_faults_before_write: x1=s:3f800000 x2=s:40400000 mx=f80 => mx=fa0 f=* ! XM
@ mulss xmm1, xmm2
  !f30f59ca
  SDM 1 4.9.1.6; 1 D.4.2.3 Tables D-16 and D-17; APM 1 4.10.2.8
unmasked_underflow_exact_tiny_faults_before_write: x1=s:00800000 x2=s:3f000000 mx=1780 => mx=1790 f=* ! XM
unmasked_overflow_faults_before_write: x1=s:7f7fffff x2=s:7f7fffff mx=1b80 => mx=1ba8 f=* ! XM
masked_overflow_with_precision_unmasked_faults: x1=s:7f7fffff x2=s:40000000 mx=f80 => mx=fa8 f=* ! XM
unmasked_underflow_exact_until_denormalized_sets_ue: x1=s:00800001 x2=s:3f000000 mx=1780 => mx=~20:1790 f=* ! XM
unmasked_underflow_inexact_sets_ue_and_pe: x1=s:00800001 x2=s:3f000001 mx=1780 => mx=17b0 f=* ! XM
@ mov eax, 7; addss xmm3, xmm3; divss xmm1, xmm2; mov ecx, 9
  b807000000 f30f58db !f30f5eca b909000000
  SDM 1 11.5.3.2; 3A 7.15 Interrupt 19
unmasked_fault_is_precise: x1=s:3f800000 x2=s:00000000 x3=s:3f800000 mx=1d80 => rax=7 x3=s:40000000 mx=1d84 f=* ! XM
@ divps xmm1, xmm2
  !0f5eca
  SDM 1 11.5.3.2; 3A 7.15 Interrupt 19
unmasked_packed_lane_leaves_whole_destination: x1=d:3f800000,3f800000,40000000,40800000 x2=d:40000000,00000000,40000000,40000000 mx=1d80 => mx=1d84 f=* ! XM
@ addss xmm1, xmm2
  f30f58ca
  SDM 1 11.5.2.1
masked_qnan_with_invalid_unmasked_does_not_fault: x1=s:7fc00123 x2=s:3f800000 mx=1f00 => x1=s:7fc00123 f=*
@ addss xmm1, xmm2
  !f30f58ca
  SDM 1 11.5.3.2 (steps 1-3)
unmasked_pre_exception_hides_post_exceptions: x1=s:3f800000 x2=s:00000001 mx=0e80 => mx=0e82 f=* ! XM
";

const FP_BEYOND_SSE2: &str = r"
@ roundss xmm1, xmm5, 0; roundss xmm2, xmm5, 1; roundss xmm3, xmm5, 2; roundss xmm4, xmm5, 3
  660f3a0acd00 660f3a0ad501 660f3a0add02 660f3a0ae503
  SDM 2B ROUNDSS
roundss_imm_modes: x5=s:c0200000 x1=d:0,0,0,0 x2=d:0,0,0,0 x3=d:0,0,0,0 x4=d:0,0,0,0 => x1=s:c0000000 x2=s:c0400000 x3=s:c0000000 x4=s:c0000000 mx=1fa0 f=*
@ roundsd xmm1, xmm2, 8
  660f3a0bca08
  SDM 2B ROUNDSD (imm8[3])
roundsd_suppress_precision: x2=r:bfd0000000000000 => x1=r:8000000000000000 mx=1f80 f=*
@ roundps xmm1, xmm2, 4
  660f3a08ca04
  SDM 2B ROUNDPS (imm8[2])
roundps_uses_mxcsr_rc: x2=d:3f000000,bf000000,3fc00000,c0200000 mx=5f80 => x1=d:3f800000,80000000,40000000,c0000000 mx=5fa0 f=*
@ haddps xmm1, xmm2; hsubpd xmm3, xmm4; addsubps xmm5, xmm6
  f20f7cca 660f7ddc f20fd0ee
  SDM 2A HADDPS; 2A HSUBPD; 2A ADDSUBPS
haddps_hsubpd_addsubps: x1=d:3f800000,40000000,40400000,40800000 x2=d:40a00000,40c00000,40e00000,41000000 x3=q:4000000000000000,3ff0000000000000 x4=q:4008000000000000,4010000000000000 x5=d:3f800000,3f800000,3f800000,3f800000 x6=d:3f800000,3f800000,40000000,40000000 => x1=d:40400000,40e00000,41300000,41700000 x3=q:3ff0000000000000,bff0000000000000 x5=d:00000000,40000000,bf800000,40400000 f=*
@ dpps xmm1, xmm2, 0x7a
  660f3a40ca7a
  SDM 2A DPPS
dpps_mask: x1=d:3f800000,40000000,40400000,40800000 x2=d:3f800000,3f800000,3f800000,3f800000 => x1=d:0,40c00000,0,40c00000 f=*
@ movddup xmm1, xmm2; movsldup xmm3, xmm2; movshdup xmm4, xmm2
  f20f12ca f30f12da f30f16e2
  SDM 2B MOVDDUP; 2B MOVSLDUP; 2B MOVSHDUP
movddup_movsldup_movshdup: x2=d:7f800001,2,3,4 => x1=d:7f800001,2,7f800001,2 x3=d:7f800001,7f800001,3,3 x4=d:2,2,4,4 f=*
";

const INSTRUCTION_LENGTH: &str = r"
@ .byte 0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x01,0xd8
  !666666666666666666666666666601d8
  SDM 3A 7.15 Interrupt 13 (instruction length over 15 bytes)
sixteen_byte_redundant_operand_size_prefixes: => ! GP
@ .byte 0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x05,0x34,0x12
  666666666666666666666666053412
  SDM 2A 2.2.1 (15-byte limit); 2A ADD
fifteen_bytes_ending_in_the_immediate_execute: rax=ffffffffffff0001 => rax=ffffffffffff1235 f=cPazso
@ .byte 0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x05,0x34,0x12
  !66666666666666666666666666053412
  SDM 3A 7.15 Interrupt 13 (instruction length over 15 bytes)
sixteenth_byte_in_the_immediate_is_gp: rax=1 => ! GP
@ mov eax, 1; .byte 0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x66,0x05,0x34,0x12
  b801000000 !66666666666666666666666666053412
  SDM 3A 7.15 Interrupt 13 (instruction length over 15 bytes)
overlong_after_an_instruction_retires_it_first: rax=ffffffffffffffff => rax=1 ! GP
";

const PAGE_CROSSING: &str = r"
@ movups xmm0, [rsi-8]; movups [rdi-5], xmm0
  0f1046f8 0f1147fb
  SDM 2B MOVUPS; 1 4.1.1 (unaligned operands)
movups_crosses_contiguous_pages: rsi=D+1000 rdi=D+1000 mff8=000102030405060708090a0b0c0d0e0f => x0=b:00,01,02,03,04,05,06,07,08,09,0a,0b,0c,0d,0e,0f mffb=000102030405060708090a0b0c0d0e0f f=*
@ add qword ptr [rdi-3], 1
  488347fd01
  SDM 2A ADD; 1 4.1.1 (unaligned operands)
add_memory_crosses_contiguous_pages: rdi=D+1000 mffd=ffffffffffffffff => mffd=0000000000000000 f=CPAZso
@ push rax; pop rbx
  50 5b
  SDM 2B PUSH; 2B POP
push_pop_cross_contiguous_pages: rsp=D+1004 rax=1122334455667788 => rbx=1122334455667788 mffc=8877665544332211 f=*
";

const EXACT_UNMASKED_OVERFLOW: &str = r"
@ mulss xmm1, xmm2
  !f30f59ca
  SDM 1 D.4.2.3 Table D-16 (PE only if inexact); APM 1 4.10.2.8
unmasked_overflow_exact_sets_only_oe: x1=s:7f7fffff x2=s:40000000 mx=1b80 => mx=1b88 f=* ! XM
unmasked_negative_overflow_exact_sets_only_oe: x1=s:ff7fffff x2=s:40000000 mx=1b80 => mx=1b88 f=* ! XM
unmasked_overflow_exact_sets_only_oe_down: x1=s:7f7fffff x2=s:40000000 mx=3b80 => mx=3b88 f=* ! XM
unmasked_overflow_exact_sets_only_oe_up: x1=s:7f7fffff x2=s:40000000 mx=5b80 => mx=5b88 f=* ! XM
unmasked_overflow_exact_sets_only_oe_zero: x1=s:7f7fffff x2=s:40000000 mx=7b80 => mx=7b88 f=* ! XM
@ addsd xmm1, xmm2
  !f20f58ca
  SDM 1 D.4.2.3 Table D-16 (PE only if inexact); APM 1 4.10.2.8
unmasked_overflow_exact_addsd: x1=r:7fefffffffffffff x2=r:7fefffffffffffff mx=1b80 => mx=1b88 f=* ! XM
@ divss xmm1, xmm2
  !f30f5eca
  SDM 1 D.4.2.3 Table D-16 (PE only if inexact); APM 1 4.10.2.8
unmasked_overflow_exact_divss: x1=s:7f7fffff x2=s:3f000000 mx=1b80 => mx=1b88 f=* ! XM
@ cvtsd2ss xmm1, xmm2
  !f20f5aca
  SDM 1 D.4.2.3 Table D-16 (PE only if inexact); APM 1 4.10.2.8
unmasked_overflow_exact_cvtsd2ss: x2=r:c7f0000000000000 mx=1b80 => mx=1b88 f=* ! XM
";

const GROUPS: &[(&str, &str)] = &[
    ("add_sub_flags", ADD_SUB_FLAGS),
    ("carry_chains", CARRY_CHAINS),
    ("logic", LOGIC),
    ("partial_registers", PARTIAL_REGISTERS),
    ("exchange", EXCHANGE),
    ("flag_moves", FLAG_MOVES),
    ("shift_rotate", SHIFT_ROTATE),
    ("multiply_divide", MULTIPLY_DIVIDE),
    ("bit_operations", BIT_OPERATIONS),
    ("conditions", CONDITIONS),
    ("stack_and_calls", STACK_AND_CALLS),
    ("strings", STRINGS),
    ("fs_gs", FS_GS),
    ("mov_moffs", MOV_MOFFS),
    ("decoding", DECODING),
    ("fences", FENCES),
    ("x87_init", X87_INIT),
    ("memory_faults", MEMORY_FAULTS),
    ("straddling_fault_address", STRADDLING_FAULT_ADDRESS),
    ("noncanonical_address", NONCANONICAL_ADDRESS),
    ("packed_integer_sse2", PACKED_INTEGER_SSE2),
    ("packed_integer_beyond_sse2", PACKED_INTEGER_BEYOND_SSE2),
    ("fp_rounding", FP_ROUNDING),
    ("fp_overflow_underflow", FP_OVERFLOW_UNDERFLOW),
    ("fp_ftz_daz_denormals", FP_FTZ_DAZ_DENORMALS),
    ("fp_nan_invalid_zero_divide", FP_NAN_INVALID_ZERO_DIVIDE),
    ("fp_min_max_compare", FP_MIN_MAX_COMPARE),
    ("fp_conversions", FP_CONVERSIONS),
    ("fp_packed_and_sticky", FP_PACKED_AND_STICKY),
    ("fp_mxcsr_and_unmasked", FP_MXCSR_AND_UNMASKED),
    ("fp_beyond_sse2", FP_BEYOND_SSE2),
    ("instruction_length", INSTRUCTION_LENGTH),
    ("exact_unmasked_overflow", EXACT_UNMASKED_OVERFLOW),
    ("page_crossing", PAGE_CROSSING),
];

/// Forms both engines refuse today, with the index of the refused instruction.
/// They are not verified, only required to be refused without changing state.
const UNSUPPORTED: &[(&str, usize)] = &[
    ("addr32_load_truncates_effective_address", 0),
    ("addr32_sum_wraps_at_32_bits", 0),
    ("bt_memory_negative_offset", 0),
    ("bt_memory_positive_offset_beyond_operand", 0),
    ("bt_memory_register_offset_can_fault", 0),
    ("btr16_memory_negative_16bit_offset", 0),
    ("bts_memory_negative_offset", 0),
    ("cmpsb_is_rsi_minus_rdi", 1),
    ("cmpxchg16b_equal", 0),
    ("cmpxchg16b_equal_without_lock", 0),
    ("cmpxchg16b_not_equal", 0),
    ("cmpxchg16b_unaligned_is_gp", 0),
    ("cmpxchg16b_unaligned_without_lock_is_gp", 0),
    ("cmpxchg8b_equal", 0),
    ("cmpxchg8b_equal_without_lock", 0),
    ("cmpxchg8b_not_equal", 0),
    ("crc32c_byte_and_qword_forms_agree", 0),
    ("crc32c_check_string", 0),
    ("enter_leave_round_trip", 0),
    ("fifteen_byte_instruction_executes", 0),
    ("fs_addr32_truncates_before_adding_base", 0),
    ("haddps_hsubpd_addsubps", 0),
    ("jecxz_tests_low_32_bits", 1),
    ("lea64_addr32_truncates_effective_address", 0),
    ("lodsd_zero_extends", 1),
    ("lodsd_zero_extends_df_clear", 0),
    ("lodsw_preserves_upper48", 1),
    ("maskmovdqu_selects_bytes", 0),
    ("movddup_movsldup_movshdup", 0),
    ("pcmpeqq_pcmpgtq", 1),
    ("push16_moves_rsp_by_two", 0),
    ("pushfq_popfq_status_round_trip", 0),
    ("rcpps_special_values", 0),
    ("rcpss_relative_error_bound", 0),
    ("rcpss_rsqrtss_signal_nothing", 0),
    ("rep_lodsb_source_fault", 1),
    ("rep_movsb_addr32_uses_32bit_registers", 1),
    ("repe_cmpsb_fault_restores_flags", 1),
    ("repe_cmpsb_fault_restores_flags_df_clear", 0),
    ("repe_cmpsb_stops_at_difference", 1),
    ("repe_cmpsb_stops_at_difference_df_clear", 0),
    ("repe_cmpsb_zero_count_keeps_flags", 1),
    ("repne_scasb_finds_byte", 1),
    ("repne_scasb_finds_byte_df_clear", 0),
    ("repne_scasd_exhausts_count", 1),
];

// ---------------------------------------------------------------------------
// Table parsing
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Case {
    name: &'static str,
    asm: &'static str,
    cite: &'static str,
    code: Vec<u8>,
    /// Offset of each instruction in `code`.
    offsets: Vec<usize>,
    faulting: Option<usize>,
    input: &'static str,
    output: &'static str,
    fault: &'static str,
}

fn hex_bytes(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0, "odd hex {text}");
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex byte"))
        .collect()
}

/// Sum of hexadecimal numbers and symbols, such as `D+10` or `U-FSB`.
fn value(text: &str) -> u64 {
    let mut total = 0_u64;
    let mut rest = text;
    let mut negative = false;
    loop {
        let end = rest.find(['+', '-']).unwrap_or(rest.len());
        let term = match &rest[..end] {
            "D" => DATA,
            "U" => UNMAPPED,
            "R" => READ_ONLY,
            "S" => STACK,
            "C" => CODE,
            "FSB" => FS_BASE,
            "GSB" => GS_BASE,
            number => u64::from_str_radix(number, 16)
                .unwrap_or_else(|_| panic!("bad number {number} in {text}")),
        };
        total = if negative {
            total.wrapping_sub(term)
        } else {
            total.wrapping_add(term)
        };
        if end == rest.len() {
            return total;
        }
        negative = rest.as_bytes()[end] == b'-';
        rest = &rest[end + 1..];
    }
}

/// XMM value described by `text`, taking unnamed bits from `base`.
fn xmm_value(text: &str, base: u128) -> u128 {
    let (kind, lanes) = text.split_once(':').expect("xmm lane kind");
    let (bits, partial) = match kind {
        "b" => (8, false),
        "w" => (16, false),
        "d" => (32, false),
        "q" => (64, false),
        "s" => (32, true),
        "r" => (64, true),
        _ => panic!("xmm lane kind {kind}"),
    };
    let lanes: Vec<u128> = lanes
        .split(',')
        .map(|lane| u128::from_str_radix(lane, 16).expect("xmm lane"))
        .collect();
    if partial {
        assert_eq!(lanes.len(), 1, "{text}");
        let mask = (1_u128 << bits) - 1;
        return (base & !mask) | lanes[0];
    }
    assert_eq!(lanes.len() * bits, 128, "{text}");
    lanes
        .iter()
        .rev()
        .fold(0, |whole, lane| (whole << bits) | lane)
}

fn flag_bits(text: &str, upper: bool) -> u8 {
    text.chars()
        .filter(|letter| letter.is_ascii_uppercase() == upper)
        .fold(0, |bits, letter| {
            bits | match letter.to_ascii_uppercase() {
                'C' => CF,
                'P' => PF,
                'A' => AF,
                'Z' => ZF,
                'S' => SF,
                'O' => OF,
                _ => panic!("flag {letter}"),
            }
        })
}

fn parse(table: &'static str) -> Vec<Case> {
    let mut cases = Vec::new();
    let mut lines = table.lines().filter(|line| !line.is_empty());
    let (mut asm, mut cite) = ("", "");
    let (mut code, mut offsets, mut faulting) = (Vec::new(), Vec::new(), None);
    while let Some(line) = lines.next() {
        if let Some(text) = line.strip_prefix("@ ") {
            asm = text;
            (code, offsets, faulting) = (Vec::new(), Vec::new(), None);
            for (index, piece) in lines.next().expect("bytes").split_whitespace().enumerate() {
                let bytes = piece.strip_prefix('!').unwrap_or(piece);
                if bytes != piece {
                    faulting = Some(index);
                }
                offsets.push(code.len());
                code.extend(hex_bytes(bytes));
            }
            cite = lines.next().expect("citation").trim();
            assert!(cite.starts_with("SDM "), "{cite}");
            continue;
        }
        let (name, states) = line.split_once(':').expect("case name");
        let (input, rest) = states.split_once("=>").expect("case arrow");
        let (output, fault) = rest.split_once('!').unwrap_or((rest, ""));
        cases.push(Case {
            name,
            asm,
            cite,
            code: code.clone(),
            offsets: offsets.clone(),
            faulting,
            input: input.trim(),
            output: output.trim(),
            fault: fault.trim(),
        });
    }
    cases
}

// ---------------------------------------------------------------------------
// Guest state
// ---------------------------------------------------------------------------

fn pattern(address: u64) -> u8 {
    (address as u8).wrapping_mul(29).wrapping_add(0x47) ^ (address >> 8) as u8
}

fn region(base: u64, len: usize) -> Vec<u8> {
    (0..len as u64).map(|at| pattern(base + at)).collect()
}

/// Memory the cases can observe. Stack bytes below the initial stack pointer
/// are scratch and are not compared.
#[derive(Clone, PartialEq)]
struct Image {
    regions: BTreeMap<u64, Vec<u8>>,
}

impl Image {
    fn new() -> Self {
        let mut regions = BTreeMap::new();
        // Two separately mapped pages, so accesses can cross a mapping boundary.
        let second = DATA + PAGE as u64;
        for base in [DATA, second, READ_ONLY, STACK_PAGE, FS_BASE, GS_BASE] {
            regions.insert(base, region(base, PAGE));
        }
        Self { regions }
    }

    fn set(&mut self, address: u64, bytes: &[u8]) {
        for (address, byte) in (address..).zip(bytes) {
            let page = self.regions.get_mut(&(address & !0xfff));
            page.expect("mapped address")[address as usize & 0xfff] = *byte;
        }
    }

    fn load(&self, code: &[u8]) -> GuestMemory {
        let mut page = vec![0xcc; PAGE];
        page[..code.len()].copy_from_slice(code);
        page[code.len()] = 0xf4; // HLT ends the case.
        let mut memory = GuestMemory::new();
        memory
            .map_region_with_data(CODE, PAGE, MemoryPermissions::READ_EXECUTE, &page)
            .expect("map code");
        for (&base, data) in &self.regions {
            let permissions = if base == READ_ONLY {
                MemoryPermissions::READ_ONLY
            } else {
                MemoryPermissions::READ_WRITE
            };
            memory
                .map_region_with_data(base, data.len(), permissions, data)
                .expect("map data");
        }
        memory
    }

    fn store(memory: &GuestMemory) -> Self {
        let mut image = Self::new();
        for (&base, data) in &mut image.regions {
            memory.read_bytes(base, data).expect("read back");
        }
        image
    }

    fn differences(&self, expected: &Self) -> String {
        let mut text = String::new();
        for (&base, data) in &self.regions {
            let first = if base == STACK_PAGE {
                (STACK - STACK_PAGE) as usize
            } else {
                0
            };
            let wanted = &expected.regions[&base];
            for at in first..data.len() {
                if data[at] != wanted[at] {
                    let address = base + at as u64;
                    let _ = write!(
                        text,
                        " [{address:x}]={:02x} want {:02x}",
                        data[at], wanted[at]
                    );
                }
            }
        }
        text
    }
}

fn initial_cpu() -> CpuState {
    let mut cpu = CpuState::new(CODE, STACK);
    for (index, register) in cpu.gpr.iter_mut().enumerate() {
        if index != 4 {
            *register = 0xa55a_c33c_9669_0000 | (index as u64 * 0x0101 + 0x9020);
        }
    }
    for (index, register) in cpu.xmm.iter_mut().enumerate() {
        *register = 0x5aa5_3cc3_6996_f00f_7ee7_1881_2442_0000 | (index as u128 * 0x0111 + 0x0203);
    }
    cpu.fs_base = FS_BASE;
    cpu.gs_base = GS_BASE;
    // None of the three is what FNINIT sets, so a reset is always visible and
    // an instruction that wrongly resets one is caught by any case.
    cpu.x87 = X87State {
        control: 0x0c40,
        status: 0x2b05,
        tag: 0x5a5a,
    };
    cpu
}

fn set_flags(flags: &mut RFlags, bits: u8) {
    flags.cf = bits & CF != 0;
    flags.pf = bits & PF != 0;
    flags.af = bits & AF != 0;
    flags.zf = bits & ZF != 0;
    flags.sf = bits & SF != 0;
    flags.of = bits & OF != 0;
}

fn get_flags(flags: &RFlags) -> u8 {
    u8::from(flags.cf)
        | u8::from(flags.pf) << 1
        | u8::from(flags.af) << 2
        | u8::from(flags.zf) << 3
        | u8::from(flags.sf) << 4
        | u8::from(flags.of) << 5
}

fn show_flags(bits: u8, mask: u8) -> String {
    "CPAZSO"
        .chars()
        .enumerate()
        .map(|(at, letter)| match (mask >> at & 1, bits >> at & 1) {
            (0, _) => '-',
            (_, 0) => letter.to_ascii_lowercase(),
            _ => letter,
        })
        .collect()
}

/// Final state a case requires, with the parts it leaves open.
struct Expected {
    cpu: CpuState,
    image: Image,
    flag_mask: u8,
    mxcsr_mask: u32,
    open_gpr: u16,
    open_xmm: u16,
}

/// Apply `text` to a state. `open` collects the parts marked `=?`.
fn apply(text: &str, expected: &mut Expected) {
    for token in text.split_whitespace() {
        let (key, data) = token.split_once('=').expect("key=value");
        if matches!(key, "f" | "keep") {
            continue;
        }
        if let Some(index) = REGISTERS.iter().position(|name| *name == key) {
            if data == "?" {
                expected.open_gpr |= 1 << index;
            } else {
                expected.cpu.gpr[index] = value(data);
            }
        } else if key == "rip" {
            expected.cpu.rip = value(data);
        } else if key == "gsbase" {
            expected.cpu.gs_base = value(data);
        } else if let Some(word) = match key {
            "fcw" => Some(&mut expected.cpu.x87.control),
            "fsw" => Some(&mut expected.cpu.x87.status),
            "ftw" => Some(&mut expected.cpu.x87.tag),
            _ => None,
        } {
            *word = u16::try_from(value(data)).expect("a 16-bit x87 word");
        } else if key == "mx" {
            if data == "?" {
                expected.mxcsr_mask = 0;
            } else if let Some(masked) = data.strip_prefix('~') {
                let (ignored, bits) = masked.split_once(':').expect("mask:value");
                expected.mxcsr_mask = !(value(ignored) as u32);
                expected.cpu.mxcsr = Mxcsr(value(bits) as u32);
            } else {
                expected.cpu.mxcsr = Mxcsr(value(data) as u32);
            }
        } else if let Some(index) = key.strip_prefix('x') {
            let index: usize = index.parse().expect("xmm index");
            if data == "?" {
                expected.open_xmm |= 1 << index;
            } else {
                expected.cpu.xmm[index] = xmm_value(data, expected.cpu.xmm[index]);
            }
        } else {
            let (base, offset) = if let Some(offset) = key.strip_prefix("fs") {
                (FS_BASE, offset)
            } else if let Some(offset) = key.strip_prefix("gs") {
                (GS_BASE, offset)
            } else {
                (DATA, key.strip_prefix('m').expect("known key"))
            };
            expected.image.set(base + value(offset), &hex_bytes(data));
        }
    }
}

fn initial_state(case: &Case) -> Expected {
    let mut start = Expected {
        cpu: initial_cpu(),
        image: Image::new(),
        flag_mask: 0,
        mxcsr_mask: !0,
        open_gpr: 0,
        open_xmm: 0,
    };
    apply(case.input, &mut start);
    start
}

fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.split_whitespace()
        .find_map(|token| token.strip_prefix(key)?.strip_prefix('='))
}

// ---------------------------------------------------------------------------
// Engines
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Engine {
    Interpreter,
    Dbt,
    /// The second run of a block the engine already translated.
    DbtCached,
    /// One instruction per dispatch, so every block is cut short.
    DbtStepped,
}

const ENGINES: [Engine; 4] = [
    Engine::Interpreter,
    Engine::Dbt,
    Engine::DbtCached,
    Engine::DbtStepped,
];
const BUDGET: usize = 1000;
const JIT_CAPACITY: usize = 64 * 1024;

fn new_dbt() -> DbtEngine {
    DbtEngine::new(JIT_CAPACITY).expect("JIT memory")
}

fn run(engine: Engine, cpu: &CpuState, memory: &mut GuestMemory) -> (CpuState, ExitReason) {
    match engine {
        Engine::Interpreter => {
            let result = Interpreter::new(cpu.clone(), memory, BUDGET).run();
            (result.final_state, result.exit_reason)
        }
        Engine::Dbt => run_dbt(&mut new_dbt(), cpu, memory, BUDGET),
        Engine::DbtCached => {
            let mut dbt = new_dbt();
            let mut scratch = memory.clone();
            run_dbt(&mut dbt, cpu, &mut scratch, BUDGET);
            let warm = dbt.metrics;
            let result = run_dbt(&mut dbt, cpu, memory, BUDGET);
            // Nothing is cached when the first instruction does not decode.
            assert!(
                warm.blocks_compiled == 0 || dbt.metrics.cache_hits > warm.cache_hits,
                "second run missed the cache: {warm:?} then {:?}",
                dbt.metrics
            );
            assert_eq!(dbt.metrics.blocks_compiled, warm.blocks_compiled);
            result
        }
        Engine::DbtStepped => {
            let mut dbt = new_dbt();
            let mut state = cpu.clone();
            for _ in 0..BUDGET {
                let (next, exit) = run_dbt(&mut dbt, &state, memory, 1);
                if exit != ExitReason::StepLimitExceeded {
                    return (next, exit);
                }
                state = next;
            }
            (state, ExitReason::StepLimitExceeded)
        }
    }
}

fn run_dbt(
    dbt: &mut DbtEngine,
    cpu: &CpuState,
    memory: &mut GuestMemory,
    budget: usize,
) -> (CpuState, ExitReason) {
    let mut state = cpu.clone();
    let result = dbt.run(&mut state, memory, budget);
    assert_eq!(state, result.final_state, "DBT reported two final states");
    (result.final_state, result.exit_reason)
}

fn fault_matches(expected: &str, exit: &ExitReason) -> bool {
    let mut words = expected.split_whitespace();
    let kind = words.next().expect("fault kind");
    let address = words.next().map(value);
    match (kind, exit) {
        ("DE", ExitReason::DivideError)
        | ("GP", ExitReason::GeneralProtection)
        | ("UD", ExitReason::IllegalInstruction(_))
        | ("XM", ExitReason::SimdException) => true,
        ("PFR" | "PFW" | "PFX", ExitReason::MemoryFault(MemoryError::UnmappedAddress(at))) => {
            Some(*at) == address
        }
        (
            kind,
            ExitReason::MemoryFault(MemoryError::PermissionViolation {
                address: at,
                required,
            }),
        ) => {
            let access = match kind {
                "PFR" => "read",
                "PFW" => "write",
                "PFX" => "execute",
                _ => return false,
            };
            Some(*at) == address && *required == access
        }
        _ => false,
    }
}

/// Every way the case differs from its known answer.
fn check(case: &Case, engine: Engine, initial_flags: u8) -> Result<(), String> {
    let mut start = initial_state(case);
    set_flags(&mut start.cpu.flags, initial_flags);
    let mut memory = start.image.load(&case.code);

    let mut want = Expected {
        cpu: start.cpu.clone(),
        image: start.image.clone(),
        ..start
    };
    let faulting = case.faulting.map(|index| case.offsets[index]);
    want.cpu.rip = CODE + faulting.unwrap_or(case.code.len() + 1) as u64;
    apply(case.output, &mut want);
    let mut flags = initial_flags;
    want.flag_mask = match field(case.output, "f") {
        Some("*") => ALL_FLAGS,
        Some(text) => {
            let (set, clear) = (flag_bits(text, true), flag_bits(text, false));
            flags = (flags | set) & !clear;
            set | clear | field(case.output, "keep").map_or(0, |kept| flag_bits(kept, true))
        }
        None if !case.fault.is_empty() => ALL_FLAGS,
        None => 0,
    };

    let (cpu, exit) = run(engine, &start.cpu, &mut memory);
    let mut text = String::new();
    let expected_exit = if case.fault.is_empty() {
        exit == ExitReason::Halt
    } else {
        fault_matches(case.fault, &exit)
    };
    if !expected_exit {
        let wanted = if case.fault.is_empty() {
            "Halt"
        } else {
            case.fault
        };
        let _ = write!(text, " exit {exit:?} want {wanted};");
    }
    for (index, name) in REGISTERS.iter().enumerate() {
        if want.open_gpr >> index & 1 == 0 && cpu.gpr[index] != want.cpu.gpr[index] {
            let _ = write!(
                text,
                " {name}={:016x} want {:016x};",
                cpu.gpr[index], want.cpu.gpr[index]
            );
        }
        if want.open_xmm >> index & 1 == 0 && cpu.xmm[index] != want.cpu.xmm[index] {
            let _ = write!(
                text,
                " xmm{index}={:032x} want {:032x};",
                cpu.xmm[index], want.cpu.xmm[index]
            );
        }
    }
    if cpu.rip != want.cpu.rip {
        let _ = write!(text, " rip={:x} want {:x};", cpu.rip, want.cpu.rip);
    }
    let got_flags = get_flags(&cpu.flags);
    if (got_flags ^ flags) & want.flag_mask != 0 {
        let _ = write!(
            text,
            " flags={} want {};",
            show_flags(got_flags, want.flag_mask),
            show_flags(flags, want.flag_mask)
        );
    }
    if (cpu.mxcsr.0 ^ want.cpu.mxcsr.0) & want.mxcsr_mask != 0 {
        let _ = write!(
            text,
            " mxcsr={:x} want {:x} under {:x};",
            cpu.mxcsr.0, want.cpu.mxcsr.0, want.mxcsr_mask
        );
    }
    if (cpu.fs_base, cpu.gs_base) != (want.cpu.fs_base, want.cpu.gs_base) {
        text.push_str(" segment base changed;");
    }
    if cpu.x87 != want.cpu.x87 {
        let _ = write!(text, " x87={:04x?} want {:04x?};", cpu.x87, want.cpu.x87);
    }
    let memory_text = Image::store(&memory).differences(&want.image);
    if !memory_text.is_empty() {
        let _ = write!(text, " memory{memory_text};");
    }
    if text.is_empty() { Ok(()) } else { Err(text) }
}

fn flag_sweep(case: &Case) -> Vec<u8> {
    match field(case.input, "f") {
        Some(text) => vec![flag_bits(text, true)],
        None => vec![0, ALL_FLAGS],
    }
}

/// Failures of one case, with engines that fail the same way on one line.
fn failures(case: &Case) -> Vec<String> {
    let mut seen: Vec<(String, Vec<String>)> = Vec::new();
    for flags in flag_sweep(case) {
        for engine in ENGINES {
            if let Err(text) = check(case, engine, flags) {
                let who = format!("{engine:?}/{}", show_flags(flags, ALL_FLAGS));
                match seen.iter_mut().find(|(known, _)| *known == text) {
                    Some((_, engines)) => engines.push(who),
                    None => seen.push((text, vec![who])),
                }
            }
        }
    }
    seen.into_iter()
        .map(|(text, engines)| {
            format!(
                "{} [{}] ({})\n    {}:{text}",
                case.name,
                case.asm,
                case.cite,
                engines.join(", ")
            )
        })
        .collect()
}

fn check_group(table: &'static str) {
    let cases = parse(table);
    let mut report = Vec::new();
    let mut verified = 0;
    for case in cases.iter().filter(|case| refused_at(case.name).is_none()) {
        let found = failures(case);
        if found.is_empty() {
            verified += 1;
        }
        report.extend(found);
    }
    println!("{verified} known answers verified of {}", cases.len());
    assert!(
        report.is_empty(),
        "{} of {} cases disagree with the manual:\n{}",
        report.len(),
        cases.len(),
        report.join("\n")
    );
}

#[test]
fn add_sub_flags() {
    check_group(ADD_SUB_FLAGS);
}

#[test]
fn carry_chains() {
    check_group(CARRY_CHAINS);
}

#[test]
fn logic() {
    check_group(LOGIC);
}

#[test]
fn partial_registers() {
    check_group(PARTIAL_REGISTERS);
}

#[test]
fn exchange() {
    check_group(EXCHANGE);
}

#[test]
fn immediate_stores() {
    check_group(IMMEDIATE_STORES);
}

#[test]
fn flag_moves() {
    check_group(FLAG_MOVES);
}

#[test]
fn shift_rotate() {
    check_group(SHIFT_ROTATE);
}

#[test]
fn multiply_divide() {
    check_group(MULTIPLY_DIVIDE);
}

#[test]
fn bit_operations() {
    check_group(BIT_OPERATIONS);
}

#[test]
fn conditions() {
    check_group(CONDITIONS);
}

#[test]
fn stack_and_calls() {
    check_group(STACK_AND_CALLS);
}

#[test]
fn strings() {
    check_group(STRINGS);
}

#[test]
fn fs_gs() {
    check_group(FS_GS);
}

#[test]
fn mov_moffs() {
    check_group(MOV_MOFFS);
}

#[test]
fn decoding() {
    check_group(DECODING);
}

#[test]
fn fences() {
    check_group(FENCES);
}

#[test]
fn x87_init() {
    check_group(X87_INIT);
}

#[test]
fn memory_faults() {
    check_group(MEMORY_FAULTS);
}

#[test]
fn straddling_fault_address() {
    check_group(STRADDLING_FAULT_ADDRESS);
}

#[test]
fn noncanonical_address() {
    check_group(NONCANONICAL_ADDRESS);
}

#[test]
fn packed_integer_sse2() {
    check_group(PACKED_INTEGER_SSE2);
}

#[test]
fn packed_integer_beyond_sse2() {
    check_group(PACKED_INTEGER_BEYOND_SSE2);
}

#[test]
fn fp_rounding() {
    check_group(FP_ROUNDING);
}

#[test]
fn fp_overflow_underflow() {
    check_group(FP_OVERFLOW_UNDERFLOW);
}

#[test]
fn fp_ftz_daz_denormals() {
    check_group(FP_FTZ_DAZ_DENORMALS);
}

#[test]
fn fp_nan_invalid_zero_divide() {
    check_group(FP_NAN_INVALID_ZERO_DIVIDE);
}

#[test]
fn fp_min_max_compare() {
    check_group(FP_MIN_MAX_COMPARE);
}

#[test]
fn fp_conversions() {
    check_group(FP_CONVERSIONS);
}

#[test]
fn fp_packed_and_sticky() {
    check_group(FP_PACKED_AND_STICKY);
}

#[test]
fn fp_mxcsr_and_unmasked() {
    check_group(FP_MXCSR_AND_UNMASKED);
}

#[test]
fn fp_beyond_sse2() {
    check_group(FP_BEYOND_SSE2);
}

#[test]
fn instruction_length() {
    check_group(INSTRUCTION_LENGTH);
}

#[test]
fn exact_unmasked_overflow() {
    check_group(EXACT_UNMASKED_OVERFLOW);
}

#[test]
fn page_crossing() {
    check_group(PAGE_CROSSING);
}

fn all_cases() -> Vec<(&'static str, Case)> {
    GROUPS
        .iter()
        .flat_map(|(group, table)| parse(table).into_iter().map(move |case| (*group, case)))
        .collect()
}

#[test]
fn case_names_are_unique_and_listed_exceptions_exist() {
    let cases = all_cases();
    let names: BTreeSet<&str> = cases.iter().map(|(_, case)| case.name).collect();
    assert_eq!(names.len(), cases.len(), "duplicate case name");
    for (name, _) in UNSUPPORTED {
        assert!(names.contains(name), "unknown case {name}");
    }
}

/// The instruction index at which both engines refuse a case, if they do.
fn refused_at(name: &str) -> Option<usize> {
    let entry = UNSUPPORTED.iter().find(|(known, _)| *known == name)?;
    Some(entry.1)
}

/// A form outside the supported set must be refused at that instruction, never
/// approximated. The state must be exactly what the instructions before it left:
/// the reference is an interpreter run of only those instructions, and their
/// own results are checked by other cases.
#[test]
fn unsupported_forms_are_refused_without_changing_state() {
    for (_, case) in all_cases() {
        let Some(index) = refused_at(case.name) else {
            continue;
        };
        let offset = case.offsets[index];
        for flags in flag_sweep(&case) {
            let mut start = initial_state(&case);
            set_flags(&mut start.cpu.flags, flags);
            let mut before = start.image.load(&case.code[..offset]);
            let (mut wanted, exit) = run(Engine::Interpreter, &start.cpu, &mut before);
            assert_eq!(exit, ExitReason::Halt, "{} prefix", case.name);
            wanted.rip = CODE + offset as u64;
            for engine in ENGINES {
                let mut memory = start.image.load(&case.code);
                let (cpu, exit) = run(engine, &start.cpu, &mut memory);
                let what = format!("{} on {engine:?}", case.name);
                assert!(
                    matches!(exit, ExitReason::IllegalInstruction(_)),
                    "{what}: {exit:?}"
                );
                assert_eq!(cpu, wanted, "{what}");
                // Scratch stack included: a refused push must not write it.
                let (got, wanted) = (Image::store(&memory), Image::store(&before));
                assert!(got == wanted, "{what}: memory changed");
            }
        }
    }
}

/// Which route each verified instruction takes in the DBT. The per-instruction
/// answer of `is_native` must agree with the engine's own fallback counter for
/// every straight-line case that does not fault; for the others the route is only
/// what `is_native` predicts. Run with `--nocapture` for the list.
#[test]
fn dbt_route_of_every_case_is_recorded() {
    let mut native: BTreeSet<&str> = BTreeSet::new();
    let mut helper: BTreeSet<&str> = BTreeSet::new();
    let mut counts: BTreeMap<&str, [usize; 3]> = BTreeMap::new();
    let (mut counted, mut predicted) = (0, 0);
    for (group, case) in all_cases() {
        if refused_at(case.name).is_some() {
            continue;
        }
        let start = initial_state(&case);
        let memory = start.image.load(&case.code);
        let text: Vec<&str> = case.asm.split(';').map(str::trim).collect();
        let mut routes = Vec::new();
        let mut repeats = false;
        for (index, &offset) in case.offsets.iter().enumerate() {
            let Ok(insn) = decode(CODE + offset as u64, |at| memory.fetch_instruction_byte(at))
            else {
                continue;
            };
            let name = text.get(index).copied().unwrap_or(case.asm);
            routes.push(is_native(&insn));
            repeats |= insn.rep;
            if is_native(&insn) {
                native.insert(name);
            } else {
                helper.insert(name);
            }
        }
        let kind = match (routes.contains(&true), routes.contains(&false)) {
            (true, false) => 0,
            (false, true) => 1,
            (true, true) => 2,
            (false, false) => continue,
        };
        counts.entry(group).or_default()[kind] += 1;
        // Straight-line cases run each instruction once, so the counter must equal
        // the number of helper instructions. A REP form runs once per element.
        let labelled = text.iter().any(|piece| {
            piece
                .split_once(':')
                .is_some_and(|(label, _)| label.parse::<u8>().is_ok())
        });
        if !labelled && !repeats && case.fault.is_empty() && routes.len() == case.offsets.len() {
            let mut dbt = new_dbt();
            let mut memory = memory.clone();
            run_dbt(&mut dbt, &start.cpu, &mut memory, BUDGET);
            assert_eq!(
                dbt.metrics.interpreter_fallbacks,
                routes.iter().filter(|native| !**native).count(),
                "{}: {:?}",
                case.name,
                dbt.metrics
            );
            counted += 1;
        } else {
            predicted += 1;
        }
    }
    println!(
        "{counted} cases confirmed by the fallback counter; {predicted} cases with branches or \
         faults are only predicted by is_native"
    );
    println!("group: cases with only native code / only helpers / both");
    for (group, [only_native, only_helper, both]) in counts {
        println!("{group}: {only_native} / {only_helper} / {both}");
    }
    println!("native instructions:");
    native.iter().for_each(|name| println!("  {name}"));
    println!("helper instructions:");
    helper.iter().for_each(|name| println!("  {name}"));
}

// ---------------------------------------------------------------------------
// Sweeps computed from the manual's definitions
// ---------------------------------------------------------------------------

/// Operands are AL/AX/EAX/RAX and BL/BX/EBX/RBX; shift counts are in CL.
const BINARY: &[(&str, [&str; 4])] = &[
    ("add", ["00d8", "6601d8", "01d8", "4801d8"]),
    ("adc", ["10d8", "6611d8", "11d8", "4811d8"]),
    ("sub", ["28d8", "6629d8", "29d8", "4829d8"]),
    ("sbb", ["18d8", "6619d8", "19d8", "4819d8"]),
    ("cmp", ["38d8", "6639d8", "39d8", "4839d8"]),
    ("and", ["20d8", "6621d8", "21d8", "4821d8"]),
    ("or", ["08d8", "6609d8", "09d8", "4809d8"]),
    ("xor", ["30d8", "6631d8", "31d8", "4831d8"]),
    ("test", ["84d8", "6685d8", "85d8", "4885d8"]),
];
const UNARY: &[(&str, [&str; 4])] = &[
    ("inc", ["fec0", "66ffc0", "ffc0", "48ffc0"]),
    ("dec", ["fec8", "66ffc8", "ffc8", "48ffc8"]),
    ("neg", ["f6d8", "66f7d8", "f7d8", "48f7d8"]),
    ("not", ["f6d0", "66f7d0", "f7d0", "48f7d0"]),
];
const SHIFTS: &[(&str, [&str; 4])] = &[
    ("rol", ["d2c0", "66d3c0", "d3c0", "48d3c0"]),
    ("ror", ["d2c8", "66d3c8", "d3c8", "48d3c8"]),
    ("rcl", ["d2d0", "66d3d0", "d3d0", "48d3d0"]),
    ("rcr", ["d2d8", "66d3d8", "d3d8", "48d3d8"]),
    ("shl", ["d2e0", "66d3e0", "d3e0", "48d3e0"]),
    ("shr", ["d2e8", "66d3e8", "d3e8", "48d3e8"]),
    ("sar", ["d2f8", "66d3f8", "d3f8", "48d3f8"]),
];
const WIDTHS: [u32; 4] = [8, 16, 32, 64];

fn mask(bits: u32) -> u64 {
    u64::MAX >> (64 - bits)
}

fn parity_even(result: u64) -> bool {
    (result & 0xff).count_ones().is_multiple_of(2)
}

fn pack(c: bool, p: bool, a: bool, z: bool, s: bool, o: bool) -> u8 {
    u8::from(c)
        | u8::from(p) << 1
        | u8::from(a) << 2
        | u8::from(z) << 3
        | u8::from(s) << 4
        | u8::from(o) << 5
}

fn signed(value: u64, bits: u32) -> i128 {
    let value = i128::from(value);
    if value >> (bits - 1) & 1 == 1 {
        value - (1_i128 << bits)
    } else {
        value
    }
}

/// Register value after writing `result` to a `bits`-wide operand
/// (SDM 1 3.4.1.1).
fn written(old: u64, result: u64, bits: u32) -> u64 {
    match bits {
        64 => result,
        32 => result,
        _ => (old & !mask(bits)) | result,
    }
}

/// Result (`None` when nothing is written), flags and the defined flags, from
/// the status flag definitions of SDM 1 3.4.3.1 and the "Flags Affected" text of
/// each entry in Volume 2.
fn arithmetic(op: &str, a: u64, b: u64, bits: u32, flags: u8) -> (Option<u64>, u8, u8) {
    let carry = u64::from(flags & CF != 0);
    let limit = 1_i128 << bits;
    let in_range = |exact: i128| -(limit / 2) <= exact && exact < limit / 2;
    let (ua, sa) = (i128::from(a), signed(a, bits));
    let add = |b_value: u64, carry: u64| {
        let exact = ua + i128::from(b_value) + i128::from(carry);
        let result = (exact % limit) as u64;
        let overflow = !in_range(sa + signed(b_value, bits) + i128::from(carry));
        let half = (a & 15) + (b_value & 15) + carry > 15;
        (result, exact >= limit, half, overflow)
    };
    let subtract = |x: u64, y: u64, borrow: u64| {
        let exact = i128::from(x) - i128::from(y) - i128::from(borrow);
        let result = exact.rem_euclid(limit) as u64;
        let overflow = !in_range(signed(x, bits) - signed(y, bits) - i128::from(borrow));
        let half = (x & 15) < (y & 15) + borrow;
        (result, exact < 0, half, overflow)
    };
    let finish = |(result, carry_out, half, overflow): (u64, bool, bool, bool)| {
        let sign = result >> (bits - 1) & 1 == 1;
        let flags = pack(
            carry_out,
            parity_even(result),
            half,
            result == 0,
            sign,
            overflow,
        );
        (result, flags)
    };
    let logic = |result: u64| {
        let (result, flags) = finish((result, false, false, false));
        (result, flags, ALL_FLAGS & !AF)
    };
    match op {
        "add" => {
            let (result, flags) = finish(add(b, 0));
            (Some(result), flags, ALL_FLAGS)
        }
        "adc" => {
            let (result, flags) = finish(add(b, carry));
            (Some(result), flags, ALL_FLAGS)
        }
        "sub" | "cmp" => {
            let (result, flags) = finish(subtract(a, b, 0));
            ((op == "sub").then_some(result), flags, ALL_FLAGS)
        }
        "sbb" => {
            let (result, flags) = finish(subtract(a, b, carry));
            (Some(result), flags, ALL_FLAGS)
        }
        "and" | "test" => {
            let (result, flags, defined) = logic(a & b);
            ((op == "and").then_some(result), flags, defined)
        }
        "or" => {
            let (result, flags, defined) = logic(a | b);
            (Some(result), flags, defined)
        }
        "xor" => {
            let (result, flags, defined) = logic(a ^ b);
            (Some(result), flags, defined)
        }
        "inc" => {
            let (result, new) = finish(add(1, 0));
            (Some(result), new & !CF | flags & CF, ALL_FLAGS)
        }
        "dec" => {
            let (result, new) = finish(subtract(a, 1, 0));
            (Some(result), new & !CF | flags & CF, ALL_FLAGS)
        }
        "neg" => {
            let (result, flags) = finish(subtract(0, a, 0));
            (Some(result), flags, ALL_FLAGS)
        }
        "not" => (Some(!a & mask(bits)), flags, ALL_FLAGS),
        _ => panic!("{op}"),
    }
}

/// The bit loops of SDM 2B "RCL/RCR/ROL/ROR" and "SAL/SAR/SHL/SHR".
fn shift(op: &str, operand: u64, count: u8, bits: u32, flags: u8) -> (u64, u8, u8) {
    let masked = u32::from(count) & if bits == 64 { 0x3f } else { 0x1f };
    if masked == 0 {
        return (operand, flags, ALL_FLAGS);
    }
    let msb = |value: u64| value >> (bits - 1) & 1 == 1;
    let second = |value: u64| value >> (bits - 2) & 1 == 1;
    let turns = match op {
        "rcl" | "rcr" if bits < 32 => masked % (bits + 1),
        "rol" | "ror" => masked % bits,
        _ => masked,
    };
    let (mut value, mut carry) = (operand, flags & CF != 0);
    let rcr_overflow = msb(operand) != carry;
    for _ in 0..turns {
        let (high, low) = (msb(value), value & 1 == 1);
        let into_low = |value: u64, bit: bool| (value << 1) & mask(bits) | u64::from(bit);
        let into_high = |value: u64, bit: bool| value >> 1 | u64::from(bit) << (bits - 1);
        (value, carry) = match op {
            "rol" => (into_low(value, high), carry),
            "ror" => (into_high(value, low), carry),
            "rcl" => (into_low(value, carry), high),
            "rcr" => (into_high(value, carry), low),
            "shl" => (into_low(value, false), high),
            "shr" => (into_high(value, false), low),
            "sar" => (into_high(value, high), low),
            _ => panic!("{op}"),
        };
    }
    match op {
        "rol" => carry = value & 1 == 1,
        "ror" => carry = msb(value),
        _ => {}
    }
    let overflow = match op {
        "rol" | "rcl" | "shl" => msb(value) != carry,
        "ror" => msb(value) != second(value),
        "rcr" => rcr_overflow,
        "shr" => msb(operand),
        "sar" => false,
        _ => panic!("{op}"),
    };
    let one = if masked == 1 { OF } else { 0 };
    if op.starts_with('r') {
        let new = flags & !(CF | OF) | pack(carry, false, false, false, false, overflow);
        return (value, new, ALL_FLAGS & !OF | one);
    }
    let carry_defined = op == "sar" || masked < bits;
    let new = pack(
        carry,
        parity_even(value),
        false,
        value == 0,
        msb(value),
        overflow,
    );
    let defined = PF | ZF | SF | one | if carry_defined { CF } else { 0 };
    (value, new, defined)
}

/// Both engines on one instruction, reusing the DBT's translated block.
struct Bench {
    memory: GuestMemory,
    dbt: DbtEngine,
    runs: usize,
}

impl Bench {
    fn new(hex: &str) -> Self {
        Self {
            memory: Image::new().load(&hex_bytes(hex)),
            dbt: new_dbt(),
            runs: 0,
        }
    }

    /// Whether both engines refuse the instruction as unsupported.
    fn refused(&mut self) -> bool {
        let cpu = initial_cpu();
        let (_, first) = run(Engine::Interpreter, &cpu, &mut self.memory);
        let (_, second) = run_dbt(&mut self.dbt, &cpu, &mut self.memory, BUDGET);
        assert_eq!(first, second);
        matches!(first, ExitReason::IllegalInstruction(_))
    }

    /// Runs with AX/BX/CX-family inputs and compares RAX and the defined flags.
    fn expect(&mut self, what: &str, inputs: [u64; 3], flags: u8, rax: u64, want: (u8, u8)) {
        let mut cpu = initial_cpu();
        (cpu.gpr[0], cpu.gpr[1], cpu.gpr[3]) = (inputs[0], inputs[2], inputs[1]);
        set_flags(&mut cpu.flags, flags);
        let mut wanted = cpu.clone();
        wanted.gpr[0] = rax;
        wanted.rip = 0;
        for engine in [Engine::Interpreter, Engine::Dbt] {
            let (mut got, exit) = match engine {
                Engine::Interpreter => run(engine, &cpu, &mut self.memory),
                _ => run_dbt(&mut self.dbt, &cpu, &mut self.memory, BUDGET),
            };
            assert_eq!(exit, ExitReason::Halt, "{what} on {engine:?}");
            let got_flags = get_flags(&got.flags);
            assert!(
                (got_flags ^ want.0) & want.1 == 0,
                "{what} rax={:x} rbx={:x} rcx={:x} flags={} on {engine:?}: flags={} want {}",
                inputs[0],
                inputs[1],
                inputs[2],
                show_flags(flags, ALL_FLAGS),
                show_flags(got_flags, want.1),
                show_flags(want.0, want.1)
            );
            got.flags = wanted.flags;
            got.rip = 0;
            assert_eq!(got, wanted, "{what} on {engine:?}");
            self.runs += 1;
        }
    }
}

fn boundary_values(bits: u32) -> Vec<u64> {
    let top = 1_u64 << (bits - 1);
    let mut values = vec![
        0,
        1,
        2,
        7,
        8,
        0x0f,
        0x10,
        0x55,
        0x7f,
        0x80,
        0xfe,
        0xff,
        top - 1,
        top,
        top + 1,
        mask(bits) - 1,
        mask(bits),
        0xa5a5_a5a5_a5a5_a5a5 & mask(bits),
    ];
    values.sort_unstable();
    values.dedup();
    values
}

/// Garbage above the operand must not matter and, below 32 bits, must survive.
fn dirty(value: u64, bits: u32) -> u64 {
    value | 0xdead_beef_7654_3210 & !mask(bits)
}

#[test]
fn arithmetic_sweep_matches_status_flag_definitions() {
    let mut runs = 0;
    for (op, forms) in BINARY {
        for (hex, bits) in forms.iter().zip(WIDTHS) {
            let mut bench = Bench::new(hex);
            for a in boundary_values(bits) {
                for b in boundary_values(bits) {
                    for flags in [0, CF, ALL_FLAGS & !CF, ALL_FLAGS] {
                        let (result, new, defined) = arithmetic(op, a, b, bits, flags);
                        let rax = dirty(a, bits);
                        let want = result.map_or(rax, |result| written(rax, result, bits));
                        let what = format!("{op}{bits}");
                        bench.expect(&what, [rax, dirty(b, bits), 3], flags, want, (new, defined));
                    }
                }
            }
            runs += bench.runs;
        }
    }
    for (op, forms) in UNARY {
        for (hex, bits) in forms.iter().zip(WIDTHS) {
            let mut bench = Bench::new(hex);
            for a in boundary_values(bits) {
                for flags in [0, CF, ALL_FLAGS & !CF, ALL_FLAGS] {
                    let (result, new, defined) = arithmetic(op, a, 0, bits, flags);
                    let rax = dirty(a, bits);
                    let want = written(rax, result.expect("unary writes"), bits);
                    let what = format!("{op}{bits}");
                    bench.expect(&what, [rax, 5, 3], flags, want, (new, defined));
                }
            }
            runs += bench.runs;
        }
    }
    println!("{runs} engine runs");
}

/// Every 8-bit operand pair and carry for the additive operations.
#[test]
fn eight_bit_arithmetic_is_exhaustive() {
    for (op, forms) in BINARY.iter().take(5) {
        let mut bench = Bench::new(forms[0]);
        for a in 0..=0xff {
            for b in 0..=0xff {
                for flags in [0, CF] {
                    let (result, new, defined) = arithmetic(op, a, b, 8, flags);
                    let want = result.unwrap_or(a);
                    bench.expect(op, [a, b, 3], flags, want, (new, defined));
                }
            }
        }
        println!("{op}: {} engine runs", bench.runs);
    }
}

#[test]
fn shift_and_rotate_sweep_matches_operation_pseudocode() {
    let mut runs = 0;
    let mut refused = Vec::new();
    for (op, forms) in SHIFTS {
        for (hex, bits) in forms.iter().zip(WIDTHS) {
            let mut bench = Bench::new(hex);
            if bench.refused() {
                refused.push(format!("{op}{bits}"));
                continue;
            }
            for operand in boundary_values(bits) {
                for count in 0..=0xff {
                    for flags in [0, CF, ALL_FLAGS] {
                        let (result, new, defined) = shift(op, operand, count, bits, flags);
                        let rax = dirty(operand, bits);
                        let want = written(rax, result, bits);
                        let rcx = 0x1234_5678_9abc_de00 | u64::from(count);
                        let what = format!("{op}{bits}");
                        bench.expect(&what, [rax, 5, rcx], flags, want, (new, defined));
                    }
                }
            }
            runs += bench.runs;
        }
    }
    assert_eq!(refused, Vec::<String>::new(), "a shift form is refused");
    println!("{runs} engine runs");
}

/// SDM 2A Jcc and 2B SETcc conditions against ordinary integer comparison.
#[test]
fn conditions_match_integer_comparison() {
    // cmp rax, rbx; then SETcc [rdi+n] for O NO B AE E NE BE A S NS P NP L GE LE G.
    const CODE_HEX: &str = "4839d80f90070f9147010f9247020f9347030f9447040f9547050f9647060f9747070f9847080f9947090f9a470a0f9b470b0f9c470c0f9d470d0f9e470e0f9f470f";
    let code = hex_bytes(CODE_HEX);
    let mut dbt = new_dbt();
    for a in boundary_values(64) {
        for b in boundary_values(64) {
            let (x, y) = (a as i64, b as i64);
            let difference = a.wrapping_sub(b);
            let overflow = x.checked_sub(y).is_none();
            let even = parity_even(difference);
            let pairs = [
                overflow,
                a < b,
                a == b,
                a <= b,
                (difference as i64) < 0,
                even,
                x < y,
                x <= y,
            ];
            let wanted: Vec<u8> = pairs
                .iter()
                .flat_map(|&yes| [u8::from(yes), u8::from(!yes)])
                .collect();
            let mut cpu = initial_cpu();
            (cpu.gpr[0], cpu.gpr[3], cpu.gpr[7]) = (a, b, DATA);
            for native in [false, true] {
                let mut memory = Image::new().load(&code);
                let (_, exit) = if native {
                    run_dbt(&mut dbt, &cpu, &mut memory, BUDGET)
                } else {
                    run(Engine::Interpreter, &cpu, &mut memory)
                };
                assert_eq!(exit, ExitReason::Halt);
                let mut got = [0; 16];
                memory.read_bytes(DATA, &mut got).expect("read back");
                assert_eq!(got[..], wanted[..], "cmp {a:x}, {b:x} native={native}");
            }
        }
    }
}

/// Deterministic operands: xorshift64* with a fixed seed.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// Lanes drawn half from range ends and half at random.
    fn vector(&mut self) -> u128 {
        const ENDS: [u64; 8] = [0, 1, 0x7f, 0x80, 0xff, 0x7fff, 0x8000, 0xffff];
        let mut whole = 0;
        for _ in 0..8 {
            let pick = self.next();
            let lane = if pick & 1 == 0 {
                ENDS[(pick >> 8) as usize % ENDS.len()]
            } else {
                pick >> 16 & 0xffff
            };
            whole = whole << 16 | u128::from(lane);
        }
        whole
    }
}

fn lane(value: u128, bits: u32, index: u32) -> u64 {
    (value >> (bits * index)) as u64 & mask(bits)
}

fn each(bits: u32, f: impl Fn(u32) -> u64) -> u128 {
    (0..128 / bits).fold(0, |whole, index| {
        whole | u128::from(f(index) & mask(bits)) << (bits * index)
    })
}

fn clamp(value: i128, low: i128, high: i128) -> u64 {
    value.clamp(low, high) as u64
}

/// Two-operand SSE2 integer instructions, lane by lane from the Operation text
/// of their entries in SDM 2B. `a` is the destination.
fn packed(op: &str, a: u128, b: u128) -> u128 {
    let bits = match op.as_bytes()[op.len() - 1] {
        b'b' => 8,
        b'w' => 16,
        b'd' => 32,
        _ => 64,
    };
    let ones = |yes: bool| if yes { u64::MAX } else { 0 };
    let wide = |value: u128, n: u32, i: u32| signed(lane(value, n, i), n);
    let unsigned = |value: u128, n: u32, i: u32| i128::from(lane(value, n, i));
    let map = |f: &dyn Fn(i128, i128) -> u64| each(bits, |i| f(wide(a, bits, i), wide(b, bits, i)));
    let umap = |f: &dyn Fn(i128, i128) -> u64| {
        each(bits, |i| f(unsigned(a, bits, i), unsigned(b, bits, i)))
    };
    let (low, high) = (-(1_i128 << (bits - 1)), (1_i128 << (bits - 1)) - 1);
    let top = i128::from(mask(bits));
    // Shift counts are the whole low quadword of the source.
    let count = b as u64;
    let shift_right_arithmetic = || {
        let by = count.min(u64::from(bits) - 1);
        each(bits, |i| (wide(a, bits, i) >> by) as u64)
    };
    let shifted = |f: &dyn Fn(u64) -> u64| {
        each(bits, |i| {
            if count >= u64::from(bits) {
                0
            } else {
                f(lane(a, bits, i))
            }
        })
    };
    let unpack =
        |n: u32, from: u32| each(n, |i| lane(if i % 2 == 0 { a } else { b }, n, from + i / 2));
    let pack_to = |n: u32, low: i128, high: i128| {
        let lanes = 128 / (2 * n);
        each(n, |i| {
            let source = if i < lanes { a } else { b };
            clamp(wide(source, 2 * n, i % lanes), low, high)
        })
    };
    match op {
        "paddb" | "paddw" | "paddd" | "paddq" => map(&|x, y| (x + y) as u64),
        "psubb" | "psubw" | "psubd" | "psubq" => map(&|x, y| (x - y) as u64),
        "paddsb" | "paddsw" => map(&|x, y| clamp(x + y, low, high)),
        "psubsb" | "psubsw" => map(&|x, y| clamp(x - y, low, high)),
        "paddusb" | "paddusw" => umap(&|x, y| clamp(x + y, 0, top)),
        "psubusb" | "psubusw" => umap(&|x, y| clamp(x - y, 0, top)),
        "pmullw" => map(&|x, y| (x * y) as u64),
        "pmulhw" => map(&|x, y| ((x * y) >> 16) as u64),
        "pmulhuw" => umap(&|x, y| ((x * y) >> 16) as u64),
        "pmuludq" => each(64, |i| lane(a, 32, 2 * i) * lane(b, 32, 2 * i)),
        "pmaddwd" => each(32, |i| {
            let product = |n| wide(a, 16, n) * wide(b, 16, n);
            (product(2 * i) + product(2 * i + 1)) as u64
        }),
        "psadbw" => each(64, |i| {
            (8 * i..8 * i + 8)
                .map(|n| lane(a, 8, n).abs_diff(lane(b, 8, n)))
                .sum()
        }),
        "pavgb" | "pavgw" => umap(&|x, y| ((x + y + 1) >> 1) as u64),
        "pminub" => umap(&|x, y| x.min(y) as u64),
        "pmaxub" => umap(&|x, y| x.max(y) as u64),
        "pminsw" => map(&|x, y| x.min(y) as u64),
        "pmaxsw" => map(&|x, y| x.max(y) as u64),
        "pcmpeqb" | "pcmpeqw" | "pcmpeqd" => map(&|x, y| ones(x == y)),
        "pcmpgtb" | "pcmpgtw" | "pcmpgtd" => map(&|x, y| ones(x > y)),
        "pand" => a & b,
        "pandn" => !a & b,
        "por" => a | b,
        "pxor" => a ^ b,
        "psllw" | "pslld" | "psllq" => shifted(&|x| x << count),
        "psrlw" | "psrld" | "psrlq" => shifted(&|x| x >> count),
        "psraw" | "psrad" => shift_right_arithmetic(),
        "packsswb" => pack_to(8, -128, 127),
        "packssdw" => pack_to(16, -32768, 32767),
        "packuswb" => pack_to(8, 0, 255),
        "punpcklbw" => unpack(8, 0),
        "punpcklwd" => unpack(16, 0),
        "punpckldq" => unpack(32, 0),
        "punpcklqdq" => unpack(64, 0),
        "punpckhbw" => unpack(8, 8),
        "punpckhwd" => unpack(16, 4),
        "punpckhdq" => unpack(32, 2),
        "punpckhqdq" => unpack(64, 1),
        _ => panic!("{op}"),
    }
}

/// Immediate shifts and shuffles of SDM 2B.
fn packed_immediate(op: &str, a: u128, b: u128, imm: u8) -> u128 {
    let field = |i: u32| u32::from(imm >> (2 * (i % 4)) & 3);
    match op {
        "pslldq" if imm > 15 => 0,
        "psrldq" if imm > 15 => 0,
        "pslldq" => a << (8 * u32::from(imm)),
        "psrldq" => a >> (8 * u32::from(imm)),
        "pshufd" => each(32, |i| lane(b, 32, field(i))),
        "pshuflw" => each(16, |i| lane(b, 16, if i < 4 { field(i) } else { i })),
        "pshufhw" => each(16, |i| lane(b, 16, if i < 4 { i } else { 4 + field(i) })),
        "shufps" => each(32, |i| lane(if i < 2 { a } else { b }, 32, field(i))),
        "shufpd" => each(64, |i| {
            lane(if i < 1 { a } else { b }, 64, u32::from(imm >> i & 1))
        }),
        _ => packed(op, a, u128::from(imm)),
    }
}

/// Runs one XMM1, XMM2 instruction in both engines and compares the whole state.
fn expect_vector(bench: &mut Bench, what: &str, cpu: &CpuState, wanted: &CpuState) {
    for engine in [Engine::Interpreter, Engine::Dbt] {
        let (mut got, exit) = match engine {
            Engine::Interpreter => run(engine, cpu, &mut bench.memory),
            _ => run_dbt(&mut bench.dbt, cpu, &mut bench.memory, BUDGET),
        };
        assert_eq!(exit, ExitReason::Halt, "{what} on {engine:?}");
        got.rip = wanted.rip;
        if got != *wanted {
            panic!(
                "{what} on {engine:?}: xmm1={:032x} xmm2={:032x} mxcsr={:x}\n got xmm1={:032x} rax={:x} mxcsr={:x} flags={}\nwant xmm1={:032x} rax={:x} mxcsr={:x} flags={}",
                cpu.xmm[1],
                cpu.xmm[2],
                cpu.mxcsr.0,
                got.xmm[1],
                got.gpr[0],
                got.mxcsr.0,
                show_flags(get_flags(&got.flags), ALL_FLAGS),
                wanted.xmm[1],
                wanted.gpr[0],
                wanted.mxcsr.0,
                show_flags(get_flags(&wanted.flags), ALL_FLAGS),
            );
        }
        bench.runs += 1;
    }
}

#[test]
fn packed_integer_sweep_matches_lane_definitions() {
    let mut runs = 0;
    let mut random = Random(0x155);
    for (op, distinct, aliased) in PACKED {
        let is_shift = op.starts_with("psll") || op.starts_with("psrl") || op.starts_with("psra");
        for (hex, alias) in [(distinct, false), (aliased, true)] {
            let mut bench = Bench::new(hex);
            for round in 0..400_u64 {
                let a = random.vector();
                let mut b = random.vector();
                if is_shift && round % 4 != 0 {
                    b = b >> 64 << 64 | u128::from(round % 70);
                }
                let mut cpu = initial_cpu();
                (cpu.xmm[1], cpu.xmm[2]) = (a, b);
                let mut wanted = cpu.clone();
                wanted.xmm[1] = packed(op, a, if alias { a } else { b });
                expect_vector(&mut bench, hex, &cpu, &wanted);
            }
            runs += bench.runs;
        }
    }
    for (op, hex) in PACKED_IMMEDIATE {
        for imm in 0..=0xff_u8 {
            let mut bench = Bench::new(&format!("{hex}{imm:02x}"));
            for _ in 0..4 {
                let mut cpu = initial_cpu();
                (cpu.xmm[1], cpu.xmm[2]) = (random.vector(), random.vector());
                let mut wanted = cpu.clone();
                wanted.xmm[1] = packed_immediate(op, cpu.xmm[1], cpu.xmm[2], imm);
                expect_vector(&mut bench, &format!("{op} {imm}"), &cpu, &wanted);
            }
            runs += bench.runs;
        }
    }
    println!("{runs} engine runs");
}

const PACKED: &[(&str, &str, &str)] = &[
    ("paddb", "660ffcca", "660ffcc9"),
    ("paddw", "660ffdca", "660ffdc9"),
    ("paddd", "660ffeca", "660ffec9"),
    ("paddq", "660fd4ca", "660fd4c9"),
    ("psubb", "660ff8ca", "660ff8c9"),
    ("psubw", "660ff9ca", "660ff9c9"),
    ("psubd", "660ffaca", "660ffac9"),
    ("psubq", "660ffbca", "660ffbc9"),
    ("paddsb", "660fecca", "660fecc9"),
    ("paddsw", "660fedca", "660fedc9"),
    ("paddusb", "660fdcca", "660fdcc9"),
    ("paddusw", "660fddca", "660fddc9"),
    ("psubsb", "660fe8ca", "660fe8c9"),
    ("psubsw", "660fe9ca", "660fe9c9"),
    ("psubusb", "660fd8ca", "660fd8c9"),
    ("psubusw", "660fd9ca", "660fd9c9"),
    ("pmullw", "660fd5ca", "660fd5c9"),
    ("pmulhw", "660fe5ca", "660fe5c9"),
    ("pmulhuw", "660fe4ca", "660fe4c9"),
    ("pmuludq", "660ff4ca", "660ff4c9"),
    ("pmaddwd", "660ff5ca", "660ff5c9"),
    ("psadbw", "660ff6ca", "660ff6c9"),
    ("pavgb", "660fe0ca", "660fe0c9"),
    ("pavgw", "660fe3ca", "660fe3c9"),
    ("pminub", "660fdaca", "660fdac9"),
    ("pmaxub", "660fdeca", "660fdec9"),
    ("pminsw", "660feaca", "660feac9"),
    ("pmaxsw", "660feeca", "660feec9"),
    ("pcmpeqb", "660f74ca", "660f74c9"),
    ("pcmpeqw", "660f75ca", "660f75c9"),
    ("pcmpeqd", "660f76ca", "660f76c9"),
    ("pcmpgtb", "660f64ca", "660f64c9"),
    ("pcmpgtw", "660f65ca", "660f65c9"),
    ("pcmpgtd", "660f66ca", "660f66c9"),
    ("pand", "660fdbca", "660fdbc9"),
    ("pandn", "660fdfca", "660fdfc9"),
    ("por", "660febca", "660febc9"),
    ("pxor", "660fefca", "660fefc9"),
    ("psllw", "660ff1ca", "660ff1c9"),
    ("pslld", "660ff2ca", "660ff2c9"),
    ("psllq", "660ff3ca", "660ff3c9"),
    ("psrlw", "660fd1ca", "660fd1c9"),
    ("psrld", "660fd2ca", "660fd2c9"),
    ("psrlq", "660fd3ca", "660fd3c9"),
    ("psraw", "660fe1ca", "660fe1c9"),
    ("psrad", "660fe2ca", "660fe2c9"),
    ("packsswb", "660f63ca", "660f63c9"),
    ("packssdw", "660f6bca", "660f6bc9"),
    ("packuswb", "660f67ca", "660f67c9"),
    ("punpcklbw", "660f60ca", "660f60c9"),
    ("punpcklwd", "660f61ca", "660f61c9"),
    ("punpckldq", "660f62ca", "660f62c9"),
    ("punpcklqdq", "660f6cca", "660f6cc9"),
    ("punpckhbw", "660f68ca", "660f68c9"),
    ("punpckhwd", "660f69ca", "660f69c9"),
    ("punpckhdq", "660f6aca", "660f6ac9"),
    ("punpckhqdq", "660f6dca", "660f6dc9"),
];
const PACKED_IMMEDIATE: &[(&str, &str)] = &[
    ("psllw", "660f71f1"),
    ("pslld", "660f72f1"),
    ("psllq", "660f73f1"),
    ("psrlw", "660f71d1"),
    ("psrld", "660f72d1"),
    ("psrlq", "660f73d1"),
    ("psraw", "660f71e1"),
    ("psrad", "660f72e1"),
    ("pslldq", "660f73f9"),
    ("psrldq", "660f73d9"),
    ("pshufd", "660f70ca"),
    ("pshuflw", "f20f70ca"),
    ("pshufhw", "f30f70ca"),
    ("shufps", "0fc6ca"),
    ("shufpd", "660fc6ca"),
];
const FLOAT: &[(&str, [&str; 2])] = &[
    ("add", ["f30f58ca", "f20f58ca"]),
    ("sub", ["f30f5cca", "f20f5cca"]),
    ("mul", ["f30f59ca", "f20f59ca"]),
    ("div", ["f30f5eca", "f20f5eca"]),
    ("sqrt", ["f30f51ca", "f20f51ca"]),
    ("min", ["f30f5dca", "f20f5dca"]),
    ("max", ["f30f5fca", "f20f5fca"]),
];

// --- multiply and divide -----------------------------------------------------

const PRODUCTS: &[(&str, [&str; 4])] = &[
    ("mul", ["f6e3", "66f7e3", "f7e3", "48f7e3"]),
    ("imul", ["f6eb", "66f7eb", "f7eb", "48f7eb"]),
    ("div", ["f6f3", "66f7f3", "f7f3", "48f7f3"]),
    ("idiv", ["f6fb", "66f7fb", "f7fb", "48f7fb"]),
];

/// RAX and RDX after a double-width result (AX, DX:AX, EDX:EAX or RDX:RAX).
fn write_pair(cpu: &mut CpuState, low: u64, high: u64, bits: u32) {
    if bits == 8 {
        cpu.gpr[0] = cpu.gpr[0] & !0xffff | low | high << 8;
    } else {
        cpu.gpr[0] = written(cpu.gpr[0], low, bits);
        cpu.gpr[2] = written(cpu.gpr[2], high, bits);
    }
}

#[test]
fn multiply_and_divide_sweep_matches_exact_arithmetic() {
    let mut runs = 0;
    for (op, forms) in PRODUCTS {
        for (hex, bits) in forms.iter().zip(WIDTHS) {
            let mut bench = Bench::new(hex);
            let values = boundary_values(bits);
            let highs: &[u64] = if op.ends_with("div") { &values } else { &[3] };
            for &high in highs {
                for &low in &values {
                    for &source in &values {
                        let mut cpu = initial_cpu();
                        cpu.gpr[3] = dirty(source, bits);
                        cpu.gpr[0] = dirty(low, bits);
                        cpu.gpr[2] = dirty(high, bits);
                        if bits == 8 {
                            cpu.gpr[0] = dirty(low, 16) & !0xff00 | (high & 0xff) << 8;
                        }
                        check_multiply_divide(&mut bench, op, &cpu, [high, low, source], bits);
                    }
                }
            }
            runs += bench.runs;
        }
    }
    println!("{runs} engine runs");
}

/// `None` is #DE: a zero divisor or a quotient outside the destination
/// (SDM 2A DIV, IDIV and Table 3-53).
fn divide(op: &str, [high, low, source]: [u64; 3], bits: u32) -> Option<(u64, u64)> {
    if source == 0 {
        return None;
    }
    let dividend = u128::from(high) << bits | u128::from(low);
    if op == "div" {
        let quotient = dividend / u128::from(source);
        let remainder = dividend % u128::from(source);
        return (quotient <= u128::from(mask(bits))).then_some((quotient as u64, remainder as u64));
    }
    let spare = 128 - 2 * bits;
    let dividend = (dividend << spare) as i128 >> spare;
    let divisor = signed(source, bits);
    let quotient = dividend.checked_div(divisor)?;
    let remainder = dividend % divisor;
    let limit = 1_i128 << (bits - 1);
    (-limit..limit)
        .contains(&quotient)
        .then_some((quotient as u64 & mask(bits), remainder as u64 & mask(bits)))
}

fn check_multiply_divide(bench: &mut Bench, op: &str, cpu: &CpuState, inputs: [u64; 3], bits: u32) {
    let [_, low, source] = inputs;
    let mut wanted = cpu.clone();
    let mut carry = None;
    let mut exit_wanted = ExitReason::Halt;
    match op {
        "mul" => {
            let product = u128::from(low) * u128::from(source);
            let (product_low, product_high) =
                (product as u64 & mask(bits), (product >> bits) as u64);
            write_pair(&mut wanted, product_low, product_high, bits);
            carry = Some(product_high != 0);
        }
        "imul" => {
            let product = signed(low, bits) * signed(source, bits);
            let product_low = product as u64 & mask(bits);
            let product_high = (product >> bits) as u64 & mask(bits);
            write_pair(&mut wanted, product_low, product_high, bits);
            carry = Some(signed(product_low, bits) != product);
        }
        _ => match divide(op, inputs, bits) {
            Some((quotient, remainder)) => {
                write_pair(&mut wanted, quotient, remainder, bits);
            }
            None => exit_wanted = ExitReason::DivideError,
        },
    }
    for engine in [Engine::Interpreter, Engine::Dbt] {
        let (mut got, exit) = match engine {
            Engine::Interpreter => run(engine, cpu, &mut bench.memory),
            _ => run_dbt(&mut bench.dbt, cpu, &mut bench.memory, BUDGET),
        };
        let what = format!("{op}{bits} {inputs:x?} on {engine:?}");
        assert_eq!(exit, exit_wanted, "{what}");
        if let Some(carry) = carry {
            assert_eq!((got.flags.cf, got.flags.of), (carry, carry), "{what}");
        }
        if exit_wanted == ExitReason::Halt {
            // MUL leaves SF, ZF, AF, PF undefined; DIV leaves all six.
            got.flags = wanted.flags;
            got.rip = wanted.rip;
        }
        assert_eq!(got, wanted, "{what}");
        bench.runs += 1;
    }
}

// --- floating point ------------------------------------------------------------

const IE: u32 = 1;
const DE: u32 = 2;
const ZE: u32 = 4;
const OE: u32 = 8;
const UE: u32 = 16;
const PE: u32 = 32;
const DAZ: u32 = 0x40;
const FTZ: u32 = 0x8000;
const DM: u32 = 0x100;
const OM: u32 = 0x400;
const UM: u32 = 0x800;
const MASKS: u32 = 0x1f80;

/// Binary32 or binary64 (SDM 1 4.2.2).
#[derive(Clone, Copy)]
struct Format {
    bits: u32,
    precision: u32,
}

const SINGLE: Format = Format {
    bits: 32,
    precision: 24,
};
const DOUBLE: Format = Format {
    bits: 64,
    precision: 53,
};

impl Format {
    fn fraction_bits(self) -> u32 {
        self.precision - 1
    }
    fn max_exponent(self) -> i32 {
        (1 << (self.bits - self.precision - 1)) - 1
    }
    fn min_exponent(self) -> i32 {
        1 - self.max_exponent()
    }
    fn sign(self) -> u64 {
        1 << (self.bits - 1)
    }
    fn infinity(self) -> u64 {
        mask(self.bits - 1) & !mask(self.fraction_bits())
    }
    fn quiet(self) -> u64 {
        1 << (self.fraction_bits() - 1)
    }
    fn indefinite(self) -> u64 {
        self.sign() | self.infinity() | self.quiet()
    }
    fn magnitude(self, value: u64) -> u64 {
        value & !self.sign()
    }
    fn negative(self, value: u64) -> bool {
        value & self.sign() != 0
    }
    fn signed(self, negative: bool, magnitude: u64) -> u64 {
        magnitude | if negative { self.sign() } else { 0 }
    }
    fn is_nan(self, value: u64) -> bool {
        self.magnitude(value) > self.infinity()
    }
    fn is_snan(self, value: u64) -> bool {
        self.is_nan(value) && value & self.quiet() == 0
    }
    fn is_infinite(self, value: u64) -> bool {
        self.magnitude(value) == self.infinity()
    }
    fn is_zero(self, value: u64) -> bool {
        self.magnitude(value) == 0
    }
    fn is_denormal(self, value: u64) -> bool {
        let magnitude = self.magnitude(value);
        magnitude != 0 && magnitude <= mask(self.fraction_bits())
    }
    /// Finite value as significand times a power of two.
    fn split(self, value: u64) -> (u128, i32) {
        let magnitude = self.magnitude(value);
        let field = (magnitude >> self.fraction_bits()) as i32;
        let fraction = u128::from(magnitude & mask(self.fraction_bits()));
        let lowest = self.min_exponent() - self.fraction_bits() as i32;
        if field == 0 {
            (fraction, lowest)
        } else {
            (fraction | 1 << self.fraction_bits(), lowest + field - 1)
        }
    }
    fn values(self) -> Vec<u64> {
        let one = (self.max_exponent() as u64) << self.fraction_bits();
        let tiny = 1 << self.fraction_bits();
        let infinity = self.infinity();
        let mut values = vec![
            0,
            1,
            2,
            tiny / 2,
            tiny - 1,
            tiny,
            tiny + 1,
            2 * tiny - 1,
            one - tiny,
            one - 1,
            one,
            one + 1,
            one + tiny / 2,
            one + tiny - 1,
            one + tiny,
            one + tiny + tiny / 2 + 1,
            one + u64::from(self.precision) * tiny,
            one - u64::from(self.precision) * tiny,
            infinity - tiny - 1,
            infinity - 1,
            infinity,
            infinity | self.quiet() | 5,
            infinity | 9,
        ];
        let negative: Vec<u64> = values.iter().map(|value| value | self.sign()).collect();
        values.extend(negative);
        values
    }
}

/// What the manual requires: the result, the flags, and the flags it leaves open.
#[derive(Clone, Copy)]
struct Outcome {
    result: u64,
    flags: u32,
    open: u32,
}

fn outcome(result: u64, flags: u32) -> Outcome {
    Outcome {
        result,
        flags,
        open: 0,
    }
}

/// `significand * 2^exponent`, where `sticky` adds a nonzero amount below the
/// last bit, rounded to the multiple of `2^lowest` the mode selects (SDM 1 4.8.4
/// Table 4-9). Returns the multiple and whether it is exact.
fn round_to(
    significand: u128,
    exponent: i32,
    sticky: bool,
    lowest: i32,
    negative: bool,
    mode: u32,
) -> (u128, bool) {
    let drop = lowest - exponent;
    if drop <= 0 {
        assert!(!sticky, "too few quotient bits");
        return (significand << -drop, true);
    }
    let (kept, rest, half) = if drop >= 127 {
        (0, significand, 1 << 126 | 1) // Every significand is far below half.
    } else {
        let kept = significand >> drop;
        (kept, significand - (kept << drop), 1 << (drop - 1))
    };
    if rest == 0 && !sticky {
        return (kept, true);
    }
    let up = match mode {
        0 => rest > half || rest == half && (sticky || kept & 1 == 1),
        1 => negative,
        2 => !negative,
        _ => false,
    };
    (kept + u128::from(up), false)
}

/// Deliver a nonzero finite exact value (SDM 1 4.9.1.4 to 4.9.1.6, 10.2.3.3).
fn deliver(
    format: Format,
    negative: bool,
    significand: u128,
    exponent: i32,
    sticky: bool,
    mxcsr: u32,
) -> Outcome {
    let mode = mxcsr >> 13 & 3;
    let precision = format.precision as i32;
    let leading = 127 - significand.leading_zeros() as i32 + exponent;
    let lowest = leading - precision + 1;
    let (unbounded, unbounded_exact) =
        round_to(significand, exponent, sticky, lowest, negative, mode);
    let carried = unbounded >> precision != 0;
    let rounded_exponent = leading + i32::from(carried);
    let tiny = rounded_exponent < format.min_exponent();
    let floor = format.min_exponent() - precision + 1;
    let lowest = lowest.max(floor);
    let (mut value, exact) = round_to(significand, exponent, sticky, lowest, negative, mode);
    let mut lowest = lowest;
    if value >> precision != 0 {
        value >>= 1;
        lowest += 1;
    }
    let inexact = if exact { 0 } else { PE };
    // Unmasked overflow and underflow set PE only if rounding to the destination
    // precision with an unbounded exponent is inexact (D.4.2.3 Tables D-16, D-17).
    let unbounded_inexact = if unbounded_exact { 0 } else { PE };
    if mxcsr & UM == 0 && tiny {
        // When only denormalizing would lose bits, D-17 leaves PE clear while
        // AMD APM 1 4.10.2.8 can be read as setting it.
        return Outcome {
            open: if unbounded_exact && !exact { PE } else { 0 },
            ..outcome(0, UE | unbounded_inexact)
        };
    }
    if mxcsr & FTZ != 0 && tiny {
        return outcome(format.signed(negative, 0), UE | PE);
    }
    if lowest + precision - 1 > format.max_exponent() {
        if mxcsr & OM == 0 {
            return outcome(0, OE | unbounded_inexact);
        }
        let to_infinity = match mode {
            0 => true,
            1 => negative,
            2 => !negative,
            _ => false,
        };
        let magnitude = format.infinity() - u64::from(!to_infinity);
        return outcome(format.signed(negative, magnitude), OE | PE);
    }
    let field = (lowest - floor) as u64;
    let magnitude = (field << format.fraction_bits()) + value as u64;
    let underflow = if tiny && !exact { UE } else { 0 };
    outcome(format.signed(negative, magnitude), inexact | underflow)
}

fn square_root(value: u128) -> u128 {
    let mut root = 0_u128;
    for bit in (0..64).rev() {
        let next = root | 1 << bit;
        if next.checked_mul(next).is_some_and(|square| square <= value) {
            root = next;
        }
    }
    root
}

/// ADD, SUB, MUL, DIV, SQRT, MIN and MAX of one lane.
fn float(op: &str, format: Format, a: u64, b: u64, mxcsr: u32) -> Outcome {
    let unary = op == "sqrt";
    let flush = |value: u64| {
        if mxcsr & DAZ != 0 && format.is_denormal(value) {
            value & format.sign()
        } else {
            value
        }
    };
    let (a, b) = (flush(a), flush(b));
    let denormal = if format.is_denormal(b) || !unary && format.is_denormal(a) {
        DE
    } else {
        0
    };
    if matches!(op, "min" | "max") {
        // SDM 2B MINSS/MAXSS and 1 11.5.2.1 Table 11-1.
        if format.is_nan(a) || format.is_nan(b) {
            return outcome(b, IE);
        }
        let order = |value: u64| {
            let magnitude = i128::from(format.magnitude(value));
            if format.negative(value) {
                -magnitude
            } else {
                magnitude
            }
        };
        let take_first = if op == "min" {
            order(a) < order(b)
        } else {
            order(a) > order(b)
        };
        return outcome(if take_first { a } else { b }, denormal);
    }
    // SDM 1 4.8.3.5 Table 4-8 and D.4.2.2 Tables D-1 and D-10. NaN operands come
    // before a denormal operand in the priority list of 4.9.2.
    let operands: &[u64] = if unary { &[b] } else { &[a, b] };
    if let Some(&nan) = operands.iter().find(|&&value| format.is_nan(value)) {
        let signaling = operands.iter().any(|&value| format.is_snan(value));
        return outcome(nan | format.quiet(), if signaling { IE } else { 0 });
    }
    let invalid = outcome(format.indefinite(), IE);
    let sign_a = format.negative(a);
    let sign_b = format.negative(b) != (op == "sub");
    let product_sign = format.negative(a) != format.negative(b);
    let (infinite_a, infinite_b) = (format.is_infinite(a), format.is_infinite(b));
    let (zero_a, zero_b) = (format.is_zero(a), format.is_zero(b));
    let with_denormal = |mut result: Outcome| {
        // An unmasked pre-computation exception hides later ones (11.5.3.2).
        if mxcsr & DM == 0 && denormal != 0 {
            result.flags = 0;
            result.open = 0;
        }
        result.flags |= denormal;
        result
    };
    let (ma, ea) = format.split(a);
    let (mb, eb) = format.split(b);
    match op {
        "add" | "sub" => {
            if infinite_a && infinite_b && sign_a != sign_b {
                return invalid;
            }
            if infinite_a || infinite_b {
                let negative = if infinite_a { sign_a } else { sign_b };
                return with_denormal(outcome(format.signed(negative, format.infinity()), 0));
            }
            // Keep 60 guard bits; anything further down is sticky.
            let (high, low) = if (mb == 0 || ea >= eb) && ma != 0 {
                ((ma, ea, sign_a), (mb, eb, sign_b))
            } else {
                ((mb, eb, sign_b), (ma, ea, sign_a))
            };
            let (guarded, base) = (high.0 << 60, high.1 - 60);
            let distance = low.1 - base;
            let (aligned, lost) = if low.0 == 0 {
                (0, false)
            } else if distance >= 0 {
                (low.0 << distance, false)
            } else if distance > -127 {
                let kept = low.0 >> -distance;
                (kept, kept << -distance != low.0)
            } else {
                (0, true)
            };
            let (negative, sum, sticky) = if high.2 == low.2 {
                (high.2, guarded + aligned, lost)
            } else if guarded >= aligned {
                // Borrow the sticky remainder from the exact difference.
                (high.2, guarded - aligned - u128::from(lost), lost)
            } else {
                assert!(!lost);
                (low.2, aligned - guarded, false)
            };
            if sum == 0 && !sticky {
                let negative = if zero_a && zero_b && sign_a == sign_b {
                    sign_a
                } else {
                    mxcsr >> 13 & 3 == 1
                };
                return with_denormal(outcome(format.signed(negative, 0), 0));
            }
            with_denormal(deliver(format, negative, sum, base, sticky, mxcsr))
        }
        "mul" => {
            if infinite_a && zero_b || zero_a && infinite_b {
                return invalid;
            }
            if infinite_a || infinite_b {
                return with_denormal(outcome(format.signed(product_sign, format.infinity()), 0));
            }
            if zero_a || zero_b {
                return with_denormal(outcome(format.signed(product_sign, 0), 0));
            }
            with_denormal(deliver(
                format,
                product_sign,
                ma * mb,
                ea + eb,
                false,
                mxcsr,
            ))
        }
        "div" => {
            if infinite_a && infinite_b || zero_a && zero_b {
                return invalid;
            }
            if infinite_a {
                return with_denormal(outcome(format.signed(product_sign, format.infinity()), 0));
            }
            if zero_b {
                return Outcome {
                    result: format.signed(product_sign, format.infinity()),
                    flags: ZE,
                    open: DE,
                };
            }
            if infinite_b || zero_a {
                return with_denormal(outcome(format.signed(product_sign, 0), 0));
            }
            let up = ma.leading_zeros() as i32 - 2;
            let dividend = ma << up;
            let exact = dividend.is_multiple_of(mb);
            with_denormal(deliver(
                format,
                product_sign,
                dividend / mb,
                ea - up - eb,
                !exact,
                mxcsr,
            ))
        }
        "sqrt" => {
            if zero_b {
                return outcome(b, 0);
            }
            if format.negative(b) {
                return Outcome {
                    open: DE,
                    ..invalid
                };
            }
            if infinite_b {
                return outcome(b, 0);
            }
            let mut up = mb.leading_zeros() as i32 - 2;
            up -= (eb - up) & 1;
            let radicand = mb << up;
            let root = square_root(radicand);
            with_denormal(deliver(
                format,
                false,
                root,
                (eb - up) / 2,
                root * root != radicand,
                mxcsr,
            ))
        }
        _ => panic!("{op}"),
    }
}

/// CVTSD2SS and CVTSS2SD (SDM 1 D.4.2.2 Tables D-11 and D-12).
fn convert_format(from: Format, to: Format, value: u64, mxcsr: u32) -> Outcome {
    if from.is_nan(value) {
        let fraction = value & mask(from.fraction_bits());
        let moved = if to.bits > from.bits {
            fraction << (to.fraction_bits() - from.fraction_bits())
        } else {
            fraction >> (from.fraction_bits() - to.fraction_bits())
        };
        let result = to.signed(from.negative(value), to.infinity() | to.quiet() | moved);
        return outcome(result, if from.is_snan(value) { IE } else { 0 });
    }
    let negative = from.negative(value);
    if from.is_infinite(value) {
        return outcome(to.signed(negative, to.infinity()), 0);
    }
    let denormal = from.is_denormal(value);
    if from.is_zero(value) || denormal && mxcsr & DAZ != 0 {
        return outcome(to.signed(negative, 0), 0);
    }
    let (significand, exponent) = from.split(value);
    let mut result = deliver(to, negative, significand, exponent, false, mxcsr);
    if denormal {
        if mxcsr & DM == 0 {
            (result.flags, result.open) = (0, 0);
        }
        result.flags |= DE;
    }
    result
}

/// CVT(T)SS2SI and CVT(T)SD2SI (SDM 2A; no denormal exception, 1 11.5.2.2).
fn convert_integer(format: Format, value: u64, bits: u32, mode: u32, daz: bool) -> Outcome {
    let indefinite = outcome(1 << (bits - 1), IE);
    if format.is_nan(value) || format.is_infinite(value) {
        return indefinite;
    }
    if format.is_zero(value) || daz && format.is_denormal(value) {
        return outcome(0, 0);
    }
    let negative = format.negative(value);
    let (significand, exponent) = format.split(value);
    if exponent > 64 {
        return indefinite;
    }
    let (magnitude, exact) = round_to(significand, exponent, false, 0, negative, mode);
    let limit = 1_u128 << (bits - 1);
    if magnitude > limit - u128::from(!negative) {
        return indefinite;
    }
    let result = if negative {
        (magnitude as u64).wrapping_neg()
    } else {
        magnitude as u64
    };
    outcome(result & mask(bits), if exact { 0 } else { PE })
}

/// Whether the manual requires #XM for these flags under `mxcsr`.
fn must_fault(flags: u32, mxcsr: u32) -> bool {
    flags & !(mxcsr >> 7) & 0x3f != 0
}

/// Compares both engines with `want` for an instruction writing XMM1 or RAX.
fn expect_float(
    bench: &mut Bench,
    what: &str,
    cpu: &CpuState,
    want: Outcome,
    write: impl Fn(&mut CpuState, u64),
) {
    let faults = must_fault(want.flags, cpu.mxcsr.0);
    if !faults && must_fault(want.open, cpu.mxcsr.0) {
        return; // The manual leaves open whether this raises #XM.
    }
    let mut wanted = cpu.clone();
    if !faults {
        write(&mut wanted, want.result);
    }
    let flags = cpu.mxcsr.0 | want.flags;
    for engine in [Engine::Interpreter, Engine::Dbt] {
        let (mut got, exit) = match engine {
            Engine::Interpreter => run(engine, cpu, &mut bench.memory),
            _ => run_dbt(&mut bench.dbt, cpu, &mut bench.memory, BUDGET),
        };
        let exit_wanted = if faults {
            ExitReason::SimdException
        } else {
            ExitReason::Halt
        };
        let seen = got.mxcsr.0;
        if !faults {
            got.rip = wanted.rip;
        }
        got.mxcsr = wanted.mxcsr;
        if exit != exit_wanted || (seen ^ flags) & !want.open != 0 || got != wanted {
            panic!(
                "{what} on {engine:?}: xmm1={:x} xmm2={:x} rax={:x} mxcsr={:x}\n got {exit:?} xmm1={:x} rax={:x} rip={:x} mxcsr={seen:x}\nwant {exit_wanted:?} xmm1={:x} rax={:x} rip={:x} mxcsr={flags:x} (open {:x})",
                cpu.xmm[1],
                cpu.xmm[2],
                cpu.gpr[0],
                cpu.mxcsr.0,
                got.xmm[1],
                got.gpr[0],
                got.rip,
                wanted.xmm[1],
                wanted.gpr[0],
                wanted.rip,
                want.open
            );
        }
        bench.runs += 1;
    }
}

fn low(format: Format, base: u128, value: u64) -> u128 {
    base & !u128::from(mask(format.bits)) | u128::from(value)
}

/// Every rounding mode with and without FTZ and DAZ, all masked and all unmasked,
/// and each exception unmasked alone.
fn mxcsr_values() -> Vec<u32> {
    let mut values = Vec::new();
    for mode in 0..4 {
        for flush in [0, FTZ, DAZ, FTZ | DAZ] {
            for masks in [MASKS, 0] {
                values.push(masks | mode << 13 | flush);
            }
        }
        for flush in [0, FTZ | DAZ] {
            for only in 7..13 {
                values.push(MASKS & !(1 << only) | mode << 13 | flush);
            }
        }
    }
    values
}

#[test]
fn scalar_float_sweep_matches_exact_rounding() {
    let mut runs = 0;
    for (op, forms) in FLOAT {
        for (hex, format) in forms.iter().zip([SINGLE, DOUBLE]) {
            let mut bench = Bench::new(hex);
            for a in format.values() {
                for b in format.values() {
                    for mxcsr in mxcsr_values() {
                        let mut cpu = initial_cpu();
                        cpu.mxcsr = Mxcsr(mxcsr);
                        cpu.xmm[1] = low(format, cpu.xmm[1], a);
                        cpu.xmm[2] = low(format, cpu.xmm[2], b);
                        let want = float(op, format, a, b, mxcsr);
                        let what = format!("{op} {}", format.bits);
                        expect_float(&mut bench, &what, &cpu, want, |cpu, result| {
                            cpu.xmm[1] = low(format, cpu.xmm[1], result);
                        });
                    }
                }
            }
            runs += bench.runs;
        }
    }
    println!("{runs} engine runs");
}

#[test]
fn float_conversion_sweep_matches_exact_rounding() {
    const TO_INTEGER: [(&str, Format, u32, bool); 8] = [
        ("f30f2dc2", SINGLE, 32, false),
        ("f3480f2dc2", SINGLE, 64, false),
        ("f20f2dc2", DOUBLE, 32, false),
        ("f2480f2dc2", DOUBLE, 64, false),
        ("f30f2cc2", SINGLE, 32, true),
        ("f3480f2cc2", SINGLE, 64, true),
        ("f20f2cc2", DOUBLE, 32, true),
        ("f2480f2cc2", DOUBLE, 64, true),
    ];
    const FROM_INTEGER: [(&str, Format, u32); 4] = [
        ("f30f2ac8", SINGLE, 32),
        ("f3480f2ac8", SINGLE, 64),
        ("f20f2ac8", DOUBLE, 32),
        ("f2480f2ac8", DOUBLE, 64),
    ];
    let mut runs = 0;
    for (hex, from, to) in [("f20f5aca", DOUBLE, SINGLE), ("f30f5aca", SINGLE, DOUBLE)] {
        let mut bench = Bench::new(hex);
        let mut values = from.values();
        // Doubles around the ends of the single range and its halfway points.
        if from.bits == 64 {
            for exponent in [-151, -150, -149, -127, -126, 0, 127, 128] {
                let power = ((1023 + exponent) as u64) << 52;
                for offset in [
                    0,
                    1,
                    1 << 28,
                    1 << 29,
                    3 << 28,
                    mask(52),
                    mask(52) - mask(28),
                ] {
                    values.extend([power + offset, (power + offset) | (1 << 63)]);
                }
            }
        }
        for value in values {
            for mxcsr in mxcsr_values() {
                let mut cpu = initial_cpu();
                cpu.mxcsr = Mxcsr(mxcsr);
                cpu.xmm[2] = low(from, cpu.xmm[2], value);
                let want = convert_format(from, to, value, mxcsr);
                expect_float(&mut bench, hex, &cpu, want, |cpu, result| {
                    cpu.xmm[1] = low(to, cpu.xmm[1], result);
                });
            }
        }
        runs += bench.runs;
    }
    for (hex, format, bits, truncate) in TO_INTEGER {
        let mut bench = Bench::new(hex);
        let mut values = format.values();
        let bias = format.max_exponent();
        for exponent in [-1, 0, 1, 30, 31, 32, 51, 52, 62, 63, 64] {
            let power = ((bias + exponent) as u64) << format.fraction_bits();
            for offset in [0, 1, mask(format.fraction_bits()), format.quiet()] {
                values.extend([power + offset, (power + offset) | format.sign()]);
            }
            values.extend([power - 1, (power - 1) | format.sign()]);
        }
        for value in values {
            for mxcsr in mxcsr_values() {
                let mut cpu = initial_cpu();
                cpu.mxcsr = Mxcsr(mxcsr);
                cpu.xmm[2] = low(format, cpu.xmm[2], value);
                let mode = if truncate { 3 } else { mxcsr >> 13 & 3 };
                let want = convert_integer(format, value, bits, mode, mxcsr & DAZ != 0);
                expect_float(&mut bench, hex, &cpu, want, |cpu, result| {
                    cpu.gpr[0] = result;
                });
            }
        }
        runs += bench.runs;
    }
    for (hex, format, bits) in FROM_INTEGER {
        let mut bench = Bench::new(hex);
        let mut values = boundary_values(bits);
        for shift in 20..bits {
            for offset in [0, 1, 2, 3] {
                let value = (1_u64 << shift | offset) & mask(bits);
                values.extend([
                    value,
                    value.wrapping_neg() & mask(bits),
                    !value & mask(bits),
                ]);
            }
        }
        for value in values {
            for mxcsr in mxcsr_values() {
                let mut cpu = initial_cpu();
                cpu.mxcsr = Mxcsr(mxcsr);
                cpu.gpr[0] = dirty(value, bits);
                let exact = signed(value, bits);
                let want = if exact == 0 {
                    outcome(0, 0)
                } else {
                    deliver(format, exact < 0, exact.unsigned_abs(), 0, false, mxcsr)
                };
                expect_float(&mut bench, hex, &cpu, want, |cpu, result| {
                    cpu.xmm[1] = low(format, cpu.xmm[1], result);
                });
            }
        }
        runs += bench.runs;
    }
    println!("{runs} engine runs");
}

/// COMISx, UCOMISx and all eight CMPSx predicates.
#[test]
fn float_comparison_sweep_matches_the_manual() {
    const ORDERED: [(&str, Format, bool); 4] = [
        ("0f2fca", SINGLE, true),
        ("0f2eca", SINGLE, false),
        ("660f2fca", DOUBLE, true),
        ("660f2eca", DOUBLE, false),
    ];
    let mut runs = 0;
    let mut programs: Vec<(String, Format, bool, Option<u8>)> = ORDERED
        .iter()
        .map(|&(hex, format, signals)| (hex.to_string(), format, signals, None))
        .collect();
    for predicate in 0..8 {
        // LT, LE, NLT and NLE signal on quiet NaNs (SDM 1 11.5.2.1).
        let signals = matches!(predicate, 1 | 2 | 5 | 6);
        for (hex, format) in [("f30fc2ca", SINGLE), ("f20fc2ca", DOUBLE)] {
            let hex = format!("{hex}{predicate:02x}");
            programs.push((hex, format, signals, Some(predicate)));
        }
    }
    for (hex, format, signals, predicate) in programs {
        let mut bench = Bench::new(&hex);
        for a in format.values() {
            for b in format.values() {
                for mxcsr in mxcsr_values() {
                    let mut cpu = initial_cpu();
                    cpu.mxcsr = Mxcsr(mxcsr);
                    cpu.xmm[1] = low(format, cpu.xmm[1], a);
                    cpu.xmm[2] = low(format, cpu.xmm[2], b);
                    set_flags(&mut cpu.flags, OF | SF | AF);
                    let (flags, relation) = compare(format, a, b, signals, mxcsr);
                    let want = outcome(0, flags);
                    expect_float(&mut bench, &hex, &cpu, want, |cpu, _| match predicate {
                        None => set_flags(
                            &mut cpu.flags,
                            match relation {
                                Relation::Unordered => ZF | PF | CF,
                                Relation::Greater => 0,
                                Relation::Less => CF,
                                Relation::Equal => ZF,
                            },
                        ),
                        Some(predicate) => {
                            let base = match predicate & 3 {
                                0 => relation == Relation::Equal,
                                1 => relation == Relation::Less,
                                2 => matches!(relation, Relation::Less | Relation::Equal),
                                _ => relation == Relation::Unordered,
                            };
                            let yes = base != (predicate >= 4);
                            let lane = if yes { mask(format.bits) } else { 0 };
                            cpu.xmm[1] = low(format, cpu.xmm[1], lane);
                        }
                    });
                }
            }
        }
        runs += bench.runs;
    }
    println!("{runs} engine runs");
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Relation {
    Unordered,
    Less,
    Equal,
    Greater,
}

fn compare(format: Format, a: u64, b: u64, signals: bool, mxcsr: u32) -> (u32, Relation) {
    if format.is_nan(a) || format.is_nan(b) {
        let invalid = signals || format.is_snan(a) || format.is_snan(b);
        return (if invalid { IE } else { 0 }, Relation::Unordered);
    }
    let daz = mxcsr & DAZ != 0;
    let order = |value: u64| {
        let magnitude = if daz && format.is_denormal(value) {
            0
        } else {
            i128::from(format.magnitude(value))
        };
        if format.negative(value) {
            -magnitude
        } else {
            magnitude
        }
    };
    let denormal = !daz && (format.is_denormal(a) || format.is_denormal(b));
    let relation = match order(a).cmp(&order(b)) {
        std::cmp::Ordering::Less => Relation::Less,
        std::cmp::Ordering::Equal => Relation::Equal,
        std::cmp::Ordering::Greater => Relation::Greater,
    };
    (if denormal { DE } else { 0 }, relation)
}

// ---------------------------------------------------------------------------
// Instruction fetch
// ---------------------------------------------------------------------------

/// Places `code` so that it ends where `CODE`'s page does, then runs it.
fn run_at_page_end(
    engine: Engine,
    code: &[u8],
    second_page: Option<(MemoryPermissions, &[u8])>,
) -> (CpuState, ExitReason, u64) {
    let start = PAGE - code.len();
    let mut page = vec![0xcc; PAGE];
    page[start..].copy_from_slice(code);
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(CODE, PAGE, MemoryPermissions::READ_EXECUTE, &page)
        .expect("map code");
    if let Some((permissions, bytes)) = second_page {
        memory
            .map_region_with_data(CODE + PAGE as u64, PAGE, permissions, bytes)
            .expect("map second page");
    }
    let mut cpu = initial_cpu();
    cpu.rip = CODE + start as u64;
    let (cpu, exit) = run(engine, &cpu, &mut memory);
    (cpu, exit, CODE + start as u64)
}

/// `mov eax, 5` then `mov rbx, imm64` cut off after three bytes.
const TRUNCATED: [u8; 8] = [0xb8, 5, 0, 0, 0, 0x48, 0xbb, 0x11];

#[test]
fn instruction_spanning_two_executable_pages_executes() {
    // SDM 1 3.2.1: the linear address space is contiguous across pages.
    let rest = [0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0xf4];
    for engine in ENGINES {
        let second = Some((MemoryPermissions::READ_EXECUTE, &rest[..]));
        let (cpu, exit, _) = run_at_page_end(engine, &TRUNCATED, second);
        assert_eq!(exit, ExitReason::Halt, "{engine:?}");
        assert_eq!(cpu.gpr[0], 5, "{engine:?}");
        assert_eq!(cpu.gpr[3], 0x8877_6655_4433_2211, "{engine:?}");
    }
}

#[test]
fn fetch_fault_is_precise_and_after_completed_instructions() {
    // SDM 3A 7.5 and 7.15 (Interrupt 14, instruction fetch).
    let next = CODE + PAGE as u64;
    for engine in ENGINES {
        let (cpu, exit, start) = run_at_page_end(engine, &TRUNCATED, None);
        let unmapped = ExitReason::MemoryFault(MemoryError::UnmappedAddress(next));
        assert_eq!(exit, unmapped, "{engine:?}");
        assert_eq!((cpu.gpr[0], cpu.rip), (5, start + 5), "{engine:?}");
        assert_eq!(cpu.gpr[3], initial_cpu().gpr[3], "{engine:?}");

        let second = Some((MemoryPermissions::READ_WRITE, &[0x22; 16][..]));
        let (cpu, exit, start) = run_at_page_end(engine, &TRUNCATED, second);
        let denied = ExitReason::MemoryFault(MemoryError::PermissionViolation {
            address: next,
            required: "execute",
        });
        assert_eq!(exit, denied, "{engine:?}");
        assert_eq!((cpu.gpr[0], cpu.rip), (5, start + 5), "{engine:?}");
    }
}

/// SDM 3A 11.1.3 option 1: store the new code, jump, then execute it.
fn self_modified_result(engine: Engine) -> (u64, u64, ExitReason) {
    // call f; mov ecx, eax; mov byte ptr [f+1], 2; jmp +0; call f; hlt;
    // f: mov eax, 1; ret
    let code = hex_bytes("e81100000089c1c6050900000002eb00e801000000f4b801000000c3");
    let mut page = vec![0xcc; PAGE];
    page[..code.len()].copy_from_slice(&code);
    let mut memory = Image::new().load(&[]);
    let base = CODE + 0x10_0000 + 0x8_0000;
    memory
        .map_region_with_data(base, PAGE, MemoryPermissions::READ_WRITE_EXECUTE, &page)
        .expect("map code");
    let mut cpu = initial_cpu();
    cpu.rip = base;
    let (cpu, exit) = run(engine, &cpu, &mut memory);
    (cpu.gpr[1], cpu.gpr[0], exit)
}

#[test]
fn interpreter_executes_modified_code_after_a_jump() {
    let wanted = (1, 2, ExitReason::Halt);
    assert_eq!(self_modified_result(Engine::Interpreter), wanted);
}

/// The DBT must not keep running the block it translated before the store (#84).
#[test]
fn dbt_executes_modified_code_after_a_jump() {
    for engine in [Engine::Dbt, Engine::DbtStepped] {
        let wanted = (1, 2, ExitReason::Halt);
        assert_eq!(self_modified_result(engine), wanted, "{engine:?}");
    }
}
