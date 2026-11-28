// SPDX-License-Identifier: GPL-3.0-or-later

.intel_syntax noprefix
.text
.globl entry
entry:
    sub rsp, 40
    lea rcx, [rip + jump_buffer]
#if defined(NONNULL_FRAME)
    mov rdx, rsp
    call qword ptr [rip + __imp___intrinsic_setjmp]
    mov ecx, 80
#elif defined(NONZERO_SAVED_FRAME)
    xor edx, edx
    call qword ptr [rip + __imp___intrinsic_setjmp]
    test eax, eax
    jnz unexpected_resume
    lea rcx, [rip + jump_buffer]
    mov qword ptr [rcx], rsp
    mov edx, 17
    call qword ptr [rip + __imp_longjmp]
unexpected_resume:
    mov ecx, 81
#else
save_point:
    mov qword ptr [rip + saved_rsp], rsp
    mov ebx, 0x31415926
    mov r12d, 0x27182818
    mov r15d, 0x1234abcd
    movdqu xmm6, xmmword ptr [rip + saved_xmm6]
    movdqu xmm15, xmmword ptr [rip + saved_xmm15]
    ldmxcsr dword ptr [rip + saved_mxcsr]
    lea rcx, [rip + jump_buffer]
    xor edx, edx
    call qword ptr [rip + __imp___intrinsic_setjmp]
    test eax, eax
    jnz resumed
    mov ecx, 19
    cmp word ptr [rip + jump_buffer + 0x5c], 0x037f
    jne finish
    call outer
    mov ecx, 10
    jmp finish
resumed:
    mov ecx, 11
    cmp rsp, qword ptr [rip + saved_rsp]
    jne finish
    mov ecx, 12
    cmp rbx, 0x31415926
    jne finish
    cmp r12, 0x27182818
    jne finish
    cmp r15, 0x1234abcd
    jne finish

    mov ecx, 13
    movdqu xmmword ptr [rip + actual_xmm], xmm6
    mov rdx, qword ptr [rip + actual_xmm]
    cmp rdx, qword ptr [rip + saved_xmm6]
    jne finish
    mov rdx, qword ptr [rip + actual_xmm + 8]
    cmp rdx, qword ptr [rip + saved_xmm6 + 8]
    jne finish
    movdqu xmmword ptr [rip + actual_xmm], xmm15
    mov rdx, qword ptr [rip + actual_xmm]
    cmp rdx, qword ptr [rip + saved_xmm15]
    jne finish
    mov rdx, qword ptr [rip + actual_xmm + 8]
    cmp rdx, qword ptr [rip + saved_xmm15 + 8]
    jne finish

    mov ecx, 14
    stmxcsr dword ptr [rip + actual_mxcsr]
    cmp dword ptr [rip + actual_mxcsr], 0x3f80
    jne finish

    cmp dword ptr [rip + phase], 0
    jne zero_jump
    mov ecx, 15
    cmp eax, 73
    jne finish
    mov dword ptr [rip + phase], 1
    jmp save_point
zero_jump:
    mov ecx, 16
    cmp eax, 1
    jne finish
    xor ecx, ecx
#endif
finish:
    call qword ptr [rip + __imp_ExitProcess]
    int3

#if !defined(NONNULL_FRAME) && !defined(NONZERO_SAVED_FRAME)
// Walk through two actual call frames. No destructor or SEH cleanup is needed.
outer:
    sub rsp, 40
    call inner
    mov ecx, 17
    jmp finish
inner:
    sub rsp, 40
    xor ebx, ebx
    xor r12d, r12d
    xor r15d, r15d
    pxor xmm6, xmm6
    pxor xmm15, xmm15
    ldmxcsr dword ptr [rip + other_mxcsr]
    lea rcx, [rip + jump_buffer]
    mov edx, 73
    cmp dword ptr [rip + phase], 0
    je jump_now
    // Supply a distinct valid x87 control word through the saved guest buffer.
    mov word ptr [rip + jump_buffer + 0x5c], 0x077f
    xor edx, edx
jump_now:
    call qword ptr [rip + __imp_longjmp]
    mov ecx, 18
    jmp finish

.section .rdata,"dr"
.p2align 4
saved_xmm6:
    .quad 0xaffaf0123456789a, 0xedfed00123456789
saved_xmm15:
    .quad 0xfacefaceaaaaaaaa, 0xbeefbeef55555555
saved_mxcsr:
    .long 0x3f80
other_mxcsr:
    .long 0x5f80
#endif

.section .jbuf,"dw"
.p2align 4
jump_buffer:
#if defined(NONNULL_FRAME)
    .fill 256, 1, 0xa5
#else
    .zero 256
#endif

#if !defined(NONNULL_FRAME) && !defined(NONZERO_SAVED_FRAME)
.bss
.p2align 4
actual_xmm:
    .zero 16
saved_rsp:
    .zero 8
phase:
    .zero 4
actual_mxcsr:
    .zero 4
#endif
