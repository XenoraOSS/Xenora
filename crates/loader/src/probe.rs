// SPDX-License-Identifier: GPL-3.0-or-later

//! Non-executing PE compatibility probe diagnostic generator for Xenora X14.5+X16.
//!
//! Parses candidate PE32+ binaries in strictly read-only mode, inventories
//! sections, relocations, TLS structures, import dependencies, and performs bounded
//! control-flow-aware static instruction disassembly without executing any guest code.
//!
//! The disassembly phase uses a worklist-based CFG traversal so that unreachable
//! padding bytes (e.g. INT3 after an unconditional JMP) are never classified as
//! startup blockers.

use crate::dbt::{is_native, is_translatable};
use crate::import_table::{
    ImportEntry, ImportName, ImportTableError, parse_delay_imports, parse_imports,
};
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::MemoryPermissions;
use crate::pe::{IMAGE_FILE_MACHINE_AMD64, PE32_PLUS_MAGIC, ParsedPe, PeError, PeSection};
use crate::services::{MockClock, ServicesState, register_standard_services};
use crate::x86_decode::{Encoding, MAX_INSTRUCTION_LEN, decode};
use crate::x86_table::Op;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

/// Bounded probe configuration.
#[derive(Clone, Debug)]
pub struct ProbeConfig {
    pub max_disasm_bytes: usize,
    pub max_instructions: usize,
    /// Maximum number of CFG worklist entries to visit (guards against exponential blowup).
    pub max_worklist_entries: usize,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            max_disasm_bytes: 65536,
            max_instructions: 10000,
            max_worklist_entries: 4096,
        }
    }
}

/// Reachability classification for a decoded instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reachability {
    /// Reached via at least one CFG edge from the entry point or a TLS callback.
    Reachable,
    /// Not reached by the bounded CFG traversal; may be dead code or padding.
    Unreachable,
    /// Reachability could not be determined (e.g. behind an indirect branch target).
    Uncertain,
}

/// Internal CFG traversal result for a single decoded instruction.
#[derive(Clone, Debug)]
struct CfgDecoded {
    #[allow(dead_code)]
    rva: u32,
    insn_len: usize,
    mnemonic: String,
    is_dbt: bool,
    is_interp: bool,
    notes: String,
    /// Statically known successor RVAs to push onto the worklist.
    successors: Vec<u32>,
    /// True if fall-through to the next sequential byte is reachable.
    fall_through: bool,
}

/// Consume a ModRM byte with its SIB byte and displacement, returning the
/// ModRM byte and the offset just past the operand, or `None` if truncated.
fn modrm_operand(slice: &[u8], pc: usize) -> Option<(u8, usize)> {
    let modrm = *slice.get(pc)?;
    let mode = modrm >> 6;
    let mut end = pc + 1;
    let mut base = modrm & 7;
    if mode != 3 && base == 4 {
        base = slice.get(end)? & 7;
        end += 1;
    }
    end += match mode {
        1 => 1,
        2 => 4,
        0 if base == 5 => 4,
        _ => 0,
    };
    (end <= slice.len()).then_some((modrm, end))
}

/// Decode a single x86-64 instruction from a file-mapped byte slice.
///
/// `slice`  — file bytes beginning at the instruction's raw file offset.
/// `rva`    — the instruction's RVA (used to compute branch targets).
/// `va`     — the instruction's virtual address (same).
///
/// Returns `None` if the slice is empty.
fn decode_one(slice: &[u8], rva: u32, va: u64) -> Option<CfgDecoded> {
    if slice.is_empty() {
        return None;
    }

    let mut pc = 0usize;
    let mut b0 = slice[pc];
    pc += 1;

    // Legacy segment prefixes: 0x65 (GS), 0x64 (FS). The run is bounded by the
    // architectural instruction-length limit (15 bytes); a longer run is
    // reported as an overlong prefix sequence instead of decoding past it.
    while (b0 == 0x65 || b0 == 0x64) && pc < slice.len() && pc < MAX_INSTRUCTION_LEN {
        b0 = slice[pc];
        pc += 1;
    }
    let overlong_prefix_run =
        pc < slice.len() && (b0 == 0x65 || b0 == 0x64) && pc >= MAX_INSTRUCTION_LEN;
    if overlong_prefix_run {
        return Some(CfgDecoded {
            rva,
            insn_len: pc,
            mnemonic: "overlong prefix run".into(),
            is_dbt: false,
            is_interp: false,
            notes: "instruction length exceeds the 15-byte architectural limit".into(),
            successors: vec![],
            fall_through: false,
        });
    }

    // Recognize the operand-size override for C7 and the extension family
    // without changing how the probe classifies prefixes on unrelated opcodes.
    let operand_override = b0 == 0x66
        && matches!(
            slice.get(pc..),
            Some(
                [0xc7, ..]
                    | [0x40..=0x4f, 0xc7, ..]
                    | [0x0f, 0xb6 | 0xb7 | 0xbe | 0xbf, ..]
                    | [0x40..=0x4f, 0x0f, 0xb6 | 0xb7 | 0xbe | 0xbf, ..]
            )
        );
    if operand_override {
        b0 = slice[pc];
        pc += 1;
    }
    let mut rex_w = false;
    let mut op = b0;

    // REX prefix
    if (op & 0xF0) == 0x40 {
        rex_w = (op & 0x08) != 0;
        if pc >= slice.len() {
            return Some(CfgDecoded {
                rva,
                insn_len: pc,
                mnemonic: "truncated".into(),
                is_dbt: false,
                is_interp: false,
                notes: "truncated at REX prefix".into(),
                successors: vec![],
                fall_through: false,
            });
        }
        op = slice[pc];
        pc += 1;
    }

    let mut mnemonic = String::new();
    let mut notes = String::new();
    let mut successors: Vec<u32> = Vec::new();
    let mut fall_through = true;

    match op {
        0xF4 => {
            mnemonic = "hlt".to_string();
            fall_through = false; // HLT terminates
        }
        0x90 => {
            mnemonic = "nop".to_string();
        }
        0xCC => {
            mnemonic = "int3".to_string();
            notes = "debugger breakpoint trap".to_string();
            // INT3 terminates a linear sequence (trap), no fall-through for CFG purposes
            fall_through = false;
        }
        0xC3 => {
            mnemonic = "ret".to_string();
            fall_through = false;
        }
        0xC7 => {
            if let Some(&modrm) = slice.get(pc) {
                pc += 1;
                if modrm & 0x38 != 0 {
                    mnemonic = format!("unsupported c7 extension (modrm 0x{modrm:02x})");
                    notes = "unknown instruction boundary".into();
                    fall_through = false;
                } else {
                    pc += modrm_extra_len(modrm, slice, pc);
                    let imm16 = operand_override && !rex_w;
                    let imm_len = if imm16 { 2 } else { 4 };
                    if let Some(imm) = slice.get(pc..pc + imm_len) {
                        pc += imm_len;
                        let (width, value) = if imm16 {
                            (16, i32::from(i16::from_le_bytes(imm.try_into().unwrap())))
                        } else {
                            (
                                if rex_w { 64 } else { 32 },
                                i32::from_le_bytes(imm.try_into().unwrap()),
                            )
                        };
                        mnemonic = format!("mov r/m{width}, {value} (modrm 0x{modrm:02x})");
                    } else {
                        pc = slice.len();
                        fall_through = false;
                    }
                }
            } else {
                fall_through = false;
            }
            if mnemonic.is_empty() {
                mnemonic = "truncated c7".into();
                notes = "incomplete MOV immediate encoding".into();
            }
        }
        0xC2 => {
            // RETN imm16
            if pc + 2 <= slice.len() {
                pc += 2;
            }
            mnemonic = "retn imm16".to_string();
            notes = "near return with stack pop".to_string();
            fall_through = false;
        }
        0x50..=0x57 => {
            let r = op - 0x50;
            mnemonic = format!("push r{r}");
        }
        0x58..=0x5F => {
            let r = op - 0x58;
            mnemonic = format!("pop r{r}");
        }
        0x01 | 0x03 => {
            if pc < slice.len() {
                let modrm = slice[pc];
                pc += 1;
                pc += modrm_extra_len(modrm, slice, pc);
                mnemonic = format!("add (modrm 0x{modrm:02x})");
                if (modrm >> 6) == 3 {}
            }
        }
        0x29 | 0x2B => {
            if pc < slice.len() {
                let modrm = slice[pc];
                pc += 1;
                pc += modrm_extra_len(modrm, slice, pc);
                mnemonic = format!("sub (modrm 0x{modrm:02x})");
                if (modrm >> 6) == 3 {}
            }
        }
        0x31 | 0x33 => {
            if pc < slice.len() {
                let modrm = slice[pc];
                pc += 1;
                pc += modrm_extra_len(modrm, slice, pc);
                mnemonic = format!("xor (modrm 0x{modrm:02x})");
                if (modrm >> 6) == 3 {}
            }
        }
        // MOVSXD r, r/m32
        0x63 => {
            if let Some((modrm, end)) = modrm_operand(slice, pc) {
                pc = end;
                let width = if rex_w { 64 } else { 32 };
                mnemonic = format!("movsxd r{width}, r/m32 (modrm 0x{modrm:02x})");
            } else {
                pc = slice.len();
                mnemonic = "truncated movsxd".into();
                fall_through = false;
            }
        }
        0x83 => {
            if pc + 1 < slice.len() {
                let modrm = slice[pc];
                let imm = slice[pc + 1];
                pc += 2;
                let sub_op = (modrm >> 3) & 7;
                mnemonic = format!("imm8_group modrm=0x{modrm:02x} imm=0x{imm:02x}");
                if (modrm >> 6) == 3 && (sub_op == 0 || sub_op == 5 || sub_op == 7) {}
            }
        }
        0x85 => {
            // TEST r/m, r
            if pc < slice.len() {
                let modrm = slice[pc];
                pc += 1;
                pc += modrm_extra_len(modrm, slice, pc);
                mnemonic = format!("test (modrm 0x{modrm:02x})");
            }
        }
        0x89 | 0x8B => {
            if pc < slice.len() {
                let modrm = slice[pc];
                pc += 1;
                let mode = (modrm >> 6) & 3;
                let rm = modrm & 7;
                mnemonic = format!("mov (modrm 0x{modrm:02x})");
                if mode == 3 {
                } else if rm == 4 {
                    // SIB
                    if pc < slice.len() {
                        let sib = slice[pc];
                        pc += 1;
                        if mode == 0 && (sib & 7) == 5 && pc + 4 <= slice.len() {
                            pc += 4; // disp32 with no base register
                        } else if mode == 1 && pc < slice.len() {
                            pc += 1; // disp8
                        } else if mode == 2 && pc + 4 <= slice.len() {
                            pc += 4; // disp32
                        }
                        mnemonic = format!("mov [SIB=0x{sib:02x}]");
                    }
                } else {
                    if mode == 1 && pc < slice.len() {
                        pc += 1;
                    } else if (mode == 2 || (mode == 0 && rm == 5)) && pc + 4 <= slice.len() {
                        pc += 4;
                    }
                }
            }
        }
        0xB8..=0xBF => {
            let imm_len = if rex_w { 8 } else { 4 };
            if pc + imm_len <= slice.len() {
                pc += imm_len;
                mnemonic = format!("mov reg, imm{imm_len}");
            }
        }
        // CALL rel32 — both call target and fall-through continuation are successors
        0xE8 => {
            if pc + 4 <= slice.len() {
                let rel32 = i32::from_le_bytes(slice[pc..pc + 4].try_into().unwrap());
                pc += 4;
                let insn_len = pc as i64;
                let target_va = (va as i64 + insn_len + rel32 as i64) as u64;
                mnemonic = format!("call rel32 -> 0x{target_va:016x}");
                // Push call target as a CFG successor; fall-through (return site) continues.
                let call_rva = rva.wrapping_add(pc as u32).wrapping_add(rel32 as u32);
                successors.push(call_rva);
                // fall_through = true (the return site is the sequential instruction)
            }
        }
        // JMP rel32 — unconditional; only the target is a successor, no fall-through
        0xE9 => {
            if pc + 4 <= slice.len() {
                let rel32 = i32::from_le_bytes(slice[pc..pc + 4].try_into().unwrap());
                pc += 4;
                let insn_len = pc as i64;
                let target_va = (va as i64 + insn_len + rel32 as i64) as u64;
                mnemonic = format!("jmp rel32 -> 0x{target_va:016x}");
                let jmp_rva = rva.wrapping_add(pc as u32).wrapping_add(rel32 as u32);
                successors.push(jmp_rva);
                fall_through = false;
            }
        }
        // JMP rel8 — unconditional short jump
        0xEB => {
            if pc < slice.len() {
                let rel8 = slice[pc] as i8;
                pc += 1;
                let insn_len = pc as i64;
                let target_va = (va as i64 + insn_len + rel8 as i64) as u64;
                mnemonic = format!("jmp rel8 -> 0x{target_va:016x}");
                let jmp_rva = (rva as i64 + insn_len + rel8 as i64) as u32;
                successors.push(jmp_rva);
                fall_through = false;
            }
        }
        // Jcc rel8 (0x70..=0x7F) — conditional; both target and fall-through are successors
        0x70..=0x7F => {
            if pc < slice.len() {
                let rel8 = slice[pc] as i8;
                pc += 1;
                let insn_len = pc as i64;
                let target_va = (va as i64 + insn_len + rel8 as i64) as u64;
                mnemonic = format!("j{:x} rel8 -> 0x{target_va:016x}", op & 0xF);
                let jcc_rva = (rva as i64 + insn_len + rel8 as i64) as u32;
                successors.push(jcc_rva);
                // fall_through = true (not-taken path)
            }
        }
        // 0F-prefixed two-byte opcodes (Jcc rel32, etc.)
        0x0F => {
            if pc < slice.len() {
                let op2 = slice[pc];
                pc += 1;
                match op2 {
                    // Jcc rel32 (0F 80..=0F 8F)
                    0x80..=0x8F => {
                        if pc + 4 <= slice.len() {
                            let rel32 = i32::from_le_bytes(slice[pc..pc + 4].try_into().unwrap());
                            pc += 4;
                            let insn_len = pc as i64;
                            let target_va = (va as i64 + insn_len + rel32 as i64) as u64;
                            mnemonic = format!("j{:x} rel32 -> 0x{target_va:016x}", op2 & 0xF);
                            let jcc_rva = (rva as i64 + insn_len + rel32 as i64) as u32;
                            successors.push(jcc_rva);
                            // fall_through = true
                        }
                    }
                    // MOVZX/MOVSX r, r/m8 or r/m16
                    0xB6 | 0xB7 | 0xBE | 0xBF => {
                        if let Some((modrm, end)) = modrm_operand(slice, pc) {
                            pc = end;
                            let name = if op2 < 0xB8 { "movzx" } else { "movsx" };
                            let source = if op2 & 1 == 0 { 8 } else { 16 };
                            let width = if rex_w {
                                64
                            } else if operand_override {
                                16
                            } else {
                                32
                            };
                            mnemonic =
                                format!("{name} r{width}, r/m{source} (modrm 0x{modrm:02x})");
                        } else {
                            pc = slice.len();
                            mnemonic = "truncated extension".into();
                            fall_through = false;
                        }
                    }
                    _ => {
                        mnemonic = format!("0f_{op2:02x}");
                        notes = "unsupported two-byte opcode".to_string();
                    }
                }
            }
        }
        // FF /2 (CALL r/m) and FF /4 (JMP r/m) — indirect, target unknown
        0xFF => {
            if pc < slice.len() {
                let modrm = slice[pc];
                pc += 1;
                let reg_field = (modrm >> 3) & 7;
                if reg_field == 2 || reg_field == 4 {
                    // indirect call/jmp
                    if (modrm & 7) == 5 && pc + 4 <= slice.len() {
                        let disp32 = i32::from_le_bytes(slice[pc..pc + 4].try_into().unwrap());
                        pc += 4;
                        let iat_target = (va as i64 + pc as i64 + disp32 as i64) as u64;
                        if reg_field == 2 {
                            mnemonic = format!("call *[RIP + 0x{iat_target:016x}]");
                            notes = "indirect IAT symbol call - requires dynamic import linking"
                                .to_string();
                            // fall_through = true (return will come back here)
                        } else {
                            mnemonic = format!("jmp *[RIP + 0x{iat_target:016x}]");
                            notes = "indirect jmp - target unknown statically".to_string();
                            fall_through = false; // unconditional, target uncertain
                        }
                    } else {
                        pc += modrm_extra_len(modrm, slice, pc);
                        if reg_field == 2 {
                            mnemonic = format!("call *r/m (modrm 0x{modrm:02x})");
                            notes = "indirect call - target unknown".to_string();
                        } else {
                            mnemonic = format!("jmp *r/m (modrm 0x{modrm:02x})");
                            notes = "indirect jmp - target unknown statically".to_string();
                            fall_through = false;
                        }
                    }
                } else if reg_field == 6 {
                    // PUSH r/m
                    pc += modrm_extra_len(modrm, slice, pc);
                    mnemonic = format!("push r/m (modrm 0x{modrm:02x})");
                } else {
                    pc += modrm_extra_len(modrm, slice, pc);
                    mnemonic = format!("ff_group_r{reg_field} (modrm 0x{modrm:02x})");
                    notes = "unsupported FF-group opcode".to_string();
                }
            }
        }
        _ => {
            mnemonic = format!("opcode_0x{op:02x}");
            notes = "unsupported opcode".to_string();
        }
    }

    // Architectural instruction-length enforcement: no decoded instruction
    // may exceed 15 bytes, whatever prefix or opcode path produced it.
    if pc > MAX_INSTRUCTION_LEN {
        return Some(CfgDecoded {
            rva,
            insn_len: MAX_INSTRUCTION_LEN,
            mnemonic: "overlong instruction".into(),
            is_dbt: false,
            is_interp: false,
            notes: "instruction length exceeds the 15-byte architectural limit".into(),
            successors: vec![],
            fall_through: false,
        });
    }

    // The decoder both engines execute with decides support, length and static
    // control flow, so the report cannot claim a form they reject.
    let (is_dbt, is_interp) = match decode(va, |address| {
        slice
            .get(address.wrapping_sub(va) as usize)
            .copied()
            .ok_or(())
    }) {
        Ok(insn) => {
            pc = usize::from(insn.len);
            if mnemonic.is_empty() || mnemonic.contains("opcode_0x") || mnemonic.starts_with("0f_")
            {
                mnemonic = insn.op.name().to_string();
                notes.clear();
            }
            if !is_translatable(&insn) {
                notes = "raw guest system calls are not dispatched by the DBT".into();
            } else if !is_native(&insn) {
                notes = "executed by the shared semantics, not translated to ARM64".into();
            }
            let relative = insn.encoding == Encoding::Rel;
            if relative {
                successors = vec![insn.imm.wrapping_sub(va).wrapping_add(u64::from(rva)) as u32];
            }
            fall_through = !matches!(insn.op, Op::Jmp | Op::Ret | Op::Hlt);
            (is_translatable(&insn), true)
        }
        Err(_) => (false, false),
    };

    Some(CfgDecoded {
        rva,
        insn_len: pc,
        mnemonic,
        is_dbt,
        is_interp,
        notes,
        successors,
        fall_through,
    })
}

/// Compute the number of additional bytes after ModRM that the addressing mode requires.
/// Does NOT include the ModRM byte itself.
fn modrm_extra_len(modrm: u8, slice: &[u8], after_modrm: usize) -> usize {
    let mode = (modrm >> 6) & 3;
    let rm = modrm & 7;
    if mode == 3 {
        return 0; // register operand, no memory bytes
    }
    let mut extra = 0;
    if rm == 4 {
        extra += 1; // SIB byte
    }
    if mode == 1 {
        extra += 1; // disp8
    } else if mode == 2 {
        extra += 4; // disp32
    } else if mode == 0
        && (rm == 5 || (rm == 4 && slice.get(after_modrm).is_some_and(|sib| sib & 7 == 5)))
    {
        extra += 4; // RIP-relative or SIB with no base
    }
    // Clamp to what's actually available
    extra.min(slice.len().saturating_sub(after_modrm))
}

/// An entry in the detailed relocation census.
#[derive(Clone, Debug, Default)]
pub struct RelocationCensus {
    pub total_entries: usize,
    pub type_counts: BTreeMap<u8, usize>,
    pub base_reloc_rva: u32,
    pub base_reloc_size: u32,
}

/// TLS metadata census without invocation.
#[derive(Clone, Debug, Default)]
pub struct TlsCensus {
    pub raw_data_start_va: u64,
    pub raw_data_end_va: u64,
    pub index_address: u64,
    pub callbacks_address: u64,
    pub zero_fill_size: u32,
    pub characteristics: u32,
    pub callback_targets: Vec<u64>,
}

/// Single imported symbol descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedSymbol {
    pub dll: String,
    pub symbol: String,
    pub import_name: ImportName,
    pub iat_rva: u32,
    pub category: &'static str,
    /// The standard synthetic registry has a handler for this exact key.
    pub is_supported: bool,
    pub service_status: Option<ServiceStatus>,
    pub canonical_module: Option<String>,
    pub canonical_symbol: Option<String>,
}

impl ImportedSymbol {
    fn coverage_label(&self) -> &'static str {
        if let Some(status) = self.service_status {
            return status.as_str();
        }
        // Explicitly tracked public APIs whose prerequisites are still missing.
        match (self.dll.to_ascii_lowercase().as_str(), self.symbol.as_str()) {
            (
                "api-ms-win-core-processthreads-l1-1-0.dll",
                "CreateThread" | "ResumeThread" | "SuspendThread" | "TerminateProcess",
            )
            | (
                "api-ms-win-core-synch-l1-1-0.dll",
                "WaitForSingleObject" | "WaitForSingleObjectEx",
            )
            | ("api-ms-win-core-synch-l1-2-1.dll", "WaitForMultipleObjects")
            | ("api-ms-win-core-rtlsupport-l1-1-0.dll", "RtlVirtualUnwind") => "unsupported",
            _ => "missing",
        }
    }

    fn startup_priority(&self) -> (u8, &'static str) {
        let dll = self.dll.to_ascii_lowercase();
        if dll.contains("processthreads") || dll.contains("core-handle") {
            (1, "process and thread identity and handles")
        } else if dll.contains("fibers") || matches!(self.symbol.get(..3), Some("Tls" | "Fls")) {
            (2, "thread and fiber local storage")
        } else if dll.contains("core-synch") {
            (3, "synchronization")
        } else if dll.contains("core-profile") || dll.contains("core-sysinfo") {
            (4, "clocks and system information")
        } else if dll.contains("core-memory") || dll.contains("core-heap") || dll == "xmem.dll" {
            (5, "virtual memory and allocation lifecycle")
        } else if dll.contains("rtlsupport")
            || dll.contains("errorhandling")
            || self.symbol.starts_with("__CxxFrameHandler")
        {
            (7, "exceptions and unwind")
        } else if self.category == "C/C++ CRT Runtime" || dll.contains("libraryloader") {
            (6, "C/C++ runtime and module initialization")
        } else if dll.contains("core-file") || dll == "xcrdapi.dll" {
            (8, "file and package I/O")
        } else {
            (9, "platform services and graphics")
        }
    }
}

/// Decoded instruction coverage record.
#[derive(Clone, Debug)]
pub struct InstructionRecord {
    pub rva: u32,
    pub va: u64,
    pub bytes: Vec<u8>,
    pub mnemonic: String,
    pub is_supported_dbt: bool,
    pub is_supported_interp: bool,
    pub notes: String,
}

/// Full diagnostic probe report.
#[derive(Clone, Debug)]
pub struct ProbeReport {
    pub file_size: usize,
    pub sha256: String,
    pub parsed: ParsedPe,
    pub relocations: RelocationCensus,
    pub tls: TlsCensus,
    /// Imports before the first malformed entry, if any.
    pub imports: Vec<ImportedSymbol>,
    /// Why the import table could not be read to its end.
    pub import_error: Option<ImportTableError>,
    /// Delay-loaded imports before the first malformed entry, if any. Binding
    /// does not resolve them, so `imports` and its totals leave them out.
    pub delayed_imports: Vec<ImportedSymbol>,
    /// Why the delay-load import tables could not be read to their end.
    pub delay_import_error: Option<ImportTableError>,
    pub instruction_records: Vec<InstructionRecord>,
    pub first_unsupported_dbt_insn: Option<InstructionRecord>,
}

impl ProbeReport {
    /// Categorize DLL name into functional subsystem.
    pub fn categorize_dll(dll: &str) -> &'static str {
        let n = dll.to_ascii_lowercase();
        if n == "d3d12_xs.dll"
            || n == "xg_xs.dll"
            || n == "xboxbvhbuilder.dll"
            || n == "amd_fidelityfx_loader_xbox.dll"
            || n == "amd_fidelityfx_upscaler_xbox.dll"
        {
            "Xbox Graphics & Compute"
        } else if n == "xmem.dll" {
            "Xbox Memory Management"
        } else if n == "acphal.dll" || n == "xapu.dll" || n == "chad.dll" {
            "Xbox Audio / Hardware DSP"
        } else if n == "gameinput.dll" {
            "Xbox Input"
        } else if n == "xcrdapi.dll" {
            "Xbox Storage (DirectStorage / XCRDAPI)"
        } else if n == "bink2scarlett.dll" {
            "Multimedia / Video"
        } else if n == "libhttpclient.gdk.dll" || n == "xcurl.dll" {
            "Xbox Networking / GDK HTTP"
        } else if n.starts_with("api-ms-win-crt-")
            || n == "msvcp140.dll"
            || n == "vcruntime140.dll"
            || n == "vcruntime140_1.dll"
            || n == "concrt140.dll"
            || n == "vcomp140.dll"
            || n == "vccorlib140.dll"
        {
            "C/C++ CRT Runtime"
        } else if n.starts_with("api-ms-win-core-")
            || n.starts_with("ext-ms-win-")
            || n == "advapi32.dll"
            || n == "ws2_32.dll"
            || n == "winhttp.dll"
            || n == "bcrypt.dll"
            || n == "mfplat.dll"
            || n == "mfreadwrite.dll"
        {
            "Windows Core Subsystems"
        } else {
            "Other / Unknown"
        }
    }
}

/// Perform a strictly read-only, non-executing PE probe.
pub fn probe_pe_bytes(bytes: &[u8], config: &ProbeConfig) -> Result<ProbeReport, PeError> {
    if bytes.len() < 0x40 {
        return Err(PeError::TooSmall);
    }
    if &bytes[0..2] != b"MZ" {
        return Err(PeError::InvalidDosSignature);
    }

    let e_lfanew = u32::from_le_bytes(bytes[0x3C..0x40].try_into().unwrap()) as usize;
    if bytes.len() < e_lfanew + 24 {
        return Err(PeError::TooSmall);
    }
    if &bytes[e_lfanew..e_lfanew + 4] != b"PE\0\0" {
        return Err(PeError::InvalidPeSignature);
    }

    let coff_offset = e_lfanew + 4;
    let machine = u16::from_le_bytes(bytes[coff_offset..coff_offset + 2].try_into().unwrap());
    if machine != IMAGE_FILE_MACHINE_AMD64 {
        return Err(PeError::UnsupportedMachine(machine));
    }
    let number_of_sections =
        u16::from_le_bytes(bytes[coff_offset + 2..coff_offset + 4].try_into().unwrap());
    let time_date_stamp =
        u32::from_le_bytes(bytes[coff_offset + 4..coff_offset + 8].try_into().unwrap());
    let size_of_optional_header = u16::from_le_bytes(
        bytes[coff_offset + 16..coff_offset + 18]
            .try_into()
            .unwrap(),
    ) as usize;

    let opt_offset = coff_offset + 20;
    if bytes.len() < opt_offset + size_of_optional_header {
        return Err(PeError::TooSmall);
    }

    let magic = u16::from_le_bytes(bytes[opt_offset..opt_offset + 2].try_into().unwrap());
    if magic != PE32_PLUS_MAGIC {
        return Err(PeError::InvalidMagic(magic));
    }

    let entry_point_rva =
        u32::from_le_bytes(bytes[opt_offset + 16..opt_offset + 20].try_into().unwrap());
    let image_base =
        u64::from_le_bytes(bytes[opt_offset + 24..opt_offset + 32].try_into().unwrap());
    let section_alignment =
        u32::from_le_bytes(bytes[opt_offset + 32..opt_offset + 36].try_into().unwrap());
    let file_alignment =
        u32::from_le_bytes(bytes[opt_offset + 36..opt_offset + 40].try_into().unwrap());
    let size_of_image =
        u32::from_le_bytes(bytes[opt_offset + 56..opt_offset + 60].try_into().unwrap());
    let size_of_headers =
        u32::from_le_bytes(bytes[opt_offset + 60..opt_offset + 64].try_into().unwrap());

    // Data directories
    let import_dir_rva = if size_of_optional_header >= 128 {
        u32::from_le_bytes(
            bytes[opt_offset + 120..opt_offset + 124]
                .try_into()
                .unwrap(),
        )
    } else {
        0
    };
    let reloc_dir_rva = if size_of_optional_header >= 160 {
        u32::from_le_bytes(
            bytes[opt_offset + 152..opt_offset + 156]
                .try_into()
                .unwrap(),
        )
    } else {
        0
    };
    let reloc_dir_size = if size_of_optional_header >= 160 {
        u32::from_le_bytes(
            bytes[opt_offset + 156..opt_offset + 160]
                .try_into()
                .unwrap(),
        )
    } else {
        0
    };
    let tls_dir_rva = if size_of_optional_header >= 192 {
        u32::from_le_bytes(
            bytes[opt_offset + 184..opt_offset + 188]
                .try_into()
                .unwrap(),
        )
    } else {
        0
    };
    // The delay import descriptor is data directory 13. It exists only when
    // NumberOfRvaAndSizes covers it.
    let (delay_dir_rva, delay_dir_size) = if size_of_optional_header >= 224
        && u32::from_le_bytes(
            bytes[opt_offset + 108..opt_offset + 112]
                .try_into()
                .unwrap(),
        ) > 13
    {
        (
            u32::from_le_bytes(
                bytes[opt_offset + 216..opt_offset + 220]
                    .try_into()
                    .unwrap(),
            ),
            u32::from_le_bytes(
                bytes[opt_offset + 220..opt_offset + 224]
                    .try_into()
                    .unwrap(),
            ),
        )
    } else {
        (0, 0)
    };

    let section_table_offset = opt_offset + size_of_optional_header;
    let section_header_size = 40;
    if bytes.len() < section_table_offset + (number_of_sections as usize * section_header_size) {
        return Err(PeError::InvalidSectionHeaders);
    }

    let mut sections = Vec::with_capacity(number_of_sections as usize);
    for i in 0..number_of_sections as usize {
        let offset = section_table_offset + i * section_header_size;
        let sec_bytes = &bytes[offset..offset + section_header_size];

        let name_raw = &sec_bytes[0..8];
        let name_len = name_raw.iter().position(|&b| b == 0).unwrap_or(8);
        let name = String::from_utf8_lossy(&name_raw[..name_len]).into_owned();

        let virtual_size = u32::from_le_bytes(sec_bytes[8..12].try_into().unwrap());
        let virtual_address = u32::from_le_bytes(sec_bytes[12..16].try_into().unwrap());
        let raw_data_size = u32::from_le_bytes(sec_bytes[16..20].try_into().unwrap());
        let raw_data_ptr = u32::from_le_bytes(sec_bytes[20..24].try_into().unwrap());
        let characteristics = u32::from_le_bytes(sec_bytes[36..40].try_into().unwrap());

        let permissions = MemoryPermissions {
            read: (characteristics & 0x4000_0000) != 0,
            write: (characteristics & 0x8000_0000) != 0,
            execute: (characteristics & 0x2000_0000) != 0,
        };

        sections.push(PeSection {
            name,
            virtual_address,
            virtual_size,
            raw_data_ptr,
            raw_data_size,
            characteristics,
            permissions,
        });
    }

    // Map an RVA to a file offset.
    //
    // The offset is derived from the section header, so on a truncated file it
    // can point past the end of the buffer. Such an offset is not readable, so
    // it is reported as unmapped rather than returned and sliced later.
    let rva_to_offset = |rva: u32| -> Option<usize> {
        for sec in &sections {
            let max_span = std::cmp::max(sec.virtual_size, sec.raw_data_size);
            if rva >= sec.virtual_address && rva < sec.virtual_address.saturating_add(max_span) {
                let off_in_sec = (rva - sec.virtual_address) as usize;
                let off = sec.raw_data_ptr as usize + off_in_sec;
                if off >= bytes.len() {
                    return None;
                }
                return Some(off);
            }
        }
        None
    };

    // 1. Relocations Census
    let mut relocations = RelocationCensus {
        total_entries: 0,
        type_counts: BTreeMap::new(),
        base_reloc_rva: reloc_dir_rva,
        base_reloc_size: reloc_dir_size,
    };
    if reloc_dir_rva != 0
        && reloc_dir_size > 0
        && let Some(reloc_off) = rva_to_offset(reloc_dir_rva)
    {
        let mut read_bytes = 0usize;
        let max_read = std::cmp::min(
            reloc_dir_size as usize,
            bytes.len().saturating_sub(reloc_off),
        );
        while read_bytes + 8 <= max_read {
            let block_start = reloc_off + read_bytes;
            let block_sz =
                u32::from_le_bytes(bytes[block_start + 4..block_start + 8].try_into().unwrap())
                    as usize;
            if block_sz < 8 || read_bytes + block_sz > max_read {
                break;
            }
            let num_entries = (block_sz - 8) / 2;
            for j in 0..num_entries {
                let e_off = block_start + 8 + j * 2;
                let val = u16::from_le_bytes(bytes[e_off..e_off + 2].try_into().unwrap());
                let rtype = (val >> 12) as u8;
                *relocations.type_counts.entry(rtype).or_insert(0) += 1;
                relocations.total_entries += 1;
            }
            read_bytes += block_sz;
        }
    }

    // 2. TLS Census
    let mut tls = TlsCensus::default();
    if tls_dir_rva != 0
        && let Some(tls_off) = rva_to_offset(tls_dir_rva)
        && tls_off + 40 <= bytes.len()
    {
        tls.raw_data_start_va = u64::from_le_bytes(bytes[tls_off..tls_off + 8].try_into().unwrap());
        tls.raw_data_end_va =
            u64::from_le_bytes(bytes[tls_off + 8..tls_off + 16].try_into().unwrap());
        tls.index_address =
            u64::from_le_bytes(bytes[tls_off + 16..tls_off + 24].try_into().unwrap());
        tls.callbacks_address =
            u64::from_le_bytes(bytes[tls_off + 24..tls_off + 32].try_into().unwrap());
        tls.zero_fill_size =
            u32::from_le_bytes(bytes[tls_off + 32..tls_off + 36].try_into().unwrap());
        tls.characteristics =
            u32::from_le_bytes(bytes[tls_off + 36..tls_off + 40].try_into().unwrap());

        if tls.callbacks_address >= image_base {
            let cb_rva = (tls.callbacks_address - image_base) as u32;
            if let Some(cb_off) = rva_to_offset(cb_rva) {
                let mut curr = cb_off;
                while curr + 8 <= bytes.len() {
                    let target = u64::from_le_bytes(bytes[curr..curr + 8].try_into().unwrap());
                    if target == 0 {
                        break;
                    }
                    tls.callback_targets.push(target);
                    curr += 8;
                    if tls.callback_targets.len() >= 64 {
                        break;
                    }
                }
            }
        }
    }

    // 3. Import Census
    let mut registry = ImportRegistry::new();
    register_standard_services(
        &mut registry,
        Arc::new(ServicesState::new(Arc::new(MockClock::new(0)))),
    );
    // Binding accepts only bytes backed by section data; match it exactly.
    let backed_offset = |rva: u32| {
        sections.iter().find_map(|section| {
            let offset = rva.checked_sub(section.virtual_address)?;
            (offset < section.raw_data_size)
                .then(|| (section.raw_data_ptr as usize).checked_add(offset as usize))?
        })
    };
    let (entries, import_error) = if import_dir_rva == 0 {
        (Vec::new(), None)
    } else {
        parse_imports(bytes, import_dir_rva, backed_offset)
    };
    let (delayed_entries, delay_import_error) = if delay_dir_rva == 0 {
        (Vec::new(), None)
    } else {
        parse_delay_imports(bytes, delay_dir_rva, delay_dir_size, backed_offset)
    };
    let describe = |entry: ImportEntry| {
        let binding = registry.resolve(&entry.module, &entry.name);
        ImportedSymbol {
            category: ProbeReport::categorize_dll(&entry.module),
            dll: entry.module,
            symbol: entry.name.to_string(),
            import_name: entry.name,
            iat_rva: entry.iat_rva,
            is_supported: binding.is_some(),
            service_status: binding.map(|service| service.status),
            canonical_module: binding.map(|service| service.module.clone()),
            canonical_symbol: binding.map(|service| service.symbol.clone()),
        }
    };
    let imports: Vec<ImportedSymbol> = entries.into_iter().map(&describe).collect();
    let delayed_imports: Vec<ImportedSymbol> = delayed_entries.into_iter().map(&describe).collect();

    // 4. Control-Flow-Aware Static Disassembly
    //
    // Use a worklist (priority queue ordered by ascending RVA) rather than a
    // linear sweep.  Seeds:
    //   - Entry point RVA
    //   - Every TLS callback RVA (converted from VA)
    //
    // For each decoded instruction:
    //   - If unconditional JMP/RET/HLT/INT3: do not add sequential fall-through.
    //   - If conditional branch / CALL: add both the branch target and the
    //     fall-through continuation.
    //   - If indirect branch (target unknown): record as "uncertain" and stop
    //     that path.
    //
    // Track visited RVAs so each instruction is decoded at most once.
    let mut instruction_records: Vec<InstructionRecord> = Vec::new();
    let mut first_unsupported_dbt_insn: Option<InstructionRecord> = None;

    if let Some(_entry_offset) = rva_to_offset(entry_point_rva) {
        // worklist: min-heap of RVAs to visit (ascending order for stable output)
        let mut worklist: BinaryHeap<Reverse<u32>> = BinaryHeap::new();
        let mut visited: BTreeSet<u32> = BTreeSet::new();
        let mut worklist_entries_used = 0usize;
        let total_budget = config.max_instructions;
        let worklist_budget = config.max_worklist_entries;

        worklist.push(Reverse(entry_point_rva));

        // Seed TLS callback RVAs
        for &cb_va in &tls.callback_targets {
            if cb_va >= image_base {
                let cb_rva = (cb_va - image_base) as u32;
                worklist.push(Reverse(cb_rva));
            }
        }

        while let Some(Reverse(rva)) = worklist.pop() {
            if visited.contains(&rva) {
                continue;
            }
            if instruction_records.len() >= total_budget {
                break;
            }
            if worklist_entries_used >= worklist_budget {
                break;
            }
            worklist_entries_used += 1;

            let Some(file_off) = rva_to_offset(rva) else {
                // RVA out of section bounds — skip
                continue;
            };

            if file_off >= bytes.len() {
                continue;
            }

            // Count bytes already visited starting from this RVA to enforce byte budget
            let budget_start_insns = instruction_records.len();
            if budget_start_insns > 0 {
                let already_decoded: usize =
                    instruction_records.iter().map(|r| r.bytes.len()).sum();
                if already_decoded >= config.max_disasm_bytes {
                    break;
                }
            }

            let avail = &bytes[file_off..];
            let insn_va = image_base + rva as u64;

            let Some(decoded) = decode_one(avail, rva, insn_va) else {
                continue;
            };
            if decoded.insn_len == 0 {
                continue;
            }

            // Mark this RVA visited before pushing successors to avoid duplicates
            visited.insert(rva);

            let insn_bytes = avail[..decoded.insn_len].to_vec();
            let rec = InstructionRecord {
                rva,
                va: insn_va,
                bytes: insn_bytes,
                mnemonic: decoded.mnemonic.clone(),
                is_supported_dbt: decoded.is_dbt,
                is_supported_interp: decoded.is_interp,
                notes: decoded.notes.clone(),
            };

            if !decoded.is_dbt && first_unsupported_dbt_insn.is_none() {
                first_unsupported_dbt_insn = Some(rec.clone());
            }
            instruction_records.push(rec);

            // Push successors from known branch targets
            for &target_rva in &decoded.successors {
                if !visited.contains(&target_rva) {
                    worklist.push(Reverse(target_rva));
                }
            }

            // Push sequential fall-through
            if decoded.fall_through {
                let next_rva = rva.saturating_add(decoded.insn_len as u32);
                if !visited.contains(&next_rva) {
                    worklist.push(Reverse(next_rva));
                }
            }
        }
    }

    let parsed_pe = ParsedPe {
        machine,
        number_of_sections,
        time_date_stamp,
        image_base,
        section_alignment,
        file_alignment,
        size_of_image,
        size_of_headers,
        entry_point_rva,
        sections,
    };

    Ok(ProbeReport {
        file_size: bytes.len(),
        sha256: format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(bytes)),
        parsed: parsed_pe,
        relocations,
        tls,
        imports,
        import_error,
        delayed_imports,
        delay_import_error,
        instruction_records,
        first_unsupported_dbt_insn,
    })
}

/// Probe PE bytes with `config` and generate artifacts in the output directory.
pub fn run_probe_and_export<O: AsRef<Path>>(
    bytes: &[u8],
    output_dir: O,
    config: &ProbeConfig,
) -> Result<ProbeReport, Box<dyn std::error::Error>> {
    let report = probe_pe_bytes(bytes, config)?;

    // 1. imports.csv
    let mut imports_csv = String::from(
        "module,symbol_or_ordinal,iat_rva,import_table,category,registered_binding,implementation_status,is_alias,canonical_module,startup_priority,startup_cluster,trusted_runner_binding,import_kind\n",
    );
    let mut missing_csv = String::from("module,symbol_or_ordinal,iat_rva,import_kind\n");
    let mut modules = BTreeSet::new();
    let mut unique_symbols = BTreeSet::new();
    let mut distinct_imports = BTreeSet::new();
    let mut canonical_bindings = BTreeSet::new();
    let mut counts = BTreeMap::<&str, usize>::new();
    let mut clusters = BTreeMap::<(u8, &str), (usize, usize)>::new();
    let mut aliases = 0;
    // The runner admits exactly these bytes and installs a private draw handler.
    let trusted_fixture = bytes == include_bytes!("../../../fixtures/pe/trusted-runner.exe")
        || bytes == include_bytes!("../../../fixtures/pe/runtime-services.exe");
    let mut trusted_runner_slots = 0;
    let mut missing = 0;
    for imp in &report.imports {
        let module_key = imp.dll.to_ascii_lowercase();
        modules.insert(module_key.clone());
        let is_ordinal = matches!(imp.import_name, ImportName::Ordinal(_));
        let import_kind = if is_ordinal { "ordinal" } else { "name" };
        unique_symbols.insert((is_ordinal.then_some(module_key.clone()), &imp.import_name));
        distinct_imports.insert((module_key, &imp.import_name));
        let module = imp.dll.replace('"', "\"\"");
        let symbol = imp.symbol.replace('"', "\"\"");
        let key = format!("\"{module}\",\"{symbol}\",0x{:08x}", imp.iat_rva);
        let trusted_binding = trusted_fixture
            && imp.dll.eq_ignore_ascii_case("xenora.dll")
            && imp.symbol == "fixture_draw";
        trusted_runner_slots += usize::from(trusted_binding);
        let status = imp.coverage_label();
        let canonical = imp.canonical_module.as_deref().unwrap_or("");
        let is_alias = imp.canonical_module.is_some() && !imp.dll.eq_ignore_ascii_case(canonical);
        if let Some(symbol) = &imp.canonical_symbol {
            canonical_bindings.insert((canonical, symbol));
        }
        aliases += usize::from(is_alias);
        *counts.entry(status).or_default() += 1;
        let (priority, cluster) = imp.startup_priority();
        let group = clusters.entry((priority, cluster)).or_default();
        group.0 += 1;
        group.1 += usize::from(!imp.is_supported);
        let canonical = canonical.replace('"', "\"\"");
        let _ = writeln!(
            imports_csv,
            "{key},ordinary,{},{},{status},{is_alias},\"{canonical}\",{priority},{cluster},{trusted_binding},{import_kind}",
            imp.category, imp.is_supported
        );
        if !imp.is_supported {
            let _ = writeln!(missing_csv, "{key},{import_kind}");
            missing += 1;
        }
    }
    // Delay-loaded imports follow the ordinary rows. Binding does not resolve
    // them, so they stay out of every ordinary total, the startup clusters and
    // `missing_imports.csv`.
    let mut delayed_modules = BTreeSet::new();
    for imp in &report.delayed_imports {
        delayed_modules.insert(imp.dll.to_ascii_lowercase());
        let is_ordinal = matches!(imp.import_name, ImportName::Ordinal(_));
        let import_kind = if is_ordinal { "ordinal" } else { "name" };
        let module = imp.dll.replace('"', "\"\"");
        let symbol = imp.symbol.replace('"', "\"\"");
        let canonical = imp.canonical_module.as_deref().unwrap_or("");
        let is_alias = imp.canonical_module.is_some() && !imp.dll.eq_ignore_ascii_case(canonical);
        let canonical = canonical.replace('"', "\"\"");
        let (priority, cluster) = imp.startup_priority();
        let _ = writeln!(
            imports_csv,
            "\"{module}\",\"{symbol}\",0x{:08x},delayed,{},{},{},{is_alias},\"{canonical}\",{priority},{cluster},false,{import_kind}",
            imp.iat_rva,
            imp.category,
            imp.is_supported,
            imp.coverage_label()
        );
    }
    let delayed_slots = report.delayed_imports.len();
    let delayed_dlls = delayed_modules.len();
    let delay_error = report
        .delay_import_error
        .map_or_else(|| "none".to_string(), |error| error.to_string());

    let implemented = counts.get("implemented").copied().unwrap_or_default();
    let partial = counts.get("partial").copied().unwrap_or_default();
    let stubs = counts.get("stub").copied().unwrap_or_default();
    let unsupported = counts.get("unsupported").copied().unwrap_or_default();
    let unknown = counts.get("missing").copied().unwrap_or_default();

    // 2. cpu_coverage.csv
    let mut cpu_csv = String::from("rva,va,bytes,mnemonic,dbt_supported,interp_supported,notes\n");
    for insn in &report.instruction_records {
        let hex_bytes = insn
            .bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        let _ = writeln!(
            cpu_csv,
            "0x{:08x},0x{:016x},\"{}\",\"{}\",{},{},\"{}\"",
            insn.rva,
            insn.va,
            hex_bytes,
            insn.mnemonic,
            insn.is_supported_dbt,
            insn.is_supported_interp,
            insn.notes
        );
    }

    // 3. candidate_static_report.json
    let mut json = String::new();
    let _ = write!(
        json,
        r#"{{
  "sha256": "{}",
  "file_size": {},
  "image_base": "0x{:016x}",
  "entry_point_rva": "0x{:08x}",
  "size_of_image": "0x{:08x}",
  "sections_count": {},
  "total_imports": {},
  "imported_dlls_count": {},
  "unresolved_imports": {},
  "trusted_runner_binding_slots": {trusted_runner_slots},
  "distinct_import_keys": {},
  "distinct_symbol_names_and_scoped_ordinals": {},
  "canonical_registered_services": {},
  "alias_slots": {aliases},
  "implemented_slots": {implemented},
  "partial_slots": {partial},
  "stub_slots": {stubs},
  "recognized_unsupported_slots": {unsupported},
  "missing_handler_slots": {unknown},
  "delayed_imports": {delayed_slots},
  "delayed_imported_dlls_count": {delayed_dlls},
  "delay_import_error": "{delay_error}",
  "total_relocations": {},
  "dir64_relocations": {},
  "tls_callbacks_count": {},
  "first_unsupported_dbt_rva": "{}"
}}
"#,
        report.sha256,
        report.file_size,
        report.parsed.image_base,
        report.parsed.entry_point_rva,
        report.parsed.size_of_image,
        report.parsed.sections.len(),
        report.imports.len(),
        modules.len(),
        missing,
        distinct_imports.len(),
        unique_symbols.len(),
        canonical_bindings.len(),
        report.relocations.total_entries,
        report
            .relocations
            .type_counts
            .get(&10)
            .copied()
            .unwrap_or(0),
        report.tls.callback_targets.len(),
        report
            .first_unsupported_dbt_insn
            .as_ref()
            .map(|i| format!("0x{:08x}", i.rva))
            .unwrap_or_else(|| "none".to_string())
    );

    // 4. candidate_static_report.md
    let mut md = String::new();
    let _ = writeln!(md, "# Xenora static PE probe report\n");
    let _ = writeln!(md, "## Executable Metadata");
    let _ = writeln!(md, "- **SHA-256**: `{}`", report.sha256);
    let _ = writeln!(md, "- **File Size**: `{}` bytes", report.file_size);
    let _ = writeln!(
        md,
        "- **Image Base**: `0x{:016x}`",
        report.parsed.image_base
    );
    let _ = writeln!(
        md,
        "- **Entry Point RVA**: `0x{:08x}`",
        report.parsed.entry_point_rva
    );
    let _ = writeln!(
        md,
        "- **Size of Image**: `0x{:08x}`",
        report.parsed.size_of_image
    );
    let _ = writeln!(md, "- **Section Alignment**: `4096` bytes");
    let _ = writeln!(md, "- **File Alignment**: `512` bytes\n");

    let _ = writeln!(md, "## Sections Census ({})", report.parsed.sections.len());
    for s in &report.parsed.sections {
        let _ = writeln!(
            md,
            "- `{:<8}` RVA: `0x{:08x}` VirtualSize: `0x{:08x}` RawSize: `0x{:08x}` Perms: [{}{}{}]",
            s.name,
            s.virtual_address,
            s.virtual_size,
            s.raw_data_size,
            if s.permissions.read { "R" } else { "-" },
            if s.permissions.write { "W" } else { "-" },
            if s.permissions.execute { "X" } else { "-" }
        );
    }

    let _ = writeln!(md, "\n## Base Relocations");
    let _ = writeln!(
        md,
        "- **Total Entries**: `{}`",
        report.relocations.total_entries
    );
    for (t, count) in &report.relocations.type_counts {
        let name = match t {
            0 => "ABSOLUTE (padding)",
            10 => "DIR64",
            _ => "OTHER",
        };
        let _ = writeln!(md, "- Type `{t}` (`{name}`): `{count}`");
    }

    let _ = writeln!(md, "\n## TLS Metadata");
    let _ = writeln!(
        md,
        "- **RawData**: `0x{:016x}`..`0x{:016x}`",
        report.tls.raw_data_start_va, report.tls.raw_data_end_va
    );
    let _ = writeln!(
        md,
        "- **Index Address**: `0x{:016x}`",
        report.tls.index_address
    );
    let _ = writeln!(
        md,
        "- **Callbacks Array**: `0x{:016x}`",
        report.tls.callbacks_address
    );
    let _ = writeln!(
        md,
        "- **Declared Callbacks ({})**:",
        report.tls.callback_targets.len()
    );
    for cb in &report.tls.callback_targets {
        let _ = writeln!(md, "  - `0x{:016x}`", cb);
    }

    let _ = writeln!(md, "\n## Import Statistics");
    let _ = writeln!(md, "- **Total Imported DLLs**: `{}`", modules.len());
    let _ = writeln!(
        md,
        "- **Total Imported Thunks**: `{}`",
        report.imports.len()
    );
    let _ = writeln!(
        md,
        "- **Unresolved standard import thunks**: `{missing}`. See `missing_imports.csv`. A registered handler may be a stub.\n"
    );

    if let Some(error) = &report.import_error {
        let _ = writeln!(
            md,
            "- **Import table is malformed**: {error}. Only the imports before it are listed, and binding refuses the image.\n"
        );
    }
    let _ = writeln!(
        md,
        "- **Delay-Loaded Import Slots**: `{delayed_slots}` across `{delayed_dlls}` DLLs. They are marked `delayed` in `imports.csv` and are not part of the counts above or of `missing_imports.csv`: binding does not resolve them, and the PE format leaves them to the image's delay-load helper.\n"
    );
    if let Some(error) = &report.delay_import_error {
        let _ = writeln!(
            md,
            "- **Delay-load import table is malformed**: {error}. Only the delay-loaded imports before it are listed.\n"
        );
    }
    if trusted_runner_slots > 0 {
        let _ = writeln!(
            md,
            "The trusted runner supplies `xenora.dll!fixture_draw` for this fixture. The standard registry counts above exclude that private handler.\n"
        );
    }

    let _ = writeln!(md, "## First Static DBT Blocker");
    if let Some(insn) = &report.first_unsupported_dbt_insn {
        let hex_bytes = insn
            .bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        let _ = writeln!(
            md,
            "- **RVA**: `0x{:08x}` (VA: `0x{:016x}`)",
            insn.rva, insn.va
        );
        let _ = writeln!(md, "- **Bytes**: `{}`", hex_bytes);
        let _ = writeln!(md, "- **Mnemonic**: `{}`", insn.mnemonic);
        let _ = writeln!(md, "- **Reason**: `{}`", insn.notes);
    }

    let mut matrix = String::from(
        "# Runtime import coverage\n\n         Priorities reflect startup prerequisites and shared runtime dependencies. Static imports do not establish call order.\n\n         | Priority | Cluster | IAT slots | Unresolved |\n         | --- | --- | ---: | ---: |\n",
    );
    for ((priority, cluster), (total, unresolved)) in clusters {
        let _ = writeln!(
            matrix,
            "| {priority} | {cluster} | {total} | {unresolved} |"
        );
    }
    let _ = writeln!(
        matrix,
        "\nThe ordinary import table contains {} slots across {} DLLs, {} distinct DLL and symbol keys, and {} distinct names or DLL-scoped ordinals.",
        report.imports.len(),
        modules.len(),
        distinct_imports.len(),
        unique_symbols.len()
    );
    if delayed_slots > 0 {
        let _ = writeln!(
            matrix,
            "\nThe delay-load import tables contain {delayed_slots} more slots across {delayed_dlls} DLLs. They are not counted in this document."
        );
    }
    let _ = writeln!(
        matrix,
        "\nRegistered slots include {implemented} implemented, {partial} partial, and {stubs} stubs. They reference {} canonical registered services through {aliases} alias slots. The {missing} unresolved slots include {unsupported} explicitly tracked unsupported APIs and {unknown} other missing handlers.",
        canonical_bindings.len()
    );

    // Keep the existing filenames for scripts that consume probe reports.
    crate::output_path::publish_batch(
        output_dir.as_ref(),
        &[
            ("imports.csv", &imports_csv),
            ("missing_imports.csv", &missing_csv),
            ("cpu_coverage.csv", &cpu_csv),
            ("candidate_static_report.json", &json),
            ("candidate_static_report.md", &md),
            ("runtime_gap_matrix.md", &matrix),
            (
                "x15_priority_backlog.md",
                "# Runtime priorities\n\nSee [runtime import coverage](runtime_gap_matrix.md) and the per-import priorities in [imports.csv](imports.csv).\n",
            ),
            (
                "test_commands.txt",
                "This static report only inspects PE bytes. It contains no execution or test-run evidence.\n",
            ),
        ],
    )?;

    Ok(report)
}
