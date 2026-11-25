// SPDX-License-Identifier: GPL-3.0-or-later

//! Guest virtual memory model for Xenora's x86-64 execution bootstrap.
//!
//! Provides isolated, page-aligned guest virtual memory with permission checking
//! (Read, Write, Execute) and bounds enforcement without host OS mapping tricks.

use std::{fmt, ops::Range};

pub(crate) const ALLOCATION_GRANULARITY: u64 = 65_536;

/// Permission flags for a mapped memory region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryPermissions {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl MemoryPermissions {
    pub const NO_ACCESS: Self = Self {
        read: false,
        write: false,
        execute: false,
    };
    pub const READ_ONLY: Self = Self {
        read: true,
        write: false,
        execute: false,
    };
    pub const READ_WRITE: Self = Self {
        read: true,
        write: true,
        execute: false,
    };
    pub const READ_EXECUTE: Self = Self {
        read: true,
        write: false,
        execute: true,
    };
    pub const READ_WRITE_EXECUTE: Self = Self {
        read: true,
        write: true,
        execute: true,
    };
}

/// Errors occurring during guest memory operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryError {
    UnmappedAddress(u64),
    PermissionViolation {
        address: u64,
        required: &'static str,
    },
    OverlappingRegion {
        base: u64,
        size: u64,
    },
    Overflow,
    AllocationFailed,
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnmappedAddress(addr) => write!(f, "unmapped guest address 0x{addr:016x}"),
            Self::PermissionViolation { address, required } => {
                write!(
                    f,
                    "permission violation at guest address 0x{address:016x}: requires {required}"
                )
            }
            Self::OverlappingRegion { base, size } => {
                write!(
                    f,
                    "overlapping memory region: base 0x{base:016x}, size 0x{size:x}"
                )
            }
            Self::Overflow => write!(f, "guest memory address calculation overflow"),
            Self::AllocationFailed => write!(f, "guest memory allocation exceeds allowed limit"),
        }
    }
}

impl std::error::Error for MemoryError {}

pub(crate) const PROCESS_HEAP: u64 = u64::MAX - 2;
const MAX_DYNAMIC_BYTES: usize = 64 * 1024 * 1024;
/// Host storage bound for each dynamic allocation kind.
pub(crate) const DYNAMIC_BUDGET: u64 = MAX_DYNAMIC_BYTES as u64;

#[derive(Clone, Copy, PartialEq, Eq)]
enum AllocationKind {
    ImageOrStatic,
    Virtual,
    Heap(u64),
    /// Stack, guard or TEB of a guest thread; only the scheduler frees it.
    Thread,
}

#[derive(Clone)]
struct MappedRegion {
    base: u64,
    data: Vec<u8>,
    permissions: MemoryPermissions,
    allocation: AllocationKind,
}

impl MappedRegion {
    fn contains(&self, address: u64, size: usize) -> bool {
        let size_u64 = size as u64;
        let Some(end) = address.checked_add(size_u64) else {
            return false;
        };
        let region_end = self.base + self.data.len() as u64;
        address >= self.base && end <= region_end
    }
}

/// Guest virtual memory address space.
#[derive(Default, Clone)]
pub struct GuestMemory {
    regions: Vec<MappedRegion>,
    private_heaps: std::collections::BTreeSet<u64>,
    private_heap_serial: u64,
    // Stable errno pointers indexed by the guest thread's TEB address.
    crt_errno: std::collections::BTreeMap<u64, u64>,
    pub(crate) crt_token_cursors: std::collections::BTreeMap<u64, u64>,
    pub(crate) callback_frames:
        std::collections::BTreeMap<(u64, u64), crate::imports::CallbackFrame>,
}

impl GuestMemory {
    /// Create a new empty guest address space.
    pub fn new() -> Self {
        Self::default()
    }

    /// Map a new region into guest virtual memory.
    pub fn map_region(
        &mut self,
        base: u64,
        size: usize,
        permissions: MemoryPermissions,
    ) -> Result<(), MemoryError> {
        self.map_region_with_data(base, size, permissions, &[])
    }

    /// Map a new region and initialize it with initial data, bypassing runtime write permission checks.
    pub fn map_region_with_data(
        &mut self,
        base: u64,
        size: usize,
        permissions: MemoryPermissions,
        initial_data: &[u8],
    ) -> Result<(), MemoryError> {
        self.insert_region(
            base,
            size,
            permissions,
            initial_data,
            AllocationKind::ImageOrStatic,
        )
    }

    fn insert_region(
        &mut self,
        base: u64,
        size: usize,
        permissions: MemoryPermissions,
        initial_data: &[u8],
        allocation: AllocationKind,
    ) -> Result<(), MemoryError> {
        let size_u64 = size as u64;
        let end = base.checked_add(size_u64).ok_or(MemoryError::Overflow)?;
        for r in &self.regions {
            let r_end = r.base + r.data.len() as u64;
            if base < r_end && r.base < end {
                return Err(MemoryError::OverlappingRegion {
                    base,
                    size: size_u64,
                });
            }
        }

        let mut data = Vec::new();
        data.try_reserve_exact(size)
            .map_err(|_| MemoryError::AllocationFailed)?;
        data.resize(size, 0);
        let copy_len = size.min(initial_data.len());
        data[..copy_len].copy_from_slice(&initial_data[..copy_len]);

        self.regions.push(MappedRegion {
            base,
            data,
            permissions,
            allocation,
        });
        self.regions.sort_by_key(|r| r.base);
        Ok(())
    }

    pub(crate) fn map_virtual_allocation(
        &mut self,
        requested_base: u64,
        size: usize,
        permissions: MemoryPermissions,
    ) -> Result<u64, MemoryError> {
        self.map_dynamic_allocation(requested_base, size, permissions, AllocationKind::Virtual)
    }

    /// Bytes mapped in total, held by virtual allocations, and held by heap
    /// blocks. Each of the last two kinds is bounded by [`DYNAMIC_BUDGET`].
    pub(crate) fn usage(&self) -> (u64, u64, u64) {
        let mapped = self
            .regions
            .iter()
            .map(|region| region.data.len())
            .sum::<usize>();
        let virtual_bytes = self.dynamic_bytes(AllocationKind::Virtual);
        let heap_bytes = self.dynamic_bytes(AllocationKind::Heap(PROCESS_HEAP));
        (mapped as u64, virtual_bytes as u64, heap_bytes as u64)
    }

    pub(crate) fn crt_errno_address(&mut self, teb: u64) -> Result<u64, MemoryError> {
        if let Some(&address) = self.crt_errno.get(&teb) {
            return Ok(address);
        }
        if self.crt_errno.len() >= 1024 {
            return Err(MemoryError::AllocationFailed);
        }
        let address = self.find_dynamic_base(4)?;
        self.map_region(address, 4, MemoryPermissions::READ_WRITE)?;
        self.crt_errno.insert(teb, address);
        Ok(address)
    }

    pub(crate) fn create_private_heap(&mut self, initial: u64) -> Option<u64> {
        if initial > MAX_DYNAMIC_BYTES as u64
            || self.private_heaps.len() >= 256
            || self.private_heap_serial >= i64::MAX as u64 - 3
        {
            return None;
        }
        self.private_heap_serial += 1;
        let heap = PROCESS_HEAP - self.private_heap_serial;
        self.private_heaps.insert(heap);
        Some(heap)
    }

    pub(crate) fn has_heap(&self, heap: u64) -> bool {
        heap == PROCESS_HEAP || self.private_heaps.contains(&heap)
    }

    pub(crate) fn destroy_private_heap(&mut self, heap: u64) -> bool {
        if !self.private_heaps.remove(&heap) {
            return false;
        }
        self.regions
            .retain(|region| region.allocation != AllocationKind::Heap(heap));
        true
    }

    pub(crate) fn map_heap_allocation(
        &mut self,
        heap: u64,
        size: usize,
    ) -> Result<u64, MemoryError> {
        if !self.has_heap(heap) {
            return Err(MemoryError::AllocationFailed);
        }
        self.map_dynamic_allocation(
            0,
            size,
            MemoryPermissions::READ_WRITE,
            AllocationKind::Heap(heap),
        )
    }

    fn map_dynamic_allocation(
        &mut self,
        requested_base: u64,
        size: usize,
        permissions: MemoryPermissions,
        allocation: AllocationKind,
    ) -> Result<u64, MemoryError> {
        // Bound retained host storage independently for each allocation kind.
        let allocated = self.dynamic_bytes(allocation);
        if size > MAX_DYNAMIC_BYTES - allocated
            || (matches!(allocation, AllocationKind::Heap(_))
                && self
                    .regions
                    .iter()
                    .filter(|r| matches!(r.allocation, AllocationKind::Heap(_)))
                    .count()
                    >= 4096)
        {
            return Err(MemoryError::AllocationFailed);
        }

        let base = if requested_base == 0 {
            self.find_dynamic_base(size)?
        } else {
            requested_base
        };
        self.insert_region(base, size, permissions, &[], allocation)?;
        Ok(base)
    }

    fn dynamic_bytes(&self, allocation: AllocationKind) -> usize {
        self.regions
            .iter()
            .filter(|region| match allocation {
                AllocationKind::Heap(_) => matches!(region.allocation, AllocationKind::Heap(_)),
                _ => region.allocation == allocation,
            })
            .map(|region| region.data.len())
            .sum()
    }

    fn find_dynamic_base(&self, size: usize) -> Result<u64, MemoryError> {
        let mut base = 0x0000_0003_0000_0000u64;
        for region in &self.regions {
            let end = base.checked_add(size as u64).ok_or(MemoryError::Overflow)?;
            if end <= region.base {
                return Ok(base);
            }
            let region_end = region.base + region.data.len() as u64;
            if base < region_end {
                base = region_end
                    .checked_next_multiple_of(ALLOCATION_GRANULARITY)
                    .ok_or(MemoryError::Overflow)?;
            }
        }
        base.checked_add(size as u64).ok_or(MemoryError::Overflow)?;
        Ok(base)
    }

    pub(crate) fn resize_heap_allocation(
        &mut self,
        heap: u64,
        base: u64,
        size: usize,
        in_place_only: bool,
    ) -> Result<u64, MemoryError> {
        let index = self
            .regions
            .iter()
            .position(|region| {
                region.base == base && region.allocation == AllocationKind::Heap(heap)
            })
            .ok_or(MemoryError::UnmappedAddress(base))?;
        let previous = self.regions[index].data.len();
        let other_bytes = self.dynamic_bytes(AllocationKind::Heap(heap)) - previous;
        if size > MAX_DYNAMIC_BYTES - other_bytes {
            return Err(MemoryError::AllocationFailed);
        }

        let boundary = self
            .regions
            .get(index + 1)
            .map_or(u64::MAX, |next| next.base);
        let destination = if (size as u64) <= boundary - base {
            base
        } else if in_place_only {
            return Err(MemoryError::AllocationFailed);
        } else {
            self.find_dynamic_base(size)?
        };

        let region = &mut self.regions[index];
        region
            .data
            .try_reserve_exact(size.saturating_sub(previous))
            .map_err(|_| MemoryError::AllocationFailed)?;
        region.data.resize(size, 0);
        if size < previous {
            region.data.shrink_to_fit();
        }
        if destination != base {
            region.base = destination;
            self.regions.sort_by_key(|region| region.base);
        }
        Ok(destination)
    }

    pub(crate) fn map_thread_region(
        &mut self,
        base: u64,
        size: usize,
        permissions: MemoryPermissions,
    ) -> Result<u64, MemoryError> {
        self.map_dynamic_allocation(base, size, permissions, AllocationKind::Thread)
    }

    pub(crate) fn release_thread_region(&mut self, base: u64) -> bool {
        self.release_dynamic_allocation(base, AllocationKind::Thread)
    }

    pub(crate) fn release_virtual_allocation(&mut self, base: u64) -> bool {
        self.release_dynamic_allocation(base, AllocationKind::Virtual)
    }

    pub(crate) fn release_heap_allocation(&mut self, heap: u64, base: u64) -> bool {
        self.release_dynamic_allocation(base, AllocationKind::Heap(heap))
    }

    pub(crate) fn heap_allocation_size(&self, heap: u64, base: u64) -> Option<usize> {
        self.regions
            .iter()
            .find(|region| region.base == base && region.allocation == AllocationKind::Heap(heap))
            .map(|region| region.data.len())
    }

    fn release_dynamic_allocation(&mut self, base: u64, allocation: AllocationKind) -> bool {
        let Some(index) = self
            .regions
            .iter()
            .position(|region| region.base == base && region.allocation == allocation)
        else {
            return false;
        };
        self.regions.remove(index);
        true
    }

    pub(crate) fn check_write(&self, address: u64, size: usize) -> Result<(), MemoryError> {
        self.resolve(address, size, Some("write")).map(|_| ())
    }

    /// Resolve `[address, address + len)` to `(region index, offset, length)` pieces.
    ///
    /// An access may span adjacent regions, as it would span adjacent pages, but every
    /// byte must be mapped and every region must grant `required` (`None` bypasses
    /// permission checks for the loader). The whole range is checked before any byte
    /// is copied, so a failed access never partially reads or writes.
    ///
    /// A failed access reports the lowest address it may not touch, which is the
    /// start of the access only when its first byte fails. An access that begins in
    /// a mapped page and runs into an unmapped or forbidden one therefore reports
    /// the first byte of that page, as an x86 page fault reports the linear address
    /// that could not be translated or accessed.
    fn resolve(
        &self,
        address: u64,
        len: usize,
        required: Option<&'static str>,
    ) -> Result<Vec<(usize, usize, usize)>, MemoryError> {
        let end = address
            .checked_add(len as u64)
            .ok_or(MemoryError::Overflow)?;
        if len == 0 {
            return if self.regions.iter().any(|r| r.contains(address, 0)) {
                Ok(Vec::new())
            } else {
                Err(MemoryError::UnmappedAddress(address))
            };
        }
        let mut pieces = Vec::new();
        let mut cursor = address;
        for (index, region) in self.regions.iter().enumerate() {
            let region_end = region.base + region.data.len() as u64;
            if region_end <= cursor {
                continue;
            }
            if region.base > cursor {
                break;
            }
            let permitted = match required {
                None => true,
                Some("read") => region.permissions.read,
                Some("write") => region.permissions.write,
                Some(_) => region.permissions.execute,
            };
            if let (false, Some(required)) = (permitted, required) {
                return Err(MemoryError::PermissionViolation {
                    address: cursor,
                    required,
                });
            }
            let take = (end.min(region_end) - cursor) as usize;
            pieces.push((index, (cursor - region.base) as usize, take));
            cursor += take as u64;
            if cursor == end {
                return Ok(pieces);
            }
        }
        Err(MemoryError::UnmappedAddress(cursor))
    }

    fn read_checked(
        &self,
        address: u64,
        dst: &mut [u8],
        required: Option<&'static str>,
    ) -> Result<(), MemoryError> {
        let mut done = 0;
        for (index, offset, take) in self.resolve(address, dst.len(), required)? {
            dst[done..done + take]
                .copy_from_slice(&self.regions[index].data[offset..offset + take]);
            done += take;
        }
        Ok(())
    }

    fn write_checked(
        &mut self,
        address: u64,
        src: &[u8],
        required: Option<&'static str>,
    ) -> Result<(), MemoryError> {
        let mut done = 0;
        for (index, offset, take) in self.resolve(address, src.len(), required)? {
            self.regions[index].data[offset..offset + take]
                .copy_from_slice(&src[done..done + take]);
            done += take;
        }
        Ok(())
    }

    /// Write raw bytes into guest memory at the specified address, checking permissions.
    pub fn write_bytes(&mut self, address: u64, src: &[u8]) -> Result<(), MemoryError> {
        if let Some((index, range)) = self.contiguous_region(address, src.len())
            && self.regions[index].permissions.write
        {
            self.regions[index].data[range].copy_from_slice(src);
            return Ok(());
        }
        self.write_checked(address, src, Some("write"))
    }

    /// Read raw bytes from guest memory at the specified address, checking permissions.
    pub fn read_bytes(&self, address: u64, dst: &mut [u8]) -> Result<(), MemoryError> {
        if let Some((index, range)) = self.contiguous_region(address, dst.len())
            && self.regions[index].permissions.read
        {
            dst.copy_from_slice(&self.regions[index].data[range]);
            return Ok(());
        }
        self.read_checked(address, dst, Some("read"))
    }

    // Only the first eligible mapping may supply the whole span. Otherwise
    // the checked resolver must preserve split-access and overlap priority.
    fn contiguous_region(&self, address: u64, len: usize) -> Option<(usize, Range<usize>)> {
        let (index, region) = self
            .regions
            .iter()
            .enumerate()
            .find(|(_, region)| region.base + region.data.len() as u64 > address)?;
        let offset = usize::try_from(address.checked_sub(region.base)?).ok()?;
        let end = offset.checked_add(len)?;
        (end <= region.data.len()).then_some((index, offset..end))
    }

    /// Read an instruction byte from guest memory, verifying execute permission.
    pub fn fetch_instruction_byte(&self, address: u64) -> Result<u8, MemoryError> {
        address.checked_add(1).ok_or(MemoryError::Overflow)?;
        let region = self
            .regions
            .iter()
            .find(|region| region.base + region.data.len() as u64 > address)
            .filter(|region| region.base <= address)
            .ok_or(MemoryError::UnmappedAddress(address))?;
        if !region.permissions.execute {
            return Err(MemoryError::PermissionViolation {
                address,
                required: "execute",
            });
        }
        Ok(region.data[(address - region.base) as usize])
    }

    /// Read a 64-bit integer (little endian) from guest memory.
    pub fn read_u64(&self, address: u64) -> Result<u64, MemoryError> {
        let mut buf = [0u8; 8];
        self.read_bytes(address, &mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    /// Read a 64-bit integer ignoring region read permissions (e.g. loader/relocation phase).
    pub fn read_u64_bypass(&self, address: u64) -> Result<u64, MemoryError> {
        let mut buf = [0u8; 8];
        self.read_checked(address, &mut buf, None)?;
        Ok(u64::from_le_bytes(buf))
    }

    /// Write a 64-bit integer (little endian) to guest memory.
    pub fn write_u64(&mut self, address: u64, value: u64) -> Result<(), MemoryError> {
        self.write_bytes(address, &value.to_le_bytes())
    }

    /// Write a 64-bit integer ignoring region write permissions (e.g. loader/relocation phase).
    pub fn write_u64_bypass(&mut self, address: u64, value: u64) -> Result<(), MemoryError> {
        self.write_checked(address, &value.to_le_bytes(), None)
    }

    /// Read a 32-bit integer (little endian) from guest memory.
    pub fn read_u32(&self, address: u64) -> Result<u32, MemoryError> {
        let mut buf = [0u8; 4];
        self.read_bytes(address, &mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    /// Write a 32-bit integer (little endian) to guest memory.
    pub fn write_u32(&mut self, address: u64, value: u32) -> Result<(), MemoryError> {
        self.write_bytes(address, &value.to_le_bytes())
    }

    /// Write a 32-bit integer ignoring region write permissions (e.g. loader/relocation phase).
    pub fn write_u32_bypass(&mut self, address: u64, value: u32) -> Result<(), MemoryError> {
        self.write_checked(address, &value.to_le_bytes(), None)
    }

    /// Read a single byte from guest memory.
    pub fn read_u8(&self, address: u64) -> Result<u8, MemoryError> {
        let mut buf = [0u8; 1];
        self.read_bytes(address, &mut buf)?;
        Ok(buf[0])
    }

    /// Write a single byte to guest memory.
    pub fn write_u8(&mut self, address: u64, value: u8) -> Result<(), MemoryError> {
        self.write_bytes(address, &[value])
    }

    /// Read a 16-bit little-endian integer from guest memory.
    pub fn read_u16(&self, address: u64) -> Result<u16, MemoryError> {
        let mut buf = [0u8; 2];
        self.read_bytes(address, &mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    /// Write a 16-bit little-endian integer to guest memory.
    pub fn write_u16(&mut self, address: u64, value: u16) -> Result<(), MemoryError> {
        self.write_bytes(address, &value.to_le_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_heap_destruction_reclaims_only_owned_regions() {
        let mut memory = GuestMemory::new();
        let private = memory.create_private_heap(0).unwrap();
        let peer = memory.create_private_heap(0).unwrap();
        let address = memory.map_heap_allocation(private, 16).unwrap();
        let retained = memory.map_heap_allocation(peer, 16).unwrap();
        assert!(!memory.release_heap_allocation(peer, address));
        assert_eq!(memory.heap_allocation_size(PROCESS_HEAP, address), None);
        assert!(!memory.destroy_private_heap(PROCESS_HEAP));
        assert!(memory.destroy_private_heap(private));
        assert!(!memory.destroy_private_heap(private));
        assert!(memory.map_heap_allocation(private, 16).is_err());
        assert!(memory.read_u8(address).is_err());
        assert_eq!(memory.read_u8(retained), Ok(0));
        assert_ne!(memory.create_private_heap(0).unwrap(), private);
    }

    #[test]
    fn heap_resize_moves_only_when_allowed_and_preserves_bytes() {
        let mut memory = GuestMemory::new();
        let base = memory.map_heap_allocation(PROCESS_HEAP, 19).unwrap();
        let neighbor = memory.map_heap_allocation(PROCESS_HEAP, 1).unwrap();
        memory.write_u8(base, 0xab).unwrap();
        memory.write_u8(neighbor, 0xcd).unwrap();
        let growth = (neighbor - base) as usize + 1;
        assert_eq!(
            memory.resize_heap_allocation(PROCESS_HEAP, base, growth, true),
            Err(MemoryError::AllocationFailed)
        );
        assert_eq!(memory.heap_allocation_size(PROCESS_HEAP, base), Some(19));
        let moved = memory
            .resize_heap_allocation(PROCESS_HEAP, base, growth, false)
            .unwrap();
        assert_ne!(moved, base);
        assert_eq!(memory.read_u8(moved), Ok(0xab));
        assert_eq!(memory.read_u8(neighbor), Ok(0xcd));
        assert_eq!(memory.read_u8(moved + growth as u64 - 1), Ok(0));
        assert_eq!(memory.heap_allocation_size(PROCESS_HEAP, base), None);
        assert_eq!(
            memory.resize_heap_allocation(PROCESS_HEAP, moved, 1, true),
            Ok(moved)
        );
        assert_eq!(memory.heap_allocation_size(PROCESS_HEAP, moved), Some(1));
    }
}

#[cfg(test)]
mod instruction_fetch_tests {
    use super::{MemoryPermissions as P, *};

    fn denied(address: u64) -> Result<u8, MemoryError> {
        Err(MemoryError::PermissionViolation {
            address,
            required: "execute",
        })
    }
    fn missing(address: u64) -> Result<u8, MemoryError> {
        Err(MemoryError::UnmappedAddress(address))
    }

    #[test]
    fn fetch_reports_overflow_before_lookup() {
        let m = GuestMemory::new();
        for at in [0, 1, 0x1000, u64::MAX - 1] {
            assert_eq!(m.fetch_instruction_byte(at), missing(at));
        }
        assert_eq!(
            m.fetch_instruction_byte(u64::MAX),
            Err(MemoryError::Overflow)
        );
        let mut m = GuestMemory::new();
        m.map_region(u64::MAX, 0, P::NO_ACCESS).unwrap();
        assert_eq!(
            m.fetch_instruction_byte(u64::MAX),
            Err(MemoryError::Overflow)
        );
    }

    #[test]
    fn fetch_checks_execute_permission_for_every_byte_value() {
        let bytes: Vec<u8> = (0..=255).collect();
        for flags in 0..8 {
            let permission = P {
                read: flags & 1 != 0,
                write: flags & 2 != 0,
                execute: flags & 4 != 0,
            };
            let mut m = GuestMemory::new();
            m.map_region_with_data(0x1000, bytes.len(), permission, &bytes)
                .unwrap();
            for (offset, byte) in bytes.iter().enumerate() {
                let at = 0x1000 + offset as u64;
                assert_eq!(
                    m.fetch_instruction_byte(at),
                    if flags & 4 != 0 {
                        Ok(*byte)
                    } else {
                        denied(at)
                    },
                    "flags={flags}, at={at:x}"
                );
            }
        }
    }

    #[test]
    fn fetch_handles_sparse_and_out_of_order_mappings() {
        let mut m = GuestMemory::new();
        m.map_region_with_data(0x3000, 2, P::READ_EXECUTE, &[0xc1, 0xc2])
            .unwrap();
        m.map_region_with_data(0, 2, P::READ_EXECUTE, &[0x01, 0x02])
            .unwrap();
        m.map_region_with_data(0x2000, 1, P::READ_ONLY, &[0xb1])
            .unwrap();
        m.map_region_with_data(0x1000, 2, P::READ_EXECUTE, &[0xa1, 0xa2])
            .unwrap();
        for (at, byte) in [
            (0, 1),
            (1, 2),
            (0x1000, 0xa1),
            (0x1001, 0xa2),
            (0x3000, 0xc1),
            (0x3001, 0xc2),
        ] {
            assert_eq!(m.fetch_instruction_byte(at), Ok(byte));
        }
        assert_eq!(m.fetch_instruction_byte(0x2000), denied(0x2000));
        for at in [2, 0xfff, 0x1002, 0x2001, 0x2fff, 0x3002] {
            assert_eq!(m.fetch_instruction_byte(at), missing(at));
        }
    }

    #[test]
    fn fetch_ignores_empty_mappings_at_shared_boundaries() {
        for zero_first in [false, true] {
            for execute in [false, true] {
                let mut m = GuestMemory::new();
                let permission = P {
                    read: false,
                    write: false,
                    execute,
                };
                if zero_first {
                    for _ in 0..7 {
                        m.map_region(0x1000, 0, P::READ_WRITE_EXECUTE).unwrap();
                    }
                }
                m.map_region_with_data(0x1000, 2, permission, &[0x34, 0x56])
                    .unwrap();
                for _ in 0..7 {
                    m.map_region(0x1000, 0, P::NO_ACCESS).unwrap();
                }
                for _ in 0..7 {
                    m.map_region(0x1002, 0, P::READ_WRITE_EXECUTE).unwrap();
                }
                for (at, byte) in [(0x1000, 0x34), (0x1001, 0x56)] {
                    assert_eq!(
                        m.fetch_instruction_byte(at),
                        if execute { Ok(byte) } else { denied(at) }
                    );
                }
                assert_eq!(m.fetch_instruction_byte(0x1002), missing(0x1002));
            }
        }
        let mut m = GuestMemory::new();
        for at in [1, 1, 2, 0x1000] {
            m.map_region(at, 0, P::READ_WRITE_EXECUTE).unwrap();
        }
        for at in [0, 1, 2, 3, 0x1000] {
            assert_eq!(m.fetch_instruction_byte(at), missing(at));
        }
    }

    #[test]
    fn fetch_checks_adjacent_mapping_permissions() {
        let mut m = GuestMemory::new();
        m.map_region_with_data(0x1000, 1, P::READ_EXECUTE, &[0xa5])
            .unwrap();
        m.map_region_with_data(0x1001, 1, P::READ_WRITE, &[0x5a])
            .unwrap();
        m.map_region(0x1001, 0, P::READ_EXECUTE).unwrap();
        m.map_region_with_data(0x1002, 1, P::READ_EXECUTE, &[0xc3])
            .unwrap();
        assert_eq!(m.fetch_instruction_byte(0x1000), Ok(0xa5));
        assert_eq!(m.fetch_instruction_byte(0x1001), denied(0x1001));
        assert_eq!(m.fetch_instruction_byte(0x1002), Ok(0xc3));
        assert_eq!(m.fetch_instruction_byte(0x1003), missing(0x1003));
    }

    #[test]
    fn fetch_handles_the_highest_region_end() {
        let mut m = GuestMemory::new();
        m.map_region_with_data(u64::MAX - 2, 2, P::READ_EXECUTE, &[0x12, 0x34])
            .unwrap();
        m.map_region(u64::MAX, 0, P::READ_EXECUTE).unwrap();
        assert_eq!(
            m.fetch_instruction_byte(u64::MAX - 3),
            missing(u64::MAX - 3)
        );
        assert_eq!(m.fetch_instruction_byte(u64::MAX - 2), Ok(0x12));
        assert_eq!(m.fetch_instruction_byte(u64::MAX - 1), Ok(0x34));
        assert_eq!(
            m.fetch_instruction_byte(u64::MAX),
            Err(MemoryError::Overflow)
        );
    }

    #[test]
    fn fetch_observes_writes_clones_and_mapping_changes() {
        let mut m = GuestMemory::new();
        m.map_region_with_data(0x1000, 1, P::READ_WRITE_EXECUTE, &[0x11])
            .unwrap();
        let mut clone = m.clone();
        m.write_u8(0x1000, 0x22).unwrap();
        clone.write_u8(0x1000, 0x33).unwrap();
        assert_eq!(m.fetch_instruction_byte(0x1000), Ok(0x22));
        assert_eq!(clone.fetch_instruction_byte(0x1000), Ok(0x33));
        let base = m
            .map_virtual_allocation(0x4000, 1, P::READ_WRITE_EXECUTE)
            .unwrap();
        m.write_u8(base, 0x44).unwrap();
        assert_eq!(m.fetch_instruction_byte(base), Ok(0x44));
        assert!(m.release_virtual_allocation(base));
        assert_eq!(m.fetch_instruction_byte(base), missing(base));
        m.map_region_with_data(base, 1, P::READ_ONLY, &[0x55])
            .unwrap();
        assert_eq!(m.fetch_instruction_byte(base), denied(base));
        assert_eq!(clone.fetch_instruction_byte(base), missing(base));
    }

    #[test]
    fn fetch_keeps_first_mapping_priority_after_empty_heap_growth() {
        // Existing private allocation methods can grow a zero allocation onto an
        // earlier mapping at its base. The fetch-only patch must preserve the
        // original mapping's priority even for this already-representable state.
        let mut m = GuestMemory::new();
        m.map_region_with_data(0x1000, 1, P::READ_EXECUTE, &[0x90])
            .unwrap();
        const BASE: u64 = 0x0000_0003_0000_0000;
        m.map_region_with_data(BASE, 4, P::READ_EXECUTE, &[0x11, 0x22, 0x33, 0x44])
            .unwrap();
        let empty = m.map_heap_allocation(PROCESS_HEAP, 0).unwrap();
        assert_eq!(empty, BASE);
        assert_eq!(
            m.resize_heap_allocation(PROCESS_HEAP, empty, 2, true),
            Ok(BASE)
        );
        assert_eq!(m.fetch_instruction_byte(BASE), Ok(0x11));
        assert_eq!(m.fetch_instruction_byte(BASE + 1), Ok(0x22));
        assert_eq!(m.fetch_instruction_byte(BASE + 3), Ok(0x44));
    }
}

#[cfg(test)]
mod data_access_contract_tests {
    use super::{MemoryPermissions as P, *};
    const A: u64 = 0x10_0000;

    #[test]
    fn data_permissions_are_independent_and_failed_reads_leave_output_untouched() {
        for bits in 0..8 {
            let p = P {
                read: bits & 1 != 0,
                write: bits & 2 != 0,
                execute: bits & 4 != 0,
            };
            let mut m = GuestMemory::new();
            m.map_region_with_data(A, 4, p, &[1, 2, 3, 4]).unwrap();
            let mut out = [0xee; 2];
            let read = m.read_bytes(A + 1, &mut out);
            assert_eq!(
                read,
                if p.read {
                    Ok(())
                } else {
                    Err(MemoryError::PermissionViolation {
                        address: A + 1,
                        required: "read",
                    })
                }
            );
            assert_eq!(out, if p.read { [2, 3] } else { [0xee; 2] });
            let write = m.write_bytes(A + 1, &[0x55, 0x66]);
            assert_eq!(
                write,
                if p.write {
                    Ok(())
                } else {
                    Err(MemoryError::PermissionViolation {
                        address: A + 1,
                        required: "write",
                    })
                }
            );
            let mut stored = [0; 4];
            m.read_checked(A, &mut stored, None).unwrap();
            assert_eq!(
                stored,
                if p.write {
                    [1, 0x55, 0x66, 4]
                } else {
                    [1, 2, 3, 4]
                }
            );
        }
    }

    #[test]
    fn empty_data_spans_keep_inclusive_ends_and_ignore_permissions() {
        let mut m = GuestMemory::new();
        m.map_region(A, 0, P::NO_ACCESS).unwrap();
        m.map_region(A + 16, 3, P::NO_ACCESS).unwrap();
        m.map_region(u64::MAX, 0, P::NO_ACCESS).unwrap();
        for at in [A, A + 16, A + 17, A + 19, u64::MAX] {
            assert_eq!(m.read_bytes(at, &mut []), Ok(()));
            assert_eq!(m.write_bytes(at, &[]), Ok(()));
        }
        for at in [A - 1, A + 1, A + 15, A + 20] {
            assert_eq!(
                m.read_bytes(at, &mut []),
                Err(MemoryError::UnmappedAddress(at))
            );
            assert_eq!(
                m.write_bytes(at, &[]),
                Err(MemoryError::UnmappedAddress(at))
            );
        }
    }

    #[test]
    fn data_address_overflow_precedes_permissions_and_preserves_output() {
        let mut m = GuestMemory::new();
        m.map_region(u64::MAX - 3, 3, P::NO_ACCESS).unwrap();
        let mut out = [0xee; 2];
        assert_eq!(
            m.read_bytes(u64::MAX - 1, &mut out),
            Err(MemoryError::Overflow)
        );
        assert_eq!(out, [0xee; 2]);
        assert_eq!(
            m.write_bytes(u64::MAX - 1, &[1, 2]),
            Err(MemoryError::Overflow)
        );
        assert_eq!(
            m.read_bytes(u64::MAX, &mut out[..1]),
            Err(MemoryError::Overflow)
        );
        assert_eq!(m.write_bytes(u64::MAX, &[1]), Err(MemoryError::Overflow));
        assert_eq!(m.read_u16(u64::MAX - 1), Err(MemoryError::Overflow));
    }

    #[test]
    fn split_data_accesses_keep_exact_fallback_errors_and_atomicity() {
        for (gap, right) in [
            (0, P::READ_WRITE),
            (0, P::READ_ONLY),
            (0, P::NO_ACCESS),
            (
                0,
                P {
                    read: false,
                    write: true,
                    execute: false,
                },
            ),
            (1, P::READ_WRITE),
        ] {
            let mut m = GuestMemory::new();
            m.map_region_with_data(A, 8, P::READ_WRITE, &[0, 1, 2, 3, 4, 5, 6, 7])
                .unwrap();
            let b = A + 8 + gap;
            m.map_region_with_data(b, 8, right, &[8, 9, 10, 11, 12, 13, 14, 15])
                .unwrap();
            let mut expected = m.clone();
            let mut actual_out = [0xee; 4];
            let mut expected_out = [0xee; 4];
            assert_eq!(
                m.read_bytes(A + 6, &mut actual_out),
                expected.read_checked(A + 6, &mut expected_out, Some("read"))
            );
            assert_eq!(actual_out, expected_out);
            assert_eq!(
                actual_out,
                if gap == 0 && right.read {
                    [6, 7, 8, 9]
                } else {
                    [0xee; 4]
                }
            );
            let bytes = [0xa0, 0xa1, 0xa2, 0xa3];
            assert_eq!(
                m.write_bytes(A + 6, &bytes),
                expected.write_checked(A + 6, &bytes, Some("write"))
            );
            let written = gap == 0 && right.write;
            assert_eq!(
                m.read_u64_bypass(A).unwrap().to_le_bytes(),
                if written {
                    [0, 1, 2, 3, 4, 5, 0xa0, 0xa1]
                } else {
                    [0, 1, 2, 3, 4, 5, 6, 7]
                }
            );
            assert_eq!(
                m.read_u64_bypass(b).unwrap().to_le_bytes(),
                if written {
                    [0xa2, 0xa3, 10, 11, 12, 13, 14, 15]
                } else {
                    [8, 9, 10, 11, 12, 13, 14, 15]
                }
            );
        }
    }

    #[test]
    fn data_accesses_keep_first_mapping_priority_for_partial_overlaps() {
        const BASE: u64 = 0x0000_0003_0000_0000;
        for first in [P::READ_WRITE, P::READ_ONLY] {
            let mut m = GuestMemory::new();
            m.map_region(0x1000, 1, P::NO_ACCESS).unwrap();
            m.map_region_with_data(BASE, 4, first, &[1, 2, 3, 4])
                .unwrap();
            let empty = m.map_heap_allocation(PROCESS_HEAP, 0).unwrap();
            assert_eq!(empty, BASE);
            assert_eq!(
                m.resize_heap_allocation(PROCESS_HEAP, empty, 8, true),
                Ok(BASE)
            );
            let mut out = [0xee; 8];
            assert_eq!(m.read_bytes(BASE, &mut out), Ok(()));
            assert_eq!(out, [1, 2, 3, 4, 0, 0, 0, 0]);
            assert_eq!(m.read_u16(BASE), Ok(0x0201));
            let bytes = [0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7];
            let mut expected = m.clone();
            assert_eq!(
                m.write_bytes(BASE, &bytes),
                expected.write_checked(BASE, &bytes, Some("write"))
            );
            m.read_bytes(BASE, &mut out).unwrap();
            assert_eq!(
                out,
                if first.write {
                    bytes
                } else {
                    [1, 2, 3, 4, 0, 0, 0, 0]
                }
            );
        }
    }

    #[test]
    fn data_accesses_observe_writes_clones_release_and_remap() {
        let mut m = GuestMemory::new();
        m.map_region_with_data(A, 8, P::READ_WRITE, &[1; 8])
            .unwrap();
        let mut clone = m.clone();
        m.write_u64(A, 0x1122_3344_5566_7788).unwrap();
        clone.write_u32(A, 0xaabb_ccdd).unwrap();
        assert_eq!(m.read_u64(A), Ok(0x1122_3344_5566_7788));
        assert_eq!(clone.read_u64(A), Ok(0x0101_0101_aabb_ccdd));
        let b = m.map_virtual_allocation(0x4000, 2, P::READ_WRITE).unwrap();
        m.write_u16(b, 0x1234).unwrap();
        assert_eq!(m.read_u16(b), Ok(0x1234));
        assert!(m.release_virtual_allocation(b));
        assert_eq!(m.read_u16(b), Err(MemoryError::UnmappedAddress(b)));
        m.map_region_with_data(b, 2, P::READ_ONLY, &[0x56, 0x78])
            .unwrap();
        assert_eq!(m.read_u16(b), Ok(0x7856));
        assert_eq!(
            m.write_u8(b, 0),
            Err(MemoryError::PermissionViolation {
                address: b,
                required: "write"
            })
        );
        assert_eq!(clone.read_u16(b), Err(MemoryError::UnmappedAddress(b)));
    }
}

#[cfg(test)]
mod data_memory_bench {
    use super::{MemoryPermissions as P, *};
    use crate::{CpuState, DbtEngine, ExitReason, Register};
    use std::{hint::black_box, time::Instant};

    const CODE: u64 = 0x1000;
    const VALUE: u64 = 0xffff_ff00;
    #[derive(Clone, Copy)]
    enum Kind {
        Registers,
        Counter,
        Sequential,
        Split,
    }

    fn setting(name: &str, default: usize, maximum: usize) -> usize {
        let value = std::env::var(name).map_or(default, |s| s.parse().expect(name));
        assert!((1..=maximum).contains(&value), "invalid {name}");
        value
    }
    fn quantile(sorted: &[f64], fraction: f64) -> f64 {
        let rank = (sorted.len() - 1) as f64 * fraction;
        let lo = rank.floor() as usize;
        sorted[lo] + (sorted[rank.ceil() as usize] - sorted[lo]) * rank.fract()
    }
    fn guest(kind: Kind, iterations: usize, data: u64) -> (Vec<u8>, usize) {
        // MOV RCX, iterations; MOV RDX, VALUE (control) or RSI, data; XOR EAX,EAX.
        let registers = matches!(kind, Kind::Registers);
        let sequential = matches!(kind, Kind::Sequential);
        let mut code = vec![0x48, 0xb9];
        code.extend_from_slice(&(iterations as u64).to_le_bytes());
        code.extend_from_slice(&[0x48, if registers { 0xba } else { 0xbe }]);
        let target = if registers { VALUE } else { data };
        code.extend_from_slice(&target.to_le_bytes());
        code.extend_from_slice(&[0x31, 0xc0]);
        let per_iteration = if registers {
            // ADD RAX,RDX; ADD RDX,1; DEC RCX; JNZ -12.
            code.extend_from_slice(&[
                0x48, 0x01, 0xd0, 0x48, 0x83, 0xc2, 0x01, 0x48, 0xff, 0xc9, 0x75, 0xf4,
            ]);
            4
        } else {
            // MOV RDX,[RSI]; ADD RAX,RDX; ADD RDX,1; MOV [RSI],RDX.
            code.extend_from_slice(&[
                0x48, 0x8b, 0x16, 0x48, 0x01, 0xd0, 0x48, 0x83, 0xc2, 0x01, 0x48, 0x89, 0x16,
            ]);
            if sequential {
                code.extend_from_slice(&[0x48, 0x83, 0xc6, 0x08]); // ADD RSI,8.
            }
            // DEC RCX; JNZ -22 (sequential) or -18 (counter).
            let back = if sequential { 0xea } else { 0xee };
            code.extend_from_slice(&[0x48, 0xff, 0xc9, 0x75, back]);
            6 + usize::from(sequential)
        };
        code.push(0xf4); // HLT; three setup instructions and one halt.
        (code, iterations * per_iteration + 4)
    }

    // Run identical source on the baseline and candidate with the same release profile.
    // Allocation counting is deliberately separate: instrumentation distorts these timings.
    #[test]
    #[ignore = "manual, correctness-gated warm DBT data-memory timing"]
    fn warm_data_memory_benchmark() {
        let samples = setting("XENORA_DATA_BENCH_SAMPLES", 31, 1001);
        let counter = setting("XENORA_DATA_BENCH_ITERS", 32768, 65536);
        let sequential = setting("XENORA_DATA_BENCH_STREAM_ITERS", 16384, 65536);
        for (label, kind, regions, last) in [
            ("register-control", Kind::Registers, 4, false),
            ("counter-front", Kind::Counter, 4, false),
            ("counter-front-many", Kind::Counter, 64, false),
            ("counter-first", Kind::Counter, 4, false),
            ("counter-first-many", Kind::Counter, 64, false),
            ("counter-third-many", Kind::Counter, 64, false),
            ("counter-middle", Kind::Counter, 64, false),
            ("counter-last", Kind::Counter, 64, true),
            ("sequential-front", Kind::Sequential, 4, false),
            ("sequential-front-many", Kind::Sequential, 64, false),
            ("sequential-first", Kind::Sequential, 4, false),
            ("sequential-first-many", Kind::Sequential, 64, false),
            ("sequential-last", Kind::Sequential, 64, true),
            ("split-counter-control", Kind::Split, 4, false),
        ] {
            let iterations = if matches!(kind, Kind::Sequential) {
                sequential
            } else {
                counter
            };
            let code_address = if label.contains("-first") {
                0x8000_0000
            } else {
                CODE
            };
            let data = if last {
                0x4000_0000
            } else if label.contains("-third") {
                0x20_1000
            } else if label.contains("-middle") {
                0x23_f000
            } else {
                0x1_0000
            };
            let (code, expected_instructions) = guest(kind, iterations, data);
            let words = if matches!(kind, Kind::Sequential) {
                iterations
            } else {
                1
            };
            let initial: Vec<u8> = (0..words)
                .flat_map(|i| (VALUE + i as u64).to_le_bytes())
                .collect();
            let increment = match kind {
                Kind::Registers => 0,
                Kind::Sequential => 1,
                _ => iterations as u64,
            };
            let expected: Vec<u8> = (0..words)
                .flat_map(|i| (VALUE + i as u64 + increment).to_le_bytes())
                .collect();
            let mut memory = GuestMemory::new();
            memory
                .map_region_with_data(code_address, 4096, P::READ_EXECUTE, &code)
                .unwrap();
            let pieces = if matches!(kind, Kind::Split) {
                memory
                    .map_region_with_data(data, 4, P::READ_WRITE, &initial[..4])
                    .unwrap();
                memory
                    .map_region_with_data(data + 4, 4, P::READ_WRITE, &initial[4..])
                    .unwrap();
                2
            } else {
                memory
                    .map_region_with_data(data, initial.len(), P::READ_WRITE, &initial)
                    .unwrap();
                1
            };
            for i in 0..regions - pieces - 1 {
                memory
                    .map_region(0x20_0000 + i as u64 * 8192, 4096, P::READ_ONLY)
                    .unwrap();
            }
            let initial_cpu = CpuState::new(code_address, 0x8000_0000); // No stack instructions.
            let mut dbt = DbtEngine::new(1024 * 1024).unwrap();
            let mut readback = vec![0; initial.len()];
            let n = iterations as u64;
            let sum = n * VALUE + n * (n - 1) / 2;
            let mut timings = Vec::with_capacity(samples);
            let warmups = 5;
            for sample in 0..samples + warmups {
                memory.write_bytes(data, &initial).unwrap();
                let mut cpu = initial_cpu.clone();
                let before = dbt.metrics;
                let started = Instant::now();
                let result = black_box(dbt.run(&mut cpu, &mut memory, expected_instructions + 16));
                let elapsed = started.elapsed();
                assert_eq!(result.exit_reason, ExitReason::Halt, "{label}");
                assert_eq!(
                    result.instructions_executed, expected_instructions,
                    "{label}"
                );
                let state = (
                    cpu.get(Register::Rax),
                    cpu.get(Register::Rdx),
                    cpu.get(Register::Rcx),
                );
                assert_eq!(state, (sum, VALUE + n, 0), "{label}");
                memory.read_bytes(data, &mut readback).unwrap();
                assert_eq!(readback, expected, "{label}");
                assert_eq!(dbt.metrics.interpreter_fallbacks, 0, "{label}");
                if sample > 0 {
                    let compiled = |m: crate::DbtMetrics| {
                        (m.blocks_compiled, m.native_bytes_emitted, m.cache_misses)
                    };
                    assert_eq!(compiled(dbt.metrics), compiled(before), "{label}");
                    if sample >= warmups {
                        timings.push(elapsed.as_nanos() as f64 / n as f64);
                    }
                } else {
                    assert!(dbt.metrics.blocks_compiled > 0, "{label}");
                }
            }
            timings.sort_by(f64::total_cmp);
            println!(
                "warm-data label={label} regions={regions} iterations={iterations} samples={samples} warmups={warmups} instructions={expected_instructions} median_ns_per_iteration={:.3} p10={:.3} p90={:.3}",
                quantile(&timings, 0.5),
                quantile(&timings, 0.1),
                quantile(&timings, 0.9)
            );
        }
    }
}

#[cfg(test)]
mod mapping_group_contract_tests {
    use super::*;

    #[test]
    fn aliases_preserve_bytes_across_resize_relocation_and_release() {
        const BASE: u64 = 0x0000_0003_0000_0000;
        let mut memory = GuestMemory::new();
        memory
            .map_region(BASE - 16, 8, MemoryPermissions::READ_ONLY)
            .unwrap();
        memory
            .map_region_with_data(BASE, 4, MemoryPermissions::READ_ONLY, &[1, 2, 3, 4])
            .unwrap();
        memory
            .map_region(BASE + 16, 8, MemoryPermissions::READ_ONLY)
            .unwrap();
        let heap = memory.create_private_heap(0).unwrap();
        assert_eq!(memory.map_heap_allocation(PROCESS_HEAP, 0), Ok(BASE));
        assert_eq!(memory.map_heap_allocation(heap, 0), Ok(BASE));
        assert_eq!(
            memory.resize_heap_allocation(PROCESS_HEAP, BASE, 8, true),
            Err(MemoryError::AllocationFailed)
        );
        assert_eq!(memory.resize_heap_allocation(heap, BASE, 8, true), Ok(BASE));
        memory.write_bytes(BASE + 4, &[5, 6, 7, 8]).unwrap();
        assert_eq!(memory.read_u64(BASE), Ok(0x0807_0605_0403_0201));
        assert_eq!(
            memory.resize_heap_allocation(heap, BASE, 32, true),
            Err(MemoryError::AllocationFailed)
        );
        let moved = memory
            .resize_heap_allocation(heap, BASE, 32, false)
            .unwrap();
        assert_ne!(moved, BASE);
        let mut bytes = [0xff; 32];
        memory.read_bytes(moved, &mut bytes).unwrap();
        let mut expected = [0; 32];
        expected[4..8].copy_from_slice(&[5, 6, 7, 8]);
        assert_eq!(bytes, expected);
        let mut failed = [0xff; 8];
        assert_eq!(
            memory.read_bytes(BASE, &mut failed),
            Err(MemoryError::UnmappedAddress(BASE + 4))
        );
        assert_eq!(failed, [0xff; 8]);
        assert_eq!(
            memory.resize_heap_allocation(PROCESS_HEAP, BASE, 8, true),
            Ok(BASE)
        );
        assert_eq!(memory.read_u64(BASE), Ok(0x0403_0201));
        assert_eq!(
            memory.resize_heap_allocation(PROCESS_HEAP, BASE, 2, true),
            Ok(BASE)
        );
        assert_eq!(memory.read_u16(BASE + 2), Ok(0x0403));
        assert!(memory.release_heap_allocation(PROCESS_HEAP, BASE));
        assert_eq!(memory.read_u32(BASE), Ok(0x0403_0201));
        assert_eq!(memory.read_u64(BASE - 16), Ok(0));
        assert_eq!(memory.read_u64(BASE + 16), Ok(0));
    }
}

#[cfg(test)]
mod bounds_access_contract_tests {
    use super::*;

    #[test]
    fn permitted_empty_spans_and_high_addresses_keep_exact_bounds() {
        let mut memory = GuestMemory::new();
        memory
            .map_region_with_data(
                u64::MAX - 8,
                8,
                MemoryPermissions::READ_WRITE,
                &[1, 2, 3, 4, 5, 6, 7, 8],
            )
            .unwrap();
        for at in [u64::MAX - 8, u64::MAX - 4, u64::MAX - 1, u64::MAX] {
            assert_eq!(memory.read_bytes(at, &mut []), Ok(()));
            assert_eq!(memory.write_bytes(at, &[]), Ok(()));
        }
        assert_eq!(memory.read_u64(u64::MAX - 8), Ok(0x0807_0605_0403_0201));
        assert_eq!(memory.read_u8(u64::MAX - 1), Ok(8));
        let mut failed = [0xee; 2];
        assert_eq!(
            memory.read_bytes(u64::MAX - 1, &mut failed),
            Err(MemoryError::Overflow)
        );
        assert_eq!(failed, [0xee; 2]);
        assert_eq!(
            memory.write_u16(u64::MAX - 1, 0xffff),
            Err(MemoryError::Overflow)
        );
        assert_eq!(memory.read_u64(u64::MAX - 7), Err(MemoryError::Overflow));
        assert_eq!(
            memory.write_u64(u64::MAX - 7, u64::MAX),
            Err(MemoryError::Overflow)
        );
        memory.write_u8(u64::MAX - 1, 0x99).unwrap();
        let mut actual = [0; 8];
        memory.read_bytes(u64::MAX - 8, &mut actual).unwrap();
        assert_eq!(actual, [1, 2, 3, 4, 5, 6, 7, 0x99]);
    }
}

#[cfg(test)]
mod mapping_search_contract_tests {
    use super::*;

    #[test]
    fn every_mapping_and_gap_keeps_its_bytes_across_table_sizes() {
        const BASE: u64 = 0x40_0000;
        for count in [0usize, 1, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33] {
            let mut memory = GuestMemory::new();
            for index in (0..count).rev() {
                memory
                    .map_region_with_data(
                        BASE + index as u64 * 0x100,
                        8,
                        MemoryPermissions::READ_WRITE,
                        &[(index + 1) as u8; 8],
                    )
                    .unwrap();
            }
            assert_eq!(
                memory.read_u8(BASE - 1),
                Err(MemoryError::UnmappedAddress(BASE - 1))
            );
            if count == 0 {
                assert_eq!(
                    memory.read_u8(BASE),
                    Err(MemoryError::UnmappedAddress(BASE))
                );
                assert_eq!(
                    memory.write_u8(BASE, 1),
                    Err(MemoryError::UnmappedAddress(BASE))
                );
                assert_eq!(
                    memory.read_bytes(BASE, &mut []),
                    Err(MemoryError::UnmappedAddress(BASE))
                );
            }
            for index in 0..count {
                let address = BASE + index as u64 * 0x100;
                let mut actual = [0; 8];
                memory.read_bytes(address, &mut actual).unwrap();
                assert_eq!(actual, [(index + 1) as u8; 8]);
                let replacement = [0x80 + index as u8; 8];
                memory.write_bytes(address, &replacement).unwrap();
                memory.read_bytes(address, &mut actual).unwrap();
                assert_eq!(actual, replacement);
                assert_eq!(memory.read_bytes(address + 8, &mut []), Ok(()));
                assert_eq!(
                    memory.read_u8(address + 8),
                    Err(MemoryError::UnmappedAddress(address + 8))
                );
                let mut failed = [0xee; 2];
                assert_eq!(
                    memory.read_bytes(address + 7, &mut failed),
                    Err(MemoryError::UnmappedAddress(address + 8))
                );
                assert_eq!(failed, [0xee; 2]);
            }
        }
    }
}
