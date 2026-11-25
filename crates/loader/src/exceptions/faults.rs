// SPDX-License-Identifier: GPL-3.0-or-later

//! Guest CPU faults as structured exceptions (#453).
//!
//! A fault raised by a guest instruction becomes an `EXCEPTION_RECORD` and
//! starts the same search as `RaiseException`, from the faulting instruction.
//! The policy, recorded in docs/research/exception-handling.md:
//!
//! - `DivideError` is `STATUS_INTEGER_DIVIDE_BY_ZERO`. Xenora's #DE does not
//!   say whether the divisor was zero or the quotient overflowed, so an
//!   overflow is reported with the same code.
//! - `MemoryFault` with an address is `STATUS_ACCESS_VIOLATION` with two
//!   parameters: 0 for a read, 1 for a write, 8 for an instruction fetch, then
//!   the address. 8 is the documented DEP value; that Windows also reports it
//!   for a fetch from unmapped memory is not documented on the pages read.
//! - `IllegalInstruction` is `STATUS_ILLEGAL_INSTRUCTION` only for UD2, the
//!   one encoding defined to be invalid. Any other opcode Xenora refuses may
//!   be a valid instruction it does not implement, and a guest handler must
//!   not hide that, so it still stops the guest. So does the runtime-limit
//!   marker `IllegalInstruction(0xFE)`.
//! - `GeneralProtection` and `SimdException` still stop the guest: Xenora's
//!   reasons do not carry the detail a record needs (which #GP cause, which
//!   MXCSR flag), and their Windows codes are not on the pages read.
//! - Faults inside host services, faults with no address, faults in a
//!   process with no exception directory and faults whose stack has no room
//!   for the dispatcher still stop the guest as before.
//! - A fault no frame accepts stops the guest with the original reason, at
//!   the faulting instruction. The unhandled-exception filter is not called.

use super::{
    Dispatch, EXCEPTION_CONTEXT_AT, RECORD_AT, RECORD_SIZE, fault, scratch_below, walk,
    write_context,
};
use crate::cpu::{CpuState, ExitReason, Register};
use crate::memory::{GuestMemory, MemoryError};

const STATUS_ACCESS_VIOLATION: u32 = 0xC000_0005;
const STATUS_ILLEGAL_INSTRUCTION: u32 = 0xC000_001D;
const STATUS_INTEGER_DIVIDE_BY_ZERO: u32 = 0xC000_0094;

const READ: u64 = 0;
const WRITE: u64 = 1;
const EXECUTE: u64 = 8;

/// The code and parameters of the record for `reason`, or `None` when the
/// policy above keeps it a stop.
fn record_for(
    reason: ExitReason,
    fetch: bool,
    cpu: &CpuState,
    memory: &mut GuestMemory,
) -> Option<(u32, Vec<u64>)> {
    let write_fault = memory.write_fault.take();
    match reason {
        ExitReason::DivideError => Some((STATUS_INTEGER_DIVIDE_BY_ZERO, Vec::new())),
        ExitReason::MemoryFault(MemoryError::UnmappedAddress(address)) => {
            let kind = if fetch {
                EXECUTE
            } else if write_fault == Some(address) {
                WRITE
            } else {
                READ
            };
            Some((STATUS_ACCESS_VIOLATION, vec![kind, address]))
        }
        ExitReason::MemoryFault(MemoryError::PermissionViolation { address, required }) => {
            let kind = match required {
                "write" => WRITE,
                "execute" => EXECUTE,
                _ => READ,
            };
            Some((STATUS_ACCESS_VIOLATION, vec![kind, address]))
        }
        ExitReason::IllegalInstruction(op) if op != 0xFE && is_ud2(cpu.rip, memory) => {
            Some((STATUS_ILLEGAL_INSTRUCTION, Vec::new()))
        }
        _ => None,
    }
}

fn is_ud2(rip: u64, memory: &GuestMemory) -> bool {
    let byte = |offset| {
        rip.checked_add(offset)
            .and_then(|address| memory.fetch_instruction_byte(address).ok())
    };
    byte(0) == Some(0x0F) && byte(1) == Some(0x0B)
}

/// Turn a fault a guest instruction raised at `cpu.rip` into an exception
/// and start the search for a handler. `fetch` says the fault happened while
/// fetching or decoding the instruction. `Ok` means the guest continues in a
/// handler; `Err` holds the reason the guest stops, which is `reason` itself
/// when the policy keeps the fault a stop or no frame accepts it.
pub(crate) fn dispatch_fault(
    reason: ExitReason,
    fetch: bool,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<(), ExitReason> {
    if memory.function_tables.is_empty() {
        return Err(reason);
    }
    let Some((code, parameters)) = record_for(reason, fetch, cpu, memory) else {
        return Err(reason);
    };
    let Ok(scratch) = scratch_below(cpu.get(Register::Rsp), memory) else {
        return Err(reason);
    };
    let record = scratch + RECORD_AT;
    memory
        .write_bytes(record, &[0; RECORD_SIZE as usize])
        .map_err(fault)?;
    memory.write_u32(record, code).map_err(fault)?;
    memory.write_u64(record + 0x10, cpu.rip).map_err(fault)?;
    memory
        .write_u32(record + 0x18, parameters.len() as u32)
        .map_err(fault)?;
    for (index, value) in parameters.iter().enumerate() {
        memory
            .write_u64(record + 0x20 + 8 * index as u64, *value)
            .map_err(fault)?;
    }
    let origin = cpu.clone();
    write_context(memory, scratch + EXCEPTION_CONTEXT_AT, &origin)?;
    walk(
        Dispatch::search(origin, scratch, Some(reason), fetch),
        cpu,
        memory,
    )
}

#[cfg(test)]
mod tests {
    //! Expected codes and parameters are the documented values, not a run.
    use super::*;
    use crate::memory::MemoryPermissions;

    fn memory_with(code: &[u8]) -> GuestMemory {
        let mut memory = GuestMemory::new();
        memory
            .map_region_with_data(0x1000, 0x1000, MemoryPermissions::READ_EXECUTE, code)
            .unwrap();
        memory
    }

    #[test]
    fn faults_map_to_their_documented_records() {
        let cpu = CpuState::new(0x1000, 0);
        let mut memory = memory_with(&[0x0F, 0x0B]);
        let mut record = |reason, fetch| record_for(reason, fetch, &cpu, &mut memory);
        assert_eq!(
            record(ExitReason::DivideError, false),
            Some((0xC000_0094, vec![]))
        );
        let unmapped = ExitReason::MemoryFault(MemoryError::UnmappedAddress(0x40));
        assert_eq!(record(unmapped, false), Some((0xC000_0005, vec![0, 0x40])));
        assert_eq!(record(unmapped, true), Some((0xC000_0005, vec![8, 0x40])));
        let denied = |required| {
            ExitReason::MemoryFault(MemoryError::PermissionViolation {
                address: 0x44,
                required,
            })
        };
        assert_eq!(
            record(denied("write"), false),
            Some((0xC000_0005, vec![1, 0x44]))
        );
        assert_eq!(
            record(denied("execute"), true),
            Some((0xC000_0005, vec![8, 0x44]))
        );
        assert_eq!(
            record(denied("read"), false),
            Some((0xC000_0005, vec![0, 0x44]))
        );
        assert_eq!(
            record(ExitReason::IllegalInstruction(0x0B), true),
            Some((0xC000_001D, vec![]))
        );
    }

    #[test]
    fn a_failed_write_to_the_faulting_address_is_reported_as_a_write() {
        let cpu = CpuState::new(0x1000, 0);
        let mut memory = memory_with(&[0x90]);
        assert!(memory.write_u32(0x40, 1).is_err());
        let unmapped = ExitReason::MemoryFault(MemoryError::UnmappedAddress(0x40));
        assert_eq!(
            record_for(unmapped, false, &cpu, &mut memory),
            Some((0xC000_0005, vec![1, 0x40]))
        );
        // The note is used once: a later read fault there is a read.
        assert_eq!(
            record_for(unmapped, false, &cpu, &mut memory),
            Some((0xC000_0005, vec![0, 0x40]))
        );
    }

    #[test]
    fn the_policy_keeps_other_faults_as_stops() {
        let cpu = CpuState::new(0x1000, 0);
        // Not UD2: an opcode Xenora refuses may be one it does not implement.
        let mut memory = memory_with(&[0xCC]);
        for reason in [
            ExitReason::IllegalInstruction(0xCC),
            ExitReason::IllegalInstruction(0xFE),
            ExitReason::GeneralProtection,
            ExitReason::SimdException,
            ExitReason::MemoryFault(MemoryError::Overflow),
            ExitReason::StepLimitExceeded,
        ] {
            assert_eq!(
                record_for(reason, false, &cpu, &mut memory),
                None,
                "{reason}"
            );
        }
        // The runtime-limit marker is never UD2, even at a UD2.
        let mut memory = memory_with(&[0x0F, 0x0B]);
        assert_eq!(
            record_for(
                ExitReason::IllegalInstruction(0xFE),
                true,
                &cpu,
                &mut memory
            ),
            None
        );
    }

    #[test]
    fn a_process_without_an_exception_directory_keeps_the_fault() {
        let mut cpu = CpuState::new(0x1000, 0x2000);
        let mut memory = memory_with(&[0xF7, 0xF1]);
        let before = cpu.clone();
        assert_eq!(
            dispatch_fault(ExitReason::DivideError, false, &mut cpu, &mut memory),
            Err(ExitReason::DivideError)
        );
        assert_eq!(cpu, before);
    }
}
