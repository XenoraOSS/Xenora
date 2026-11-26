// SPDX-License-Identifier: GPL-3.0-or-later

//! Thread-local storage layout as compiled code reads it (#76).
//!
//! Microsoft's public PE Format page ("The .tls Section") describes the
//! access: load the address of the thread's TLS array from the thread
//! environment block, load the module's TLS index from the address the TLS
//! directory names, use the index times the pointer size as an offset into
//! the array to get the address of the module's data block, then add the
//! variable's offset in the template. The block starts as a copy of the
//! template followed by `SizeOfZeroFill` zero bytes.
//!
//! The startup harness stores the `thread_tls_base` it passes to `setup_tls`
//! in the TEB's TLS pointer field, so these tests start from that value and
//! follow the same steps through guest memory. The directory and template are
//! original. Nothing here ran on Windows.

use xenora_loader::startup::{StartupLayout, run_synthetic_startup};
use xenora_loader::{
    ExitReason, GuestMemory, ImportRegistry, MemoryError, MemoryPermissions as P, Register,
    TlsDirectory, setup_tls,
};

const IMAGE: u64 = 0x1_4000_0000;
const TEMPLATE: u64 = IMAGE + 0x3000;
const INDEX: u64 = IMAGE + 0x4000;
const THREAD_TLS: u64 = 0x3_0000_0000;
/// Read as a pointer, the first eight bytes are the template's own address, so
/// a layout that takes the data for the array still finds readable memory.
const TEMPLATE_BYTES: [u8; 20] = [
    0x00, 0x30, 0x00, 0x40, 0x01, 0x00, 0x00, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
    0xA1, 0xB2, 0xC3, 0xD4,
];

fn directory(template_len: u64, zero_fill: u32) -> TlsDirectory {
    TlsDirectory {
        raw_data_start_va: TEMPLATE,
        raw_data_end_va: TEMPLATE + template_len,
        index_address: INDEX,
        size_of_zero_fill: zero_fill,
        ..TlsDirectory::default()
    }
}

/// An image with the template in read-only data and a writable index that
/// does not start at zero.
fn image(base: u64) -> GuestMemory {
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(
            TEMPLATE - IMAGE + base,
            0x1000,
            P::READ_ONLY,
            &TEMPLATE_BYTES,
        )
        .unwrap();
    memory
        .map_region_with_data(INDEX - IMAGE + base, 0x1000, P::READ_WRITE, &[0xFF; 8])
        .unwrap();
    memory
}

/// The address of the module's data block, found the way compiled code does.
fn block_address(memory: &GuestMemory, teb_tls_pointer: u64, index_address: u64) -> u64 {
    let index = u64::from(memory.read_u32(index_address).unwrap());
    memory.read_u64(teb_tls_pointer + index * 8).unwrap()
}

#[test]
fn the_tls_array_slot_leads_to_a_copy_of_the_template() {
    let mut memory = image(IMAGE);
    setup_tls(&directory(20, 12), IMAGE, IMAGE, &mut memory, THREAD_TLS).unwrap();

    assert_eq!(memory.read_u32(INDEX), Ok(0));
    let block = block_address(&memory, THREAD_TLS, INDEX);
    assert_ne!(block, THREAD_TLS, "the array is not the data block");
    assert_ne!(block, TEMPLATE, "the block is the thread's own copy");
    let mut data = [0xEEu8; 32];
    memory.read_bytes(block, &mut data).unwrap();
    assert_eq!(data[..20], TEMPLATE_BYTES);
    assert_eq!(data[20..], [0u8; 12]);
}

#[test]
fn the_thread_block_is_writable_and_separate_from_the_template() {
    let mut memory = image(IMAGE);
    setup_tls(&directory(20, 12), IMAGE, IMAGE, &mut memory, THREAD_TLS).unwrap();
    let block = block_address(&memory, THREAD_TLS, INDEX);

    memory.write_u64(block + 8, 0x0102_0304_0506_0708).unwrap();
    assert_eq!(memory.read_u64(block + 8), Ok(0x0102_0304_0506_0708));
    // The image's template is the initial value for later threads.
    let mut template = [0u8; 20];
    memory.read_bytes(TEMPLATE, &mut template).unwrap();
    assert_eq!(template, TEMPLATE_BYTES);
    // The array slot still names the block after the block is written.
    assert_eq!(block_address(&memory, THREAD_TLS, INDEX), block);
}

#[test]
fn a_rebased_image_uses_its_loaded_addresses() {
    let loaded = IMAGE + 0x20_0000;
    let mut memory = image(loaded);
    setup_tls(&directory(20, 0), loaded, IMAGE, &mut memory, THREAD_TLS).unwrap();

    let index_address = INDEX - IMAGE + loaded;
    assert_eq!(memory.read_u32(index_address), Ok(0));
    let block = block_address(&memory, THREAD_TLS, index_address);
    let mut data = [0u8; 20];
    memory.read_bytes(block, &mut data).unwrap();
    assert_eq!(data, TEMPLATE_BYTES);
}

#[test]
fn a_zero_fill_only_directory_gets_a_zeroed_block() {
    let mut memory = image(IMAGE);
    setup_tls(&directory(0, 24), IMAGE, IMAGE, &mut memory, THREAD_TLS).unwrap();
    let block = block_address(&memory, THREAD_TLS, INDEX);
    let mut data = [0xEEu8; 24];
    memory.read_bytes(block, &mut data).unwrap();
    assert_eq!(data, [0u8; 24]);
}

#[test]
fn an_inverted_template_range_is_rejected_before_anything_is_mapped() {
    let mut memory = image(IMAGE);
    let mut inverted = directory(20, 0);
    inverted.raw_data_end_va = TEMPLATE - 1;
    assert!(setup_tls(&inverted, IMAGE, IMAGE, &mut memory, THREAD_TLS).is_err());
    assert_eq!(
        memory.read_u64(THREAD_TLS),
        Err(MemoryError::UnmappedAddress(THREAD_TLS))
    );
}

#[test]
fn main_thread_can_read_static_tls_on_both_sides_of_the_teb_boundary() {
    const TEMPLATE_RVA: u64 = 0x3020;
    const BLOCKS: [(u32, u64); 2] = [(60 * 1024, 0x1000), (60 * 1024 + 1, 0x12000)];

    for (size, block_offset) in BLOCKS {
        // Extend the repository's original TLS fixture at its preferred base,
        // so the two new template pointers need no relocations.
        let mut pe = include_bytes!("../../../fixtures/pe/trusted-runner.exe").to_vec();
        pe[0x6a0..0x6a8].copy_from_slice(&(IMAGE + TEMPLATE_RVA).to_le_bytes());
        pe[0x6a8..0x6b0].copy_from_slice(&(IMAGE + TEMPLATE_RVA + 2).to_le_bytes());
        pe[0x6c0..0x6c4].copy_from_slice(&(size - 2).to_le_bytes());
        pe[0x820..0x822].copy_from_slice(&[0xa5, 0x5a]);
        let mut entry = vec![
            0x65, 0x48, 0x8b, 0x04, 0x25, 0x58, 0, 0, 0, // mov rax, gs:[0x58]
            0x48, 0x8b, 0x00, // mov rax, [rax] (module index zero)
            0x0f, 0xb7, 0x08, // movzx ecx, word ptr [rax]
            0x0f, 0xb6, 0x90, // movzx edx, byte ptr [rax+size-1]
        ];
        entry.extend_from_slice(&(size - 1).to_le_bytes());
        entry.push(0xf4); // hlt
        pe[0x400..0x400 + entry.len()].copy_from_slice(&entry);

        let mut imports = ImportRegistry::new();
        imports.register("xenora.dll", "fixture_draw", |_, _, _| Ok(0));
        let result = run_synthetic_startup(
            &pe,
            StartupLayout {
                image_base: IMAGE,
                stack_top: 0x2_0001_0000,
                tls_base: THREAD_TLS,
            },
            imports,
            100,
        )
        .unwrap_or_else(|error| panic!("start guest with {size} bytes of static TLS: {error}"));
        assert_eq!(result.execution.exit_reason, ExitReason::Halt);
        let cpu = result.execution.final_state;
        assert_eq!(cpu.gs_base, THREAD_TLS + 0x10000);
        assert_eq!(cpu.get(Register::Rax), THREAD_TLS + block_offset);
        assert_eq!(cpu.get(Register::Rcx), 0x5aa5);
        assert_eq!(cpu.get(Register::Rdx), 0);
        assert_eq!(result.tls_callbacks, 1);
    }
}
