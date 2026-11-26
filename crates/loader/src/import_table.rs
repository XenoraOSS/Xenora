// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded parser for the PE32+ import directory (Microsoft PE format,
//! "The .idata Section"). Binding and static inspection both use it, so they
//! agree on which imports an image has and how they are named.
//!
//! Static inspection also reads the delay-load directory ("Delay-Load Import
//! Tables (Image Only)"). Binding does not: the PE format leaves those imports
//! to a delay-load helper that runs inside the image on first use.

use std::fmt;

/// Most import descriptors accepted in one image.
pub const MAX_DESCRIPTORS: usize = 1024;
/// Most imported symbols accepted in one image.
pub const MAX_IMPORTS: usize = 65_536;
/// Longest module or symbol name accepted, in bytes.
pub const MAX_NAME_LEN: usize = 4096;

const DESCRIPTOR_LEN: u32 = 20;
const DELAY_DESCRIPTOR_LEN: u32 = 32;
/// Delay-load attribute bit 0: the descriptor's address fields are RVAs.
const DELAY_RVA_BASED: u32 = 1;
const ORDINAL_FLAG: u64 = 1 << 63;

/// How an import names its target. A symbol that happens to be spelled like an
/// ordinal is still a name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ImportName {
    Name(String),
    Ordinal(u16),
}

impl fmt::Display for ImportName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(name) => f.write_str(name),
            Self::Ordinal(ordinal) => write!(f, "Ordinal#{ordinal}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportEntry {
    /// Module name as spelled in the image.
    pub module: String,
    pub name: ImportName,
    /// RVA of the import address table slot.
    pub iat_rva: u32,
}

/// A delay-load descriptor field that must hold an RVA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DelayField {
    DllName,
    AddressTable,
    NameTable,
}

impl fmt::Display for DelayField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::DllName => "DLL name",
            Self::AddressTable => "import address table",
            Self::NameTable => "import name table",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportTableError {
    /// A structure at this RVA is not fully backed by section data.
    OutOfBounds {
        rva: u32,
    },
    /// A nonempty ordinary descriptor omits its DLL name or address table RVA.
    IncompleteImportDescriptor {
        rva: u32,
    },
    /// A name is empty, unterminated, too long or not printable ASCII.
    MalformedName {
        rva: u32,
    },
    /// A lookup entry at this RVA sets reserved bits.
    ReservedBits {
        rva: u32,
    },
    /// The delay-load descriptor at this RVA sets reserved attribute bits.
    DelayReservedBits {
        rva: u32,
    },
    /// The delay-load descriptor at this RVA has `RvaBased` clear, the earlier
    /// form whose address fields are virtual addresses. It is not read.
    DelayVirtualAddresses {
        rva: u32,
    },
    /// The delay-load descriptor at this RVA leaves a required field zero.
    DelayFieldMissing {
        rva: u32,
        field: DelayField,
    },
    /// The declared delay-load directory ends at this RVA before its empty
    /// descriptor.
    DelayDirectoryEnd {
        rva: u32,
    },
    ImportDirectoryEnd {
        rva: u32,
    },
    TooManyDescriptors,
    TooManyImports,
}

impl fmt::Display for ImportTableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfBounds { rva } => write!(
                f,
                "import data at RVA 0x{rva:08x} is truncated or outside the section data"
            ),
            Self::IncompleteImportDescriptor { rva } => write!(
                f,
                "import descriptor at RVA 0x{rva:08x} is missing a DLL name or import address table RVA"
            ),
            Self::MalformedName { rva } => write!(f, "malformed import name at RVA 0x{rva:08x}"),
            Self::ReservedBits { rva } => {
                write!(
                    f,
                    "reserved bits set in the import lookup at RVA 0x{rva:08x}"
                )
            }
            Self::DelayReservedBits { rva } => write!(
                f,
                "reserved attribute bits set in the delay-load descriptor at RVA 0x{rva:08x}"
            ),
            Self::DelayVirtualAddresses { rva } => write!(
                f,
                "delay-load descriptor at RVA 0x{rva:08x} holds virtual addresses, not RVAs, and is not read"
            ),
            Self::DelayFieldMissing { rva, field } => {
                write!(f, "delay-load descriptor at RVA 0x{rva:08x} has no {field}")
            }
            Self::DelayDirectoryEnd { rva } => write!(
                f,
                "delay-load directory ends at RVA 0x{rva:08x} before its empty descriptor"
            ),
            Self::ImportDirectoryEnd { rva } => write!(
                f,
                "import directory ends at RVA 0x{rva:08x} before its empty descriptor"
            ),
            Self::TooManyDescriptors => write!(f, "more than {MAX_DESCRIPTORS} import descriptors"),
            Self::TooManyImports => write!(f, "more than {MAX_IMPORTS} imported symbols"),
        }
    }
}

impl std::error::Error for ImportTableError {}

/// Reads image bytes by RVA, requiring every byte to be backed by the file.
struct Reader<'a, F> {
    bytes: &'a [u8],
    rva_to_offset: F,
}

impl<F: Fn(u32) -> Option<usize>> Reader<'_, F> {
    fn byte(&self, rva: u32) -> Option<u8> {
        self.bytes.get((self.rva_to_offset)(rva)?).copied()
    }

    fn array<const N: usize>(&self, rva: u32) -> Result<[u8; N], ImportTableError> {
        let mut out = [0; N];
        for (index, byte) in out.iter_mut().enumerate() {
            *byte = rva
                .checked_add(index as u32)
                .and_then(|rva| self.byte(rva))
                .ok_or(ImportTableError::OutOfBounds { rva })?;
        }
        Ok(out)
    }

    fn u32(&self, rva: u32) -> Result<u32, ImportTableError> {
        self.array(rva).map(u32::from_le_bytes)
    }

    fn name(&self, rva: u32) -> Result<String, ImportTableError> {
        let malformed = ImportTableError::MalformedName { rva };
        let mut name = String::new();
        for index in 0..=MAX_NAME_LEN as u32 {
            let byte = rva
                .checked_add(index)
                .and_then(|rva| self.byte(rva))
                .ok_or(malformed)?;
            match byte {
                0 if name.is_empty() => return Err(malformed),
                0 => return Ok(name),
                b' '..=b'~' => name.push(char::from(byte)),
                _ => return Err(malformed),
            }
        }
        Err(malformed)
    }
}

/// Parse the import descriptors within the declared directory extent.
///
/// Returns every import before the first problem, and the problem. Callers that
/// execute an image must refuse it if there is one; inspection may report both.
pub fn parse_imports(
    bytes: &[u8],
    directory_rva: u32,
    directory_size: u32,
    rva_to_offset: impl Fn(u32) -> Option<usize>,
) -> (Vec<ImportEntry>, Option<ImportTableError>) {
    let reader = Reader {
        bytes,
        rva_to_offset,
    };
    let mut entries = Vec::new();
    let error = collect(&reader, directory_rva, directory_size, &mut entries).err();
    (entries, error)
}

fn collect<F: Fn(u32) -> Option<usize>>(
    reader: &Reader<'_, F>,
    directory_rva: u32,
    directory_size: u32,
    entries: &mut Vec<ImportEntry>,
) -> Result<(), ImportTableError> {
    let end = directory_rva
        .checked_add(directory_size)
        .ok_or(ImportTableError::OutOfBounds { rva: directory_rva })?;
    let mut descriptor = directory_rva;
    for _ in 0..MAX_DESCRIPTORS {
        if end - descriptor < DESCRIPTOR_LEN {
            return Err(ImportTableError::ImportDirectoryEnd { rva: end });
        }
        let fields = reader.array::<{ DESCRIPTOR_LEN as usize }>(descriptor)?;
        // The timestamp and forwarder chain are part of the zero terminator.
        if fields == [0; DESCRIPTOR_LEN as usize] {
            return Ok(());
        }
        let lookup_rva = u32::from_le_bytes(fields[0..4].try_into().unwrap());
        let name_rva = u32::from_le_bytes(fields[12..16].try_into().unwrap());
        let address_rva = u32::from_le_bytes(fields[16..20].try_into().unwrap());
        if name_rva == 0 || address_rva == 0 {
            return Err(ImportTableError::IncompleteImportDescriptor { rva: descriptor });
        }
        let module = reader.name(name_rva)?;
        collect_symbols(reader, &module, lookup_rva, address_rva, false, entries)?;
        descriptor = descriptor
            .checked_add(DESCRIPTOR_LEN)
            .ok_or(ImportTableError::OutOfBounds { rva: descriptor })?;
    }
    Err(ImportTableError::TooManyDescriptors)
}

/// Parse the delay-load directory of `directory_size` bytes at `directory_rva`.
///
/// Only descriptors that lie wholly inside the declared directory are read,
/// and the directory must hold the empty descriptor that ends the table.
/// Each entry's `iat_rva` is its slot in the delay import address table.
/// Returns every import before the first problem, and the problem.
pub fn parse_delay_imports(
    bytes: &[u8],
    directory_rva: u32,
    directory_size: u32,
    rva_to_offset: impl Fn(u32) -> Option<usize>,
) -> (Vec<ImportEntry>, Option<ImportTableError>) {
    let reader = Reader {
        bytes,
        rva_to_offset,
    };
    let mut entries = Vec::new();
    let error = collect_delayed(&reader, directory_rva, directory_size, &mut entries).err();
    (entries, error)
}

fn collect_delayed<F: Fn(u32) -> Option<usize>>(
    reader: &Reader<'_, F>,
    directory_rva: u32,
    directory_size: u32,
    entries: &mut Vec<ImportEntry>,
) -> Result<(), ImportTableError> {
    let end = directory_rva
        .checked_add(directory_size)
        .ok_or(ImportTableError::OutOfBounds { rva: directory_rva })?;
    let mut descriptor = directory_rva;
    for _ in 0..MAX_DESCRIPTORS {
        if end - descriptor < DELAY_DESCRIPTOR_LEN {
            return Err(ImportTableError::DelayDirectoryEnd { rva: end });
        }
        let fields = reader.array::<{ DELAY_DESCRIPTOR_LEN as usize }>(descriptor)?;
        // The table ends with an empty descriptor.
        if fields == [0; DELAY_DESCRIPTOR_LEN as usize] {
            return Ok(());
        }
        let attributes = reader.u32(descriptor)?;
        if attributes & !DELAY_RVA_BASED != 0 {
            return Err(ImportTableError::DelayReservedBits { rva: descriptor });
        }
        if attributes & DELAY_RVA_BASED == 0 {
            return Err(ImportTableError::DelayVirtualAddresses { rva: descriptor });
        }
        let name_rva = reader.u32(descriptor + 4)?;
        let address_rva = reader.u32(descriptor + 12)?;
        let names_rva = reader.u32(descriptor + 16)?;
        // All three are required. The address table holds thunk addresses, so
        // it cannot stand in for a missing name table.
        for (field, rva) in [
            (DelayField::DllName, name_rva),
            (DelayField::AddressTable, address_rva),
            (DelayField::NameTable, names_rva),
        ] {
            if rva == 0 {
                return Err(ImportTableError::DelayFieldMissing {
                    rva: descriptor,
                    field,
                });
            }
        }
        // Each must also start in section data, whether or not an import uses it.
        reader.array::<1>(name_rva)?;
        reader.array::<8>(address_rva)?;
        reader.array::<8>(names_rva)?;
        let module = reader.name(name_rva)?;
        collect_symbols(reader, &module, names_rva, address_rva, true, entries)?;
        // The declared end bounds this sum.
        descriptor += DELAY_DESCRIPTOR_LEN;
    }
    Err(ImportTableError::TooManyDescriptors)
}

fn collect_symbols<F: Fn(u32) -> Option<usize>>(
    reader: &Reader<'_, F>,
    module: &str,
    lookup_rva: u32,
    address_rva: u32,
    slots_on_disk: bool,
    entries: &mut Vec<ImportEntry>,
) -> Result<(), ImportTableError> {
    // Without a lookup table the address table holds the same entries on disk.
    let start = if lookup_rva != 0 {
        lookup_rva
    } else {
        address_rva
    };
    for index in 0.. {
        let at = |base: u32| {
            base.checked_add(index * 8)
                .ok_or(ImportTableError::OutOfBounds { rva: base })
        };
        let lookup = at(start)?;
        let value = u64::from_le_bytes(reader.array(lookup)?);
        if value == 0 {
            return Ok(());
        }
        if entries.len() == MAX_IMPORTS {
            return Err(ImportTableError::TooManyImports);
        }
        let iat_rva = at(address_rva)?;
        if slots_on_disk {
            // A delay-load slot starts out holding its thunk's address.
            reader.array::<8>(iat_rva)?;
        }
        let name = if value & ORDINAL_FLAG != 0 {
            let ordinal = value & !ORDINAL_FLAG;
            ImportName::Ordinal(
                u16::try_from(ordinal)
                    .map_err(|_| ImportTableError::ReservedBits { rva: lookup })?,
            )
        } else {
            let hint = u32::try_from(value)
                .ok()
                .filter(|rva| rva >> 31 == 0)
                .ok_or(ImportTableError::ReservedBits { rva: lookup })?;
            // Keep name diagnostics for unreadable strings. A valid name
            // still needs both hint bytes before this record can be admitted.
            let name = reader.name(hint + 2)?;
            reader.array::<2>(hint)?;
            ImportName::Name(name)
        };
        entries.push(ImportEntry {
            module: module.to_owned(),
            name,
            iat_rva,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_import_requires_both_hint_bytes_in_ordinary_and_delay_tables() {
        const LOOKUP_RVA: u32 = 0x1000;
        const ADDRESS_RVA: u32 = 0x2000;
        const HINT_RVA: u32 = 0x3000;
        let mut bytes = [0; 64];
        bytes[..8].copy_from_slice(&(ORDINAL_FLAG | 42).to_le_bytes());
        bytes[8..16].copy_from_slice(&u64::from(HINT_RVA).to_le_bytes());
        bytes[48] = 0x34;
        bytes[32] = 0x12;
        bytes[33..40].copy_from_slice(b"Symbol\0");

        let ordinal = ImportEntry {
            module: "guest.dll".into(),
            name: ImportName::Ordinal(42),
            iat_rva: ADDRESS_RVA,
        };
        for slots_on_disk in [false, true] {
            for (first_backed, second_backed) in
                [(false, false), (false, true), (true, false), (true, true)]
            {
                let reader = Reader {
                    bytes: &bytes,
                    rva_to_offset: |rva| match rva {
                        0x1000..=0x1017 => Some((rva - LOOKUP_RVA) as usize),
                        0x2000..=0x200f => Some(16 + (rva - ADDRESS_RVA) as usize),
                        HINT_RVA if first_backed => Some(48),
                        0x3001 if second_backed => Some(32),
                        0x3002..=0x3008 => Some(32 + (rva - 0x3001) as usize),
                        _ => None,
                    },
                };
                let mut entries = Vec::new();
                let result = collect_symbols(
                    &reader,
                    "guest.dll",
                    LOOKUP_RVA,
                    ADDRESS_RVA,
                    slots_on_disk,
                    &mut entries,
                );

                if first_backed && second_backed {
                    assert_eq!(result, Ok(()));
                    assert_eq!(
                        entries,
                        [
                            ordinal.clone(),
                            ImportEntry {
                                module: "guest.dll".into(),
                                name: ImportName::Name("Symbol".into()),
                                iat_rva: ADDRESS_RVA + 8,
                            },
                        ],
                    );
                } else {
                    assert_eq!(result, Err(ImportTableError::OutOfBounds { rva: HINT_RVA }));
                    assert_eq!(entries.as_slice(), std::slice::from_ref(&ordinal));
                }
            }
        }
    }

    #[test]
    fn import_names_keep_printable_bytes_and_reject_malformed_strings() {
        let read = |bytes: &[u8]| {
            Reader {
                bytes,
                rva_to_offset: |rva| Some(rva as usize),
            }
            .name(0)
        };
        assert_eq!(read(b"~name with spaces~\0").unwrap(), "~name with spaces~");

        let error = ImportTableError::MalformedName { rva: 0 };
        for bytes in [
            &b"\0"[..],
            b"no terminator",
            b"tab\tname\0",
            b"newline\nname\0",
            b"del\x7fname\0",
            b"nonascii\x80name\0",
        ] {
            assert_eq!(read(bytes), Err(error), "{bytes:?}");
        }

        let mut largest = vec![b'x'; MAX_NAME_LEN];
        largest.push(0);
        assert_eq!(read(&largest).unwrap().len(), MAX_NAME_LEN);
        largest.insert(0, b' ');
        assert_eq!(read(&largest), Err(error));
    }
}
