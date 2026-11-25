// SPDX-License-Identifier: GPL-3.0-or-later
// Freestanding PE test. The TLS callback exits with 73. Reaching the PE
// entry would instead return 37 through Xenora's entry return sentinel.
.intel_syntax noprefix
.text
.globl gate_entry
gate_entry:
    mov eax, 37
    ret

tls_exit:
    sub rsp, 40
    mov ecx, 73
    call qword ptr [rip + __imp_ExitProcess]
    int3

.data
tls_start:
    .byte 0
tls_end:
    .p2align 2
tls_index:
    .long 0

.section .rdata,"dr"
    .p2align 3
    .globl _tls_used
_tls_used:
    .quad tls_start, tls_end, tls_index, tls_callbacks
    .long 0, 0
tls_callbacks:
    .quad tls_exit, 0
