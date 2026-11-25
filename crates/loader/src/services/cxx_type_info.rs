// SPDX-License-Identifier: GPL-3.0-or-later

//! C++ `type_info` and `std::exception` helpers exported by `vcruntime140.dll`.
//!
//! Compiled `typeid`, `type_info::operator==`, `type_info::before`,
//! `type_info::hash_code` and `type_info::name` call these helpers with a
//! pointer to the type descriptor's data: an 8-byte slot that caches the
//! readable name, followed by the NUL-terminated decorated name such as
//! `.?AVWidget@ui@@`. That layout is the publicly described MSVC RTTI type
//! descriptor (virtual table pointer, spare word, name).
//!
//! * Equality and ordering compare decorated names byte by byte.
//! * `__std_type_info_hash` is FNV-1a over the decorated name. The C++
//!   standard only requires equal types to hash equally within one run; the
//!   value is not claimed to match the one Microsoft's runtime computes.
//! * `__std_type_info_name` undecorates plain class, struct, union and enum
//!   names, including namespaces and nesting. Other decorations (templates,
//!   back-references, anonymous namespaces, pointer and function types) stop
//!   the guest rather than return a wrong name.
//!
//! `std::exception` keeps its message in an `__std_exception_data` record: the
//! message pointer, then a one-byte flag saying whether the exception owns
//! (and must free) that text. `__std_exception_copy` gives a copy its own
//! duplicate of an owned message so `what()` stays equal after the original is
//! destroyed, and shares a borrowed one. `__std_exception_destroy` frees an
//! owned message and clears the record. Owned text lives on the CRT heap, so
//! guest `malloc` and `free` interoperate with it.

use super::ServicesState;
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, MemoryError, PROCESS_HEAP};
use std::sync::{Arc, Mutex};

const MODULE: &str = "vcruntime140.dll";
/// Longest decorated name read from the guest.
const MAX_NAME: usize = 4096;
/// Longest exception message duplicated, in bytes.
const MAX_MESSAGE: usize = 1 << 20;

fn fault(error: MemoryError) -> ExitReason {
    ExitReason::MemoryFault(error)
}

fn unsupported<T>() -> Result<T, ExitReason> {
    Err(ExitReason::IllegalInstruction(0xFE))
}

/// The decorated name stored after the cached-name slot at `data`.
fn decorated(memory: &GuestMemory, data: u64) -> Result<Vec<u8>, ExitReason> {
    if data == 0 {
        return Err(fault(MemoryError::UnmappedAddress(0)));
    }
    let start = data.checked_add(8).ok_or(fault(MemoryError::Overflow))?;
    let mut name = Vec::new();
    for offset in 0..MAX_NAME as u64 {
        let byte = memory.read_u8(start + offset).map_err(fault)?;
        if byte == 0 {
            return Ok(name);
        }
        name.push(byte);
    }
    unsupported()
}

/// FNV-1a, 64-bit.
fn hash(name: &[u8]) -> u64 {
    name.iter().fold(0xcbf2_9ce4_8422_2325, |state, &byte| {
        (state ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The readable name of a plain class, struct, union or enum type, or `None`
/// when the decoration uses anything else.
pub fn undecorate(name: &[u8]) -> Option<String> {
    let name = std::str::from_utf8(name).ok()?;
    let rest = name.strip_prefix(".?A")?;
    let (keyword, rest) = match rest.as_bytes().first()? {
        b'V' => ("class", &rest[1..]),
        b'U' => ("struct", &rest[1..]),
        b'T' => ("union", &rest[1..]),
        // `W4` is an enum with an int underlying type.
        b'W' => ("enum", rest.strip_prefix("W4")?),
        _ => return None,
    };
    let parts = rest.strip_suffix("@@")?;
    let fragments: Vec<&str> = parts.split('@').collect();
    let plain = |part: &&str| {
        !part.is_empty()
            && !part.as_bytes()[0].is_ascii_digit()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    };
    if !fragments.iter().all(plain) {
        return None;
    }
    let qualified: Vec<&str> = fragments.into_iter().rev().collect();
    Some(format!("{keyword} {}", qualified.join("::")))
}

/// Names allocated by `__std_type_info_name`, released by
/// `__std_type_info_destroy_list`.
#[derive(Default)]
pub(super) struct TypeNames {
    allocations: Vec<u64>,
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    registry.register_with_status(
        MODULE,
        "__std_type_info_compare",
        ServiceStatus::Implemented,
        |args, _, memory| {
            if args.rcx == args.rdx {
                return Ok(0);
            }
            let left = decorated(memory, args.rcx)?;
            let right = decorated(memory, args.rdx)?;
            let result: i32 = match left.cmp(&right) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
            Ok(u64::from(result as u32))
        },
    );

    registry.register_with_status(
        MODULE,
        "__std_type_info_hash",
        ServiceStatus::Partial,
        |args, _, memory| Ok(hash(&decorated(memory, args.rcx)?)),
    );

    let names: Arc<Mutex<TypeNames>> = Arc::clone(&state.type_names);
    registry.register_with_status(
        MODULE,
        "__std_type_info_name",
        ServiceStatus::Partial,
        move |args, _, memory| {
            let cached = memory.read_u64(args.rcx).map_err(fault)?;
            if cached != 0 {
                return Ok(cached);
            }
            let Some(readable) = undecorate(&decorated(memory, args.rcx)?) else {
                return unsupported();
            };
            let mut bytes = readable.into_bytes();
            bytes.push(0);
            let address = memory
                .map_heap_allocation(PROCESS_HEAP, bytes.len())
                .map_err(fault)?;
            memory.write_bytes(address, &bytes).map_err(fault)?;
            memory.write_u64(args.rcx, address).map_err(fault)?;
            names.lock().expect("type names").allocations.push(address);
            Ok(address)
        },
    );

    let destroy = Arc::clone(&state.type_names);
    registry.register_with_status(
        MODULE,
        "__std_type_info_destroy_list",
        ServiceStatus::Partial,
        move |_, _, memory| {
            // Descriptors keep their cached pointers, as after the runtime
            // unloads; nothing reads them once the list is destroyed.
            for address in destroy.lock().expect("type names").allocations.drain(..) {
                memory.release_heap_allocation(PROCESS_HEAP, address);
            }
            Ok(0)
        },
    );

    registry.register_with_status(
        MODULE,
        "__std_exception_copy",
        ServiceStatus::Partial,
        |args, _, memory| {
            let (from, to) = (args.rcx, args.rdx);
            let text = memory.read_u64(from).map_err(fault)?;
            let owned = memory.read_u8(from + 8).map_err(fault)? != 0;
            if !owned || text == 0 {
                memory.write_u64(to, text).map_err(fault)?;
                memory.write_u8(to + 8, 0).map_err(fault)?;
                return Ok(0);
            }
            let mut bytes = Vec::new();
            loop {
                let byte = memory.read_u8(text + bytes.len() as u64).map_err(fault)?;
                bytes.push(byte);
                if byte == 0 {
                    break;
                }
                if bytes.len() > MAX_MESSAGE {
                    return unsupported();
                }
            }
            // An allocation failure leaves an empty record, so the copy
            // reports the runtime's generic message rather than share text it
            // does not own.
            let Ok(copy) = memory.map_heap_allocation(PROCESS_HEAP, bytes.len()) else {
                memory.write_u64(to, 0).map_err(fault)?;
                memory.write_u8(to + 8, 0).map_err(fault)?;
                return Ok(0);
            };
            memory.write_bytes(copy, &bytes).map_err(fault)?;
            memory.write_u64(to, copy).map_err(fault)?;
            memory.write_u8(to + 8, 1).map_err(fault)?;
            Ok(0)
        },
    );

    registry.register_with_status(
        MODULE,
        "__std_exception_destroy",
        ServiceStatus::Partial,
        |args, _, memory| {
            let text = memory.read_u64(args.rcx).map_err(fault)?;
            let owned = memory.read_u8(args.rcx + 8).map_err(fault)? != 0;
            if owned && text != 0 && !memory.release_heap_allocation(PROCESS_HEAP, text) {
                // Freeing memory the CRT heap never handed out is a guest bug.
                return Err(fault(MemoryError::UnmappedAddress(text)));
            }
            memory.write_u64(args.rcx, 0).map_err(fault)?;
            memory.write_u8(args.rcx + 8, 0).map_err(fault)?;
            Ok(0)
        },
    );
}

#[cfg(test)]
mod tests {
    use super::{hash, undecorate};

    #[test]
    fn undecorates_plain_and_nested_type_names() {
        let cases = [
            (".?AVWidget@@", "class Widget"),
            (".?AUPoint@geometry@@", "struct geometry::Point"),
            (".?ATValue@@", "union Value"),
            (".?AW4Color@ui@app@@", "enum app::ui::Color"),
            (".?AVInner@Outer@@", "class Outer::Inner"),
        ];
        for (decorated, readable) in cases {
            assert_eq!(undecorate(decorated.as_bytes()).as_deref(), Some(readable));
        }
    }

    #[test]
    fn refuses_decorations_it_does_not_model() {
        for decorated in [
            ".?AV?$Box@H@@",       // template
            ".?AVImpl@?A0x1234@@", // anonymous namespace
            ".?AVA@1@@",           // back-reference
            ".?AVWidget@",         // unterminated
            ".PEAX",               // pointer type
            ".?AW1Small@@",        // enum with a non-int underlying type
            ".?AV@@",
            "?AVWidget@@",
        ] {
            assert_eq!(undecorate(decorated.as_bytes()), None, "{decorated}");
        }
    }

    #[test]
    fn hash_is_fnv1a() {
        assert_eq!(hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(hash(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(hash(b".?AVA@@"), hash(b".?AVB@@"));
    }
}
