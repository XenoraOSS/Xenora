//! Register-only C jumps shared by the legacy CRT, UCRT and VCRUNTIME140.
//!
//! The jump buffer is the documented 256-byte x64 layout: the frame word, the
//! callee-saved general registers, the resume stack and instruction pointers,
//! the x87 and SSE control words, and xmm6 through xmm15. `_setjmp` is an
//! ordinary service: the machinery's own return path is exactly the first
//! return the CRT documents, so the handler only records the state and answers
//! zero. `longjmp` cannot be an ordinary service - it rewrites the
//! callee-saved registers, the stack pointer and the instruction pointer,
//! which the standard return path would overwrite - so it is registered as a
//! context service, the same mechanism the exception unwinder uses.
//!
//! The saved frame word is zero. VCRUNTIME140 requires an explicit null
//! frame and refuses a restore that requests stack unwinding. The control
//! words are the live ones, so a restore across a changed rounding mode
//! behaves like the CRT's.

use crate::cpu::ExitReason;
use crate::cpu::{CpuState, Register};
use crate::imports::ImportRegistry;
use crate::imports::{ServiceStatus, Win64Args};
use crate::memory::GuestMemory;

/// The x64 jump buffer spans 256 bytes, `SETJMP_FLOAT128[16]` as the headers
/// declare it.
const JUMP_BUFFER_SIZE: usize = 256;
const VCRUNTIME140: &str = "vcruntime140.dll";

/// qword offsets inside the jump buffer, in save order.
const FIELD_FRAME: usize = 0x00;
const FIELD_RBX: usize = 0x08;
const FIELD_RSP: usize = 0x10;
const FIELD_RBP: usize = 0x18;
const FIELD_RSI: usize = 0x20;
const FIELD_RDI: usize = 0x28;
const FIELD_R12: usize = 0x30;
const FIELD_R13: usize = 0x38;
const FIELD_R14: usize = 0x40;
const FIELD_R15: usize = 0x48;
const FIELD_RIP: usize = 0x50;
const FIELD_MXCSR: usize = 0x58;
const FIELD_FPCSR: usize = 0x5C;
const FIELD_XMM6: usize = 0x60;

fn write_qword(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_qword(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("qword field"))
}

/// The documented first return: zero, through the ordinary service path.
fn setjmp(
    args: &Win64Args,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<u64, ExitReason> {
    if args.rcx == 0 {
        return Ok(0);
    }
    save_jump_buffer(args.rcx, cpu, memory)
}

fn save_jump_buffer(
    buffer: u64,
    cpu: &CpuState,
    memory: &mut GuestMemory,
) -> Result<u64, ExitReason> {
    memory
        .check_write(buffer, JUMP_BUFFER_SIZE)
        .map_err(ExitReason::MemoryFault)?;
    let rsp = cpu.get(Register::Rsp);
    let resume_rip = memory.read_u64(rsp).map_err(ExitReason::MemoryFault)?;
    let resume_rsp = rsp.checked_add(8).ok_or(ExitReason::MemoryFault(
        crate::memory::MemoryError::Overflow,
    ))?;

    let mut bytes = [0u8; JUMP_BUFFER_SIZE];
    // The frame word stays zero: the unwinding of abandoned frames is not
    // modelled, and no frame registration exists to record.
    write_qword(&mut bytes, FIELD_RBX, cpu.get(Register::Rbx));
    write_qword(&mut bytes, FIELD_RSP, resume_rsp);
    write_qword(&mut bytes, FIELD_RBP, cpu.get(Register::Rbp));
    write_qword(&mut bytes, FIELD_RSI, cpu.get(Register::Rsi));
    write_qword(&mut bytes, FIELD_RDI, cpu.get(Register::Rdi));
    write_qword(&mut bytes, FIELD_R12, cpu.get(Register::R12));
    write_qword(&mut bytes, FIELD_R13, cpu.get(Register::R13));
    write_qword(&mut bytes, FIELD_R14, cpu.get(Register::R14));
    write_qword(&mut bytes, FIELD_R15, cpu.get(Register::R15));
    write_qword(&mut bytes, FIELD_RIP, resume_rip);
    bytes[FIELD_MXCSR..FIELD_MXCSR + 4].copy_from_slice(&cpu.mxcsr.0.to_le_bytes());
    bytes[FIELD_FPCSR..FIELD_FPCSR + 2].copy_from_slice(&cpu.x87.control.to_le_bytes());
    for index in 0..10 {
        let offset = FIELD_XMM6 + index * 16;
        bytes[offset..offset + 16].copy_from_slice(&cpu.xmm[6 + index].to_le_bytes());
    }
    memory
        .write_bytes(buffer, &bytes)
        .map_err(ExitReason::MemoryFault)?;
    Ok(0)
}

/// The second return: the callee-saved state, the stack and the instruction
/// pointer all move to the recorded point, and the value lands in RAX. A zero
/// `value` becomes one, as documented. Because this rewrites registers the
/// ordinary return path would restore, it is a context service: the machinery
/// applies the CPU state exactly as the handler leaves it.
fn longjmp(
    args: &Win64Args,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
    require_null_frame: bool,
) -> Result<(), ExitReason> {
    let buffer = args.rcx;
    let value = args.rdx as u32;
    let mut bytes = [0u8; JUMP_BUFFER_SIZE];
    memory
        .read_bytes(buffer, &mut bytes)
        .map_err(ExitReason::MemoryFault)?;
    if require_null_frame && read_qword(&bytes, FIELD_FRAME) != 0 {
        return Err(ExitReason::InvalidUnwind);
    }

    cpu.set(Register::Rbx, read_qword(&bytes, FIELD_RBX));
    cpu.set(Register::Rsp, read_qword(&bytes, FIELD_RSP));
    cpu.set(Register::Rbp, read_qword(&bytes, FIELD_RBP));
    cpu.set(Register::Rsi, read_qword(&bytes, FIELD_RSI));
    cpu.set(Register::Rdi, read_qword(&bytes, FIELD_RDI));
    cpu.set(Register::R12, read_qword(&bytes, FIELD_R12));
    cpu.set(Register::R13, read_qword(&bytes, FIELD_R13));
    cpu.set(Register::R14, read_qword(&bytes, FIELD_R14));
    cpu.set(Register::R15, read_qword(&bytes, FIELD_R15));
    cpu.rip = read_qword(&bytes, FIELD_RIP);
    cpu.mxcsr.0 = u32::from_le_bytes(
        bytes[FIELD_MXCSR..FIELD_MXCSR + 4]
            .try_into()
            .expect("mxcsr field"),
    );
    cpu.x87.control = u16::from_le_bytes(
        bytes[FIELD_FPCSR..FIELD_FPCSR + 2]
            .try_into()
            .expect("fpcsr field"),
    );
    for index in 0..10 {
        let offset = FIELD_XMM6 + index * 16;
        cpu.xmm[6 + index] =
            u128::from_le_bytes(bytes[offset..offset + 16].try_into().expect("xmm field"));
    }
    cpu.set(Register::Rax, u64::from(if value == 0 { 1 } else { value }));
    Ok(())
}

pub(crate) fn register(registry: &mut ImportRegistry) {
    // Both the legacy msvcrt.dll spelling busybox imports and the modern
    // Universal CRT spellings: ucrtbase exports the service under its own
    // name and under the compiler-intrinsic one mingw links against. The
    // api-ms-win-crt-private api set resolves to ucrtbase like the rest.
    for module in ["msvcrt.dll", "ucrtbase.dll"] {
        registry.register_with_status(module, "_setjmp", ServiceStatus::Partial, setjmp);
        registry.register_context_service(
            module,
            "longjmp",
            ServiceStatus::Partial,
            |args, cpu, memory| longjmp(args, cpu, memory, false),
        );
    }
    registry.register_with_status(
        "ucrtbase.dll",
        "__intrinsic_setjmp",
        ServiceStatus::Partial,
        setjmp,
    );
    // The private CRT api set carries the jump services for a ucrt-linked
    // image, the same resolution the other private CRT exports follow.
    for symbol in ["_setjmp", "__intrinsic_setjmp", "longjmp"] {
        registry.alias("api-ms-win-crt-private-l1-1-0.dll", "ucrtbase.dll", symbol);
    }

    registry.register_with_status(
        VCRUNTIME140,
        "__intrinsic_setjmp",
        ServiceStatus::Partial,
        |args, cpu, memory| {
            // The x64 CRT's null Frame selects a jump without cleanup.
            if args.rdx != 0 {
                return Err(ExitReason::InvalidUnwind);
            }
            save_jump_buffer(args.rcx, cpu, memory)
        },
    );
    registry.register_context_service(
        VCRUNTIME140,
        "longjmp",
        ServiceStatus::Partial,
        |args, cpu, memory| longjmp(args, cpu, memory, true),
    );
}
