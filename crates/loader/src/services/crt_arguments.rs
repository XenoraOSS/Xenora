// SPDX-License-Identifier: GPL-3.0-or-later

//! The arguments and environment a hosted C program's startup code hands to
//! `main`.
//!
//! Documented: the global variables `__argc` (the argument count), `__argv` (a
//! pointer to the array of argument strings) and `_environ` (a pointer to the
//! array of environment strings), and ISO C's rule that a null pointer follows
//! the last argument.
//!
//! Inferred: that `__p___argc`, `__p___argv` and `__p__environ` return the
//! address of those variables, as the documented `__p__fmode` does for
//! `_fmode`. Microsoft lists all five functions here only by name.
//!
//! Xenora's choice, because nothing public describes it: what
//! `_configure_narrow_argv` and `_initialize_narrow_environment` do. The
//! arguments come from the host, as bytes with no code page; wildcards are not
//! expanded; and the environment table is empty.

use crate::cpu::ExitReason;
use crate::memory::{GuestMemory, MemoryError, PROCESS_HEAP};
use std::fmt;

const EINVAL: i32 = 22;
/// `_crt_argv_no_arguments`, `_crt_argv_unexpanded_arguments` and
/// `_crt_argv_expanded_arguments` (corecrt_startup.h).
const NO_ARGUMENTS: i32 = 0;
const UNEXPANDED_ARGUMENTS: i32 = 1;
const EXPANDED_ARGUMENTS: i32 = 2;
/// Windows limits a command line to 32,767 characters including its
/// terminator (`CreateProcess`). The same bound is applied here to the
/// arguments with their terminators.
pub const MAX_PROGRAM_ARGUMENT_BYTES: usize = 32_767;

/// Why [`super::ServicesState::set_program_arguments`] refused its input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramArgumentsError {
    /// An argument holds a NUL byte, which would end it early in the guest.
    ContainsNul,
    /// The arguments and their terminators exceed
    /// [`MAX_PROGRAM_ARGUMENT_BYTES`].
    TooLong,
    /// The guest has already built `__argv` or read its command line; it would
    /// not see a change.
    AlreadyConfigured,
}

impl fmt::Display for ProgramArgumentsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ContainsNul => "a program argument contains a NUL byte",
            Self::TooLong => "the program arguments exceed 32,767 bytes",
            Self::AlreadyConfigured => "the guest has already configured its arguments",
        })
    }
}

impl std::error::Error for ProgramArgumentsError {}

/// Which of the three guest variables a `__p_` function points at. The value
/// is the variable's offset in their shared block.
#[derive(Clone, Copy)]
pub(super) enum Variable {
    Argc = 0,
    Argv = 8,
    Environ = 16,
}

#[derive(Default)]
pub(super) struct CrtArguments {
    /// What the host supplied, program name first.
    supplied: Option<Vec<Vec<u8>>>,
    /// Guest address of the block holding `__argc`, `__argv` and `_environ`.
    variables: Option<u64>,
    configured: bool,
    command_line_read: bool,
    environment_initialized: bool,
}

fn fault(error: MemoryError) -> ExitReason {
    ExitReason::MemoryFault(error)
}

impl CrtArguments {
    pub(super) fn supply(&mut self, arguments: &[&str]) -> Result<(), ProgramArgumentsError> {
        if self.configured || self.command_line_read {
            return Err(ProgramArgumentsError::AlreadyConfigured);
        }
        if arguments.iter().any(|argument| argument.contains('\0')) {
            return Err(ProgramArgumentsError::ContainsNul);
        }
        let bytes: usize = arguments.iter().map(|argument| argument.len() + 1).sum();
        if bytes > MAX_PROGRAM_ARGUMENT_BYTES {
            return Err(ProgramArgumentsError::TooLong);
        }
        self.supplied = Some(
            arguments
                .iter()
                .map(|argument| argument.as_bytes().to_vec())
                .collect(),
        );
        Ok(())
    }

    /// The arguments as text, for the command line. They cannot change after
    /// that. With no host input there is one, the empty program name.
    pub(super) fn for_command_line(&mut self) -> Vec<String> {
        self.command_line_read = true;
        let default = [Vec::new()];
        let arguments = self.supplied.as_deref().unwrap_or(&default);
        arguments
            .iter()
            .map(|argument| String::from_utf8_lossy(argument).into_owned())
            .collect()
    }

    /// Guest address of one of the three variables. They are allocated
    /// together from the process heap on first use, as zeros: no arguments,
    /// and null `__argv` and `_environ`.
    pub(super) fn address(
        &mut self,
        memory: &mut GuestMemory,
        variable: Variable,
    ) -> Result<u64, ExitReason> {
        let base = match self.variables {
            Some(base) => base,
            None => {
                let base = memory
                    .map_heap_allocation(PROCESS_HEAP, 24)
                    .map_err(fault)?;
                self.variables = Some(base);
                base
            }
        };
        Ok(base + variable as u64)
    }

    /// `_configure_narrow_argv`. Modes 1 and 2 build `__argc` and `__argv`
    /// once; a later call changes nothing. Mode 0 builds nothing. Any other
    /// mode is refused with `EINVAL`.
    pub(super) fn configure(
        &mut self,
        memory: &mut GuestMemory,
        mode: i32,
    ) -> Result<i32, ExitReason> {
        match mode {
            NO_ARGUMENTS => return Ok(0),
            UNEXPANDED_ARGUMENTS | EXPANDED_ARGUMENTS => {}
            _ => return Ok(EINVAL),
        }
        if self.configured {
            return Ok(0);
        }
        // With no host input the program name is not available, and ISO C
        // then makes argv[0] the empty string.
        let default = [Vec::new()];
        let arguments = self.supplied.as_deref().unwrap_or(&default);
        let table_bytes = 8 * (arguments.len() + 1);
        let string_bytes: usize = arguments.iter().map(|argument| argument.len() + 1).sum();
        let table = memory
            .map_heap_allocation(PROCESS_HEAP, table_bytes + string_bytes)
            .map_err(fault)?;
        // The pointers first, then the strings. The block starts as zeros, so
        // the pointer after the last argument and every terminator are in place.
        let mut string = table + table_bytes as u64;
        for (index, argument) in arguments.iter().enumerate() {
            memory
                .write_u64(table + 8 * index as u64, string)
                .map_err(fault)?;
            memory.write_bytes(string, argument).map_err(fault)?;
            string += argument.len() as u64 + 1;
        }
        let count = arguments.len() as u32;
        let argc = self.address(memory, Variable::Argc)?;
        let argv = self.address(memory, Variable::Argv)?;
        memory.write_u32(argc, count).map_err(fault)?;
        memory.write_u64(argv, table).map_err(fault)?;
        self.configured = true;
        Ok(0)
    }

    /// `_initialize_narrow_environment`. Points `_environ` at an empty table,
    /// one null pointer, once.
    pub(super) fn initialize_environment(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<i32, ExitReason> {
        if !self.environment_initialized {
            let table = memory.map_heap_allocation(PROCESS_HEAP, 8).map_err(fault)?;
            let environ = self.address(memory, Variable::Environ)?;
            memory.write_u64(environ, table).map_err(fault)?;
            self.environment_initialized = true;
        }
        Ok(0)
    }
}
