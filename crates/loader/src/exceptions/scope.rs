// SPDX-License-Identifier: GPL-3.0-or-later

//! `__C_specific_handler`: the language handler of MSVC-style C structured
//! exception handling (#453).
//!
//! Its handler data is a scope table. No public page read for this work
//! describes it, so the layout below was derived from the unwind data of
//! original programs: tests/compat_fixtures/guest_c_seh.c built by clang
//! 23.1.6 with `-fasync-exceptions`, and the startup frames of the mingw-w64
//! 14.0.0 C runtime in tests/compat_fixtures/guest_cpp_exceptions.cpp. It is
//! recorded as derived in docs/research/exception-handling.md:
//!
//! - a 32-bit count, then that many 16-byte records of four image-relative
//!   32-bit fields: `Begin`, `End`, `Handler`, `Target`;
//! - `Begin..End` is the guarded code, innermost scope first;
//! - `Target` 0 marks a `__finally`: `Handler` is its termination handler,
//!   called as `handler(abnormal, establisher_frame)`;
//! - otherwise the record is an `__except` whose body starts at `Target`, and
//!   `Handler` is its filter, called as `filter(exception_pointers,
//!   establisher_frame)`, or the literal 1 when the filter is the constant
//!   `EXCEPTION_EXECUTE_HANDLER`.
//!
//! Filter results follow the `try-except` page: negative continues execution,
//! zero continues the search, positive unwinds to the `__except` body with
//! the exception code in EAX (`GetExceptionCode` in the body reads it there).

use super::{
    EXCEPTION_EXIT_UNWIND, EXCEPTION_RETURN, EXCEPTION_TARGET_UNWIND, EXCEPTION_UNWINDING, Target,
    at, fault, read_u64, scratch_below, unwind,
};
use crate::cpu::{CpuState, ExitReason, Register};
use crate::imports::{
    IMPORT_TRAMPOLINE_STRIDE, ImportRegistry, RETURN_SENTINEL_PAGE, ServiceStatus, Win64Args,
};
use crate::memory::GuestMemory;

/// Return address under a filter or termination handler that
/// `__C_specific_handler` calls. Reaching it continues [`scope_resume`].
pub(crate) const SCOPE_RETURN: u64 = EXCEPTION_RETURN - IMPORT_TRAMPOLINE_STRIDE;
const _: () = assert!(SCOPE_RETURN & !0xFFF == RETURN_SENTINEL_PAGE);

const CONTINUE_EXECUTION: u64 = 0;
const CONTINUE_SEARCH: u64 = 1;
/// A filter given as the constant `EXCEPTION_EXECUTE_HANDLER`.
const EXECUTE_HANDLER: u32 = 1;
/// More scopes than any fixture has by far; a larger count is corrupt data.
const MAX_SCOPES: u32 = 4096;
/// `DISPATCHER_CONTEXT.ScopeIndex`, after `HistoryTable`.
const SCOPE_INDEX_AT: u64 = 0x48;

/// One `__C_specific_handler` call, waiting for a guest filter or
/// termination handler it called to return.
#[derive(Clone)]
pub(crate) struct ScopeWalk {
    /// The CPU at entry: RSP holds the return address into the dispatcher.
    entry: CpuState,
    record: u64,
    establisher: u64,
    context: u64,
    dispatcher: u64,
    unwinding: bool,
    /// The scope record being looked at.
    index: u32,
}

impl ScopeWalk {
    /// The context of the code that called `__C_specific_handler`.
    pub(crate) fn caller(&self, memory: &GuestMemory) -> Result<CpuState, ExitReason> {
        let rsp = self.entry.get(Register::Rsp);
        let mut caller = self.entry.clone();
        caller.rip = read_u64(memory, rsp)?;
        caller.set(Register::Rsp, at(rsp, 8)?);
        Ok(caller)
    }
}

#[derive(Clone, Copy)]
struct Scope {
    begin: u32,
    end: u32,
    handler: u32,
    target: u32,
}

fn c_specific_handler(
    args: &Win64Args,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<(), ExitReason> {
    let flags = memory.read_u32(at(args.rcx, 4)?).map_err(fault)?;
    let index = memory
        .read_u32(at(args.r9, SCOPE_INDEX_AT)?)
        .map_err(fault)?;
    let walk = ScopeWalk {
        entry: cpu.clone(),
        record: args.rcx,
        establisher: args.rdx,
        context: args.r8,
        dispatcher: args.r9,
        unwinding: flags & (EXCEPTION_UNWINDING | EXCEPTION_EXIT_UNWIND) != 0,
        index,
    };
    step(walk, None, cpu, memory)
}

/// A filter or termination handler returned to [`SCOPE_RETURN`].
pub(crate) fn scope_resume(cpu: &mut CpuState, memory: &mut GuestMemory) -> Result<(), ExitReason> {
    let walk = memory
        .scope_frames
        .remove(&(cpu.gs_base, cpu.get(Register::Rsp)))
        .ok_or(ExitReason::InvalidUnwind)?;
    let value = cpu.get(Register::Rax);
    step(walk, Some(value), cpu, memory)
}

/// Look at the scopes from `walk.index` on. `returned` is the result of the
/// filter or termination handler that just ran, if any.
fn step(
    mut walk: ScopeWalk,
    returned: Option<u64>,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<(), ExitReason> {
    let d = walk.dispatcher;
    let read = |offset| read_u64(memory, at(d, offset)?);
    let (control_pc, base, target_ip, data) = (read(0x00)?, read(0x08)?, read(0x20)?, read(0x38)?);
    let flags = memory.read_u32(at(walk.record, 4)?).map_err(fault)?;
    let count = memory.read_u32(data).map_err(fault)?;
    if count > MAX_SCOPES {
        return Err(ExitReason::InvalidUnwind);
    }
    let rva = control_pc
        .checked_sub(base)
        .and_then(|rva| u32::try_from(rva).ok())
        .ok_or(ExitReason::InvalidUnwind)?;
    let scope = |index: u32| -> Result<Scope, ExitReason> {
        let field = |n: u64| {
            memory
                .read_u32(at(data, 4 + 16 * u64::from(index) + 4 * n)?)
                .map_err(fault)
        };
        Ok(Scope {
            begin: field(0)?,
            end: field(1)?,
            handler: field(2)?,
            target: field(3)?,
        })
    };

    if let (Some(result), false) = (returned, walk.unwinding) {
        // EXCEPTION_CONTINUE_EXECUTION is -1 and EXCEPTION_EXECUTE_HANDLER 1.
        match result as i32 {
            ..0 => return finish(&walk, CONTINUE_EXECUTION, cpu, memory),
            0 => walk.index += 1,
            _ => return execute(&walk, scope(walk.index)?, base, cpu, memory),
        }
    }
    while walk.index < count {
        let s = scope(walk.index)?;
        if (s.begin..s.end).contains(&rva) {
            if !walk.unwinding && s.target != 0 {
                if s.handler == EXECUTE_HANDLER {
                    return execute(&walk, s, base, cpu, memory);
                }
                let pointers = [walk.record, walk.context];
                let filter = at(base, u64::from(s.handler))?;
                return call(walk, filter, Some(pointers), cpu, memory);
            }
            if walk.unwinding {
                if flags & EXCEPTION_TARGET_UNWIND != 0
                    && s.target != 0
                    && Some(target_ip) == base.checked_add(u64::from(s.target))
                {
                    // The `__except` the unwind is going to: the scopes
                    // outside it stay in force.
                    break;
                }
                if s.target == 0 {
                    walk.index += 1;
                    memory
                        .write_u32(at(d, SCOPE_INDEX_AT)?, walk.index)
                        .map_err(fault)?;
                    let handler = at(base, u64::from(s.handler))?;
                    return call(walk, handler, None, cpu, memory);
                }
            }
        }
        walk.index += 1;
    }
    finish(&walk, CONTINUE_SEARCH, cpu, memory)
}

/// Return `disposition` to the dispatcher with the registers of the entry.
fn finish(
    walk: &ScopeWalk,
    disposition: u64,
    cpu: &mut CpuState,
    memory: &GuestMemory,
) -> Result<(), ExitReason> {
    let mut caller = walk.caller(memory)?;
    caller.set(Register::Rax, disposition);
    *cpu = caller;
    Ok(())
}

/// A filter accepted the exception: unwind to its `__except` body in the
/// establisher frame, which receives the exception code in EAX.
fn execute(
    walk: &ScopeWalk,
    scope: Scope,
    base: u64,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<(), ExitReason> {
    if scope.target == 0 {
        return Err(ExitReason::InvalidUnwind);
    }
    let code = memory.read_u32(walk.record).map_err(fault)?;
    let origin = walk.caller(memory)?;
    let scratch = scratch_below(walk.entry.get(Register::Rsp), memory)?;
    let target = Target {
        frame: walk.establisher,
        ip: at(base, u64::from(scope.target))?,
        value: u64::from(code),
    };
    unwind(origin, scratch, target, walk.record, cpu, memory)
}

/// Call a filter with `EXCEPTION_POINTERS` built from `pointers`, or a
/// termination handler with `abnormal` TRUE, below the entry's stack.
fn call(
    walk: ScopeWalk,
    function: u64,
    pointers: Option<[u64; 2]>,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<(), ExitReason> {
    memory.fetch_instruction_byte(function).map_err(fault)?;
    let rsp = walk.entry.get(Register::Rsp);
    // The callee's home area, then EXCEPTION_POINTERS, below the entry.
    let area = rsp.checked_sub(0x40).ok_or(ExitReason::InvalidUnwind)? & !15;
    let slot = area.checked_sub(8).ok_or(ExitReason::InvalidUnwind)?;
    memory.check_write(slot, 0x38).map_err(fault)?;
    let first = match pointers {
        Some([record, context]) => {
            memory.write_u64(area + 0x20, record).map_err(fault)?;
            memory.write_u64(area + 0x28, context).map_err(fault)?;
            area + 0x20
        }
        None => 1,
    };
    memory.write_u64(slot, SCOPE_RETURN).map_err(fault)?;
    let key = (walk.entry.gs_base, slot + 8);
    if memory.scope_frames.contains_key(&key) {
        return Err(ExitReason::InvalidUnwind);
    }
    let mut callee = walk.entry.clone();
    callee.set(Register::Rcx, first);
    callee.set(Register::Rdx, walk.establisher);
    callee.set(Register::Rsp, slot);
    callee.rip = function;
    memory.scope_frames.insert(key, walk);
    *cpu = callee;
    Ok(())
}

pub(super) fn register(registry: &mut ImportRegistry) {
    registry.register_context_service(
        "ucrtbase.dll",
        "__C_specific_handler",
        ServiceStatus::Partial,
        c_specific_handler,
    );
    registry.alias(
        "api-ms-win-crt-private-l1-1-0.dll",
        "ucrtbase.dll",
        "__C_specific_handler",
    );
    registry.alias("msvcrt.dll", "ucrtbase.dll", "__C_specific_handler");
}
