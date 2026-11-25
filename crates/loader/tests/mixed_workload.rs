// SPDX-License-Identifier: GPL-3.0-or-later

//! Original mixed CPU-to-GPU workload (#147).
//!
//! An x86-64 loop in a synthetic PE32+ image, loaded by the PE loader and run
//! by the ARM64 dynamic binary translator (DBT), fills a guest buffer through
//! checked guest memory. The host then reads that buffer and dispatches the
//! translated `fixtures/rdna2/add.bin` compute shader on native Metal.
//!
//! Expected values come from the workload formula evaluated in plain Rust
//! ([`expected_words`]) and the `add` fixture's documented constant, never
//! from the DBT or the GPU. `benches/mixed_workload.rs` includes this file
//! and times only runs that pass [`MixedWorkload::check`].

use xenora_loader::cpu::{CpuState, ExecutionResult, ExitReason, Register};
use xenora_loader::dbt::DbtEngine;
use xenora_loader::memory::{MemoryError, MemoryPermissions};
use xenora_loader::pe::{LoadedImage, ParsedPe};

/// Guest address of the output buffer the guest loop writes.
pub const OUTPUT: u64 = 0x5000_0000;
/// Capacity of the mapped output buffer in 32-bit words. It equals the
/// largest grid `NativeCompute` accepts for one dispatch.
pub const OUTPUT_WORDS: u32 = 4096;
const STACK: u64 = 0x2000_0000;
const STACK_LEN: usize = 0x1_0000;
const IMAGE_BASE: u64 = 0x1_4000_0000;

/// Initial loop state and per-iteration increment of the guest loop.
pub const SEED: u32 = 0x0BAD_F00D;
pub const STEP: u32 = 0x9E37_79B9;
pub const MASK: u32 = 0xA5A5_A5A5;
/// Constant the `add` fixture adds to each input word, as declared by its
/// `input`/`expected` pair in `benchmarks/manifest.toml` (0 becomes
/// 305419896).
pub const ADD_CONSTANT: u32 = 0x1234_5678;

/// The guest loop. Inputs: `rdi` = output pointer, `ecx` = word count (at
/// least one), `eax` = seed. On return `ebx` holds the 32-bit sum of the
/// words written. Every instruction here is in the DBT's native set.
#[rustfmt::skip]
const GUEST_LOOP: &[u8] = &[
    0x31, 0xDB,                   // xor ebx, ebx
    // loop:
    0x8D, 0x14, 0x80,             // lea edx, [rax + rax*4]
    0x81, 0xF2, 0xA5, 0xA5, 0xA5, 0xA5, // xor edx, MASK
    0x89, 0x17,                   // mov [rdi], edx
    0x01, 0xD3,                   // add ebx, edx
    0x05, 0xB9, 0x79, 0x37, 0x9E, // add eax, STEP
    0x48, 0x83, 0xC7, 0x04,       // add rdi, 4
    0xFF, 0xC9,                   // dec ecx
    0x75, 0xE6,                   // jnz loop
    0xF4,                         // hlt
];

/// Word `i` of the workload, evaluated directly from its definition:
/// `w_i = (5 * x_i) ^ MASK` with `x_i = SEED + i * STEP`, all modulo 2^32.
#[must_use]
pub fn expected_words(count: u32) -> Vec<u32> {
    (0..count)
        .map(|i| {
            let x = SEED.wrapping_add(i.wrapping_mul(STEP));
            x.wrapping_mul(5) ^ MASK
        })
        .collect()
}

/// The guest checksum for `count` words: their sum modulo 2^32.
#[must_use]
pub fn expected_checksum(count: u32) -> u32 {
    expected_words(count).into_iter().fold(0, u32::wrapping_add)
}

/// The GPU output for guest words `words` under the `add` fixture.
#[must_use]
pub fn expected_gpu(words: &[u32]) -> Vec<u32> {
    words.iter().map(|w| w.wrapping_add(ADD_CONSTANT)).collect()
}

/// A minimal PE32+ image with one executable `.text` section at RVA 0x1000.
fn build_image(code: &[u8]) -> Vec<u8> {
    fn put(pe: &mut [u8], at: usize, bytes: &[u8]) {
        pe[at..at + bytes.len()].copy_from_slice(bytes);
    }
    assert!(code.len() <= 0x200);
    let mut pe = vec![0u8; 0x400];
    put(&mut pe, 0, b"MZ");
    put(&mut pe, 0x3C, &0x80u32.to_le_bytes());
    put(&mut pe, 0x80, b"PE\0\0");
    put(&mut pe, 0x84, &0x8664u16.to_le_bytes()); // AMD64
    put(&mut pe, 0x86, &1u16.to_le_bytes()); // one section
    put(&mut pe, 0x94, &240u16.to_le_bytes()); // optional header size
    put(&mut pe, 0x96, &0x0022u16.to_le_bytes()); // executable, large address aware
    let optional = 0x98;
    put(&mut pe, optional, &0x20Bu16.to_le_bytes()); // PE32+
    put(&mut pe, optional + 16, &0x1000u32.to_le_bytes()); // entry point RVA
    put(&mut pe, optional + 24, &IMAGE_BASE.to_le_bytes());
    put(&mut pe, optional + 32, &0x1000u32.to_le_bytes()); // section alignment
    put(&mut pe, optional + 36, &0x200u32.to_le_bytes()); // file alignment
    put(&mut pe, optional + 56, &0x2000u32.to_le_bytes()); // size of image
    put(&mut pe, optional + 60, &0x200u32.to_le_bytes()); // size of headers
    put(&mut pe, optional + 68, &3u16.to_le_bytes()); // console subsystem
    put(&mut pe, optional + 108, &16u32.to_le_bytes()); // data directories
    let section = optional + 240;
    put(&mut pe, section, b".text");
    put(&mut pe, section + 8, &(code.len() as u32).to_le_bytes());
    put(&mut pe, section + 12, &0x1000u32.to_le_bytes());
    put(&mut pe, section + 16, &0x200u32.to_le_bytes());
    put(&mut pe, section + 20, &0x200u32.to_le_bytes());
    put(&mut pe, section + 36, &0x6000_0020u32.to_le_bytes()); // code, read, execute
    put(&mut pe, 0x200, code);
    pe
}

/// Why a run did not produce the expected values.
#[derive(Debug, PartialEq, Eq)]
pub enum Mismatch {
    Exit(ExitReason),
    Checksum {
        expected: u32,
        actual: u64,
    },
    GuestWord {
        index: usize,
        expected: u32,
        actual: u32,
    },
    GuestLength {
        expected: usize,
        actual: usize,
    },
    GpuWord {
        index: usize,
        expected: u32,
        actual: u32,
    },
    GpuLength {
        expected: usize,
        actual: usize,
    },
}

/// The loaded guest image, its output buffer and the DBT that runs it.
pub struct MixedWorkload {
    pub image: LoadedImage,
    pub dbt: DbtEngine,
}

impl Default for MixedWorkload {
    fn default() -> Self {
        Self::new()
    }
}

impl MixedWorkload {
    /// Load the guest image through the PE loader and map its stack and
    /// output buffer.
    ///
    /// # Panics
    ///
    /// Panics if the synthetic image does not load or the JIT cannot be set up.
    #[must_use]
    pub fn new() -> Self {
        let bytes = build_image(GUEST_LOOP);
        let pe = ParsedPe::parse(&bytes).expect("parse the synthetic image");
        let mut image = pe.load_into_memory(&bytes).expect("load the image");
        image
            .memory
            .map_region(STACK, STACK_LEN, MemoryPermissions::READ_WRITE)
            .expect("map the stack");
        image
            .memory
            .map_region(
                OUTPUT,
                OUTPUT_WORDS as usize * 4,
                MemoryPermissions::READ_WRITE,
            )
            .expect("map the output buffer");
        let dbt = DbtEngine::new(1024 * 1024).expect("JIT memory");
        Self { image, dbt }
    }

    /// Zero the whole output buffer, so a run that writes nothing cannot pass
    /// on data left by an earlier run.
    ///
    /// # Panics
    ///
    /// Panics if the mapped buffer cannot be written.
    pub fn clear_output(&mut self) {
        self.image
            .memory
            .write_bytes(OUTPUT, &[0; OUTPUT_WORDS as usize * 4])
            .expect("clear the output buffer");
    }

    /// Run the guest loop for `count` words from the image entry point.
    ///
    /// # Panics
    ///
    /// Panics if `count` is zero, which the loop does not support.
    pub fn run_cpu(&mut self, count: u32) -> ExecutionResult {
        assert!(count > 0, "the guest loop needs at least one word");
        let mut cpu = CpuState::new(self.image.entry_point, STACK + STACK_LEN as u64);
        cpu.set(Register::Rdi, OUTPUT);
        cpu.set(Register::Rcx, u64::from(count));
        cpu.set(Register::Rax, u64::from(SEED));
        // Eight instructions per word plus the prologue and HLT.
        let budget = 8 * count as usize + 16;
        self.dbt.run(&mut cpu, &mut self.image.memory, budget)
    }

    /// Read `count` words of the output buffer through checked guest memory.
    ///
    /// # Errors
    ///
    /// Returns the memory error for a range outside the mapped buffer.
    pub fn guest_words(&self, count: u32) -> Result<Vec<u32>, MemoryError> {
        let mut bytes = vec![0u8; count as usize * 4];
        self.image.memory.read_bytes(OUTPUT, &mut bytes)?;
        Ok(bytes
            .as_chunks::<4>()
            .0
            .iter()
            .copied()
            .map(u32::from_le_bytes)
            .collect())
    }

    /// Compare one run against the independently derived expectations.
    ///
    /// # Errors
    ///
    /// Returns the first mismatch found.
    pub fn check(
        cpu: &ExecutionResult,
        guest: &[u32],
        gpu: &[u32],
        count: u32,
    ) -> Result<(), Mismatch> {
        if cpu.exit_reason != ExitReason::Halt {
            return Err(Mismatch::Exit(cpu.exit_reason));
        }
        let checksum = expected_checksum(count);
        let actual = cpu.final_state.get(Register::Rbx);
        if actual != u64::from(checksum) {
            return Err(Mismatch::Checksum {
                expected: checksum,
                actual,
            });
        }
        let words = expected_words(count);
        if guest.len() != words.len() {
            return Err(Mismatch::GuestLength {
                expected: words.len(),
                actual: guest.len(),
            });
        }
        for (index, (&expected, &actual)) in words.iter().zip(guest).enumerate() {
            if expected != actual {
                return Err(Mismatch::GuestWord {
                    index,
                    expected,
                    actual,
                });
            }
        }
        let expected_out = expected_gpu(&words);
        if gpu.len() != expected_out.len() {
            return Err(Mismatch::GpuLength {
                expected: expected_out.len(),
                actual: gpu.len(),
            });
        }
        for (index, (&expected, &actual)) in expected_out.iter().zip(gpu).enumerate() {
            if expected != actual {
                return Err(Mismatch::GpuWord {
                    index,
                    expected,
                    actual,
                });
            }
        }
        Ok(())
    }
}

/// The translated `add` fixture as MSL.
///
/// # Panics
///
/// Panics if the fixture no longer compiles.
#[must_use]
pub fn add_kernel() -> String {
    let binary = include_bytes!("../../../fixtures/rdna2/add.bin");
    xenora_shader::compile(xenora_shader::Architecture::Gfx1030, binary)
        .expect("compile the add fixture")
        .msl
}

#[test]
fn expectations_follow_the_formula_by_hand() {
    // x_0 = 0x0BADF00D, 5 * x_0 = 0x3A65B041, ^ MASK = 0x9FC015E4.
    // x_1 = 0x0BADF00D + 0x9E3779B9 = 0xA9E569C6, 5 * x_1 = 0x517B10DE
    // (mod 2^32), ^ MASK = 0xF4DEB57B.
    assert_eq!(expected_words(2), [0x9FC0_15E4, 0xF4DE_B57B]);
    assert_eq!(
        expected_checksum(2),
        0x9FC0_15E4u32.wrapping_add(0xF4DE_B57B)
    );
    assert_eq!(expected_gpu(&[0, 0xEDCB_A988]), [0x1234_5678, 0]);
}

#[test]
fn guest_loop_fills_the_buffer_natively() {
    let mut workload = MixedWorkload::new();
    for count in [1, 7, 256, OUTPUT_WORDS] {
        workload.clear_output();
        let before = workload.dbt.metrics.interpreter_fallbacks;
        let result = workload.run_cpu(count);
        assert_eq!(result.exit_reason, ExitReason::Halt, "{count} words");
        assert_eq!(
            result.final_state.get(Register::Rbx),
            u64::from(expected_checksum(count)),
            "{count} words"
        );
        assert_eq!(
            workload.guest_words(count).unwrap(),
            expected_words(count),
            "{count} words"
        );
        // The word after the last one written stays cleared.
        if count < OUTPUT_WORDS {
            let next = workload.guest_words(count + 1).unwrap();
            assert_eq!(next[count as usize], 0, "{count} words");
        }
        assert_eq!(
            workload.dbt.metrics.interpreter_fallbacks, before,
            "every guest instruction is lowered to ARM64"
        );
    }
}

#[test]
fn guest_store_past_the_buffer_faults() {
    let mut workload = MixedWorkload::new();
    let result = workload.run_cpu(OUTPUT_WORDS + 1);
    let end = OUTPUT + u64::from(OUTPUT_WORDS) * 4;
    match result.exit_reason {
        ExitReason::MemoryFault(MemoryError::UnmappedAddress(address)) => {
            assert_eq!(address, end);
        }
        other => panic!("expected an unmapped-address fault at {end:#x}, got {other:?}"),
    }
    // Every word inside the buffer was still written in order.
    assert_eq!(
        workload.guest_words(OUTPUT_WORDS).unwrap(),
        expected_words(OUTPUT_WORDS)
    );
}

#[test]
fn check_rejects_wrong_guest_and_gpu_words() {
    let mut workload = MixedWorkload::new();
    let result = workload.run_cpu(4);
    let guest = workload.guest_words(4).unwrap();
    let gpu = expected_gpu(&expected_words(4));
    assert_eq!(MixedWorkload::check(&result, &guest, &gpu, 4), Ok(()));

    let mut bad_guest = guest.clone();
    bad_guest[2] ^= 1;
    assert!(matches!(
        MixedWorkload::check(&result, &bad_guest, &gpu, 4),
        Err(Mismatch::GuestWord { index: 2, .. })
    ));
    let mut bad_gpu = gpu.clone();
    bad_gpu[3] = bad_gpu[3].wrapping_add(1);
    assert!(matches!(
        MixedWorkload::check(&result, &guest, &bad_gpu, 4),
        Err(Mismatch::GpuWord { index: 3, .. })
    ));
    assert!(matches!(
        MixedWorkload::check(&result, &guest, &gpu[..3], 4),
        Err(Mismatch::GpuLength { .. })
    ));
}

#[test]
fn check_rejects_truncated_and_oversized_guest_output() {
    let mut workload = MixedWorkload::new();
    let result = workload.run_cpu(4);
    let guest = workload.guest_words(4).unwrap();
    let gpu = expected_gpu(&expected_words(4));

    assert_eq!(
        MixedWorkload::check(&result, &guest[..3], &gpu, 4),
        Err(Mismatch::GuestLength {
            expected: 4,
            actual: 3,
        })
    );
    let mut oversized = guest.clone();
    oversized.push(guest[0]);
    assert_eq!(
        MixedWorkload::check(&result, &oversized, &gpu, 4),
        Err(Mismatch::GuestLength {
            expected: 4,
            actual: 5,
        })
    );
    assert_eq!(MixedWorkload::check(&result, &guest, &gpu, 4), Ok(()));
}

#[test]
#[cfg_attr(
    not(feature = "gpu-tests"),
    ignore = "needs a native Metal device; run with --features gpu-tests --test mixed_workload"
)]
fn guest_words_drive_a_native_metal_dispatch() {
    let gpu = xenora_metal::NativeCompute::new().expect("native Metal device");
    let kernel = add_kernel();
    let mut workload = MixedWorkload::new();
    for count in [1, 64, 1000, OUTPUT_WORDS] {
        workload.clear_output();
        let result = workload.run_cpu(count);
        let guest = workload.guest_words(count).expect("read the guest buffer");
        let output = gpu
            .run_words(&kernel, xenora_shader::ENTRY_POINT, &guest)
            .expect("dispatch on Metal");
        assert_eq!(
            MixedWorkload::check(&result, &guest, &output, count),
            Ok(()),
            "{count} words"
        );
    }
}
