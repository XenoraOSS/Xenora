// SPDX-License-Identifier: GPL-3.0-or-later

//! The guest CPUID contract (issue #130).
//!
//! CPUID reports a fixed synthetic processor. A feature bit is set only when
//! every instruction of that feature is implemented, so each bit that is set
//! is checked against instructions that run, and each bit software commonly
//! tests that is clear has a witness instruction of that feature which both
//! engines refuse. The witness must be removed before its bit can be set.
//!
//! Bit positions and the feature each instruction belongs to are from the
//! Intel SDM Vol. 2, CPUID and the instruction entries. A feature whose own
//! instructions all run stays clear while one it builds on is clear, and has
//! its instruction list checked instead. Encodings were
//! assembled with `clang -target x86_64-apple-macos` and read with `objdump`.

use xenora_loader::cpu::{
    CPUID_CMOV, CPUID_LAHF_SAHF, CPUID_LONG_MODE, CPUID_LZCNT, CPUID_POPCNT, CpuState, ExitReason,
    Interpreter, RFlags, cpuid,
};
use xenora_loader::dbt::DbtEngine;
use xenora_loader::memory::{GuestMemory, MemoryPermissions};
use xenora_loader::x86_decode::decode;

const ENTRY: u64 = 0x1000;
const RAX: usize = 0;
const RCX: usize = 1;
const RDX: usize = 2;
const RBX: usize = 3;
const RSI: usize = 6;

/// Index of a CPUID result, which come in the order EAX, EBX, ECX, EDX.
const EBX: usize = 1;
const ECX: usize = 2;
const EDX: usize = 3;

fn image(code: &[u8]) -> GuestMemory {
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, code)
        .unwrap();
    memory
}

/// Final state and exit reason from the interpreter, then from the DBT.
fn both(cpu: &CpuState, code: &[u8], budget: usize) -> [(&'static str, CpuState, ExitReason); 2] {
    let mut memory = image(code);
    let interpreted = Interpreter::new(cpu.clone(), &mut memory, budget).run();
    let mut memory = image(code);
    let mut state = cpu.clone();
    let translated = DbtEngine::new(64 * 1024)
        .unwrap()
        .run(&mut state, &mut memory, budget);
    [
        (
            "interpreter",
            interpreted.final_state,
            interpreted.exit_reason,
        ),
        ("DBT", translated.final_state, translated.exit_reason),
    ]
}

const SET: RFlags = RFlags {
    cf: true,
    zf: true,
    sf: true,
    of: true,
    pf: true,
    af: true,
};

#[test]
fn every_leaf_returns_its_documented_values() {
    // (leaf, subleaf, [EAX, EBX, ECX, EDX])
    let leaves: &[(u32, u32, [u32; 4])] = &[
        // Highest basic leaf 1, and "Xeno", "uest", "ra g" as little-endian text.
        (0, 0, [1, 0x6F6E_6558, 0x7473_6575, 0x6720_6172]),
        // POPCNT is ECX bit 23 and CMOV is EDX bit 15. No signature.
        (1, 0, [0, 0, 0x0080_0000, 0x0000_8000]),
        (1, 0xFFFF_FFFF, [0, 0, 0x0080_0000, 0x0000_8000]),
        // Highest extended leaf.
        (0x8000_0000, 0, [0x8000_0001, 0, 0, 0]),
        // LAHF/SAHF is ECX bit 0, LZCNT ECX bit 5 and long mode EDX bit 29.
        (0x8000_0001, 0, [0, 0, 0x0000_0021, 0x2000_0000]),
        (0x8000_0001, 7, [0, 0, 0x0000_0021, 0x2000_0000]),
        // Everything else, above or between the highest leaves, is zeros.
        (2, 0, [0; 4]),
        (4, 0, [0; 4]),
        (6, 0, [0; 4]),
        (7, 0, [0; 4]),
        (7, 1, [0; 4]),
        (0xB, 0, [0; 4]),
        (0xD, 1, [0; 4]),
        (0x16, 0, [0; 4]),
        (0x4000_0000, 0, [0; 4]),
        (0x7FFF_FFFF, 0, [0; 4]),
        (0x8000_0002, 0, [0; 4]),
        (0x8000_0008, 0, [0; 4]),
        (0xFFFF_FFFF, 0, [0; 4]),
    ];
    for &(leaf, subleaf, wanted) in leaves {
        // Only EAX and ECX select the leaf: the upper halves are noise.
        let mut cpu = CpuState::new(ENTRY, 0);
        for (index, register) in cpu.gpr.iter_mut().enumerate() {
            *register = 0xA5A5_0000_1111_0000 + index as u64;
        }
        cpu.gpr[RAX] = 0xDEAD_BEEF_0000_0000 | u64::from(leaf);
        cpu.gpr[RCX] = 0xFEED_FACE_0000_0000 | u64::from(subleaf);
        cpu.flags = SET;
        // Each result is a 32-bit register write, so it clears the upper half.
        // CPUID changes no flag and no other register.
        let mut expected = cpu.clone();
        expected.rip = ENTRY + 2;
        for (register, value) in [RAX, RBX, RCX, RDX].into_iter().zip(wanted) {
            expected.gpr[register] = u64::from(value);
        }
        for (engine, state, exit) in both(&cpu, &[0x0F, 0xA2], 1) {
            let what = format!("leaf {leaf:#x} subleaf {subleaf:#x} on the {engine}");
            assert_eq!(exit, ExitReason::StepLimitExceeded, "{what}");
            assert_eq!(state, expected, "{what}");
        }
        // The function a runtime service would read agrees with the instruction.
        assert_eq!(cpuid(leaf, subleaf), wanted, "leaf {leaf:#x}");
    }

    let [_, ebx, ecx, edx] = cpuid(0, 0);
    let vendor: Vec<u8> = [ebx, edx, ecx]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    assert_eq!(vendor, b"Xenora guest");
    assert_eq!(
        (
            CPUID_POPCNT,
            CPUID_CMOV,
            CPUID_LAHF_SAHF,
            CPUID_LZCNT,
            CPUID_LONG_MODE
        ),
        (1 << 23, 1 << 15, 1, 1 << 5, 1 << 29)
    );
}

#[test]
fn reported_features_have_all_their_instructions_in_the_fixtures() {
    // `tests/x86_integer.rs` runs every line of this file through both engines
    // against an independent reference. Here each reported feature must have
    // its whole instruction set among those lines.
    let fixture = include_str!("x86/integer.txt");
    let decodable = |text: &str| {
        let line = fixture
            .lines()
            .find(|line| line.split_once(" | ").is_some_and(|(_, asm)| asm == text))
            .unwrap_or_else(|| panic!("no fixture for {text}"));
        let code: Vec<u8> = line
            .split_once(" | ")
            .unwrap()
            .0
            .split(' ')
            .map(|byte| u8::from_str_radix(byte, 16).unwrap())
            .collect();
        decode(ENTRY, |at| {
            code.get((at - ENTRY) as usize).copied().ok_or(())
        })
        .unwrap_or_else(|error| panic!("{text}: {error:?}"));
    };
    let memory = |bits: u32| {
        let width = match bits {
            16 => "word",
            32 => "dword",
            _ => "qword",
        };
        format!("{width} ptr [rsi + 4*rdi + 16]")
    };
    // 16-, 32- and 64-bit destinations (RBX and R10) from a register and from memory.
    let sizes = [
        (16, "bx", "r9w", "r10w"),
        (32, "ebx", "r9d", "r10d"),
        (64, "rbx", "r9", "r10"),
    ];

    assert_ne!(cpuid(1, 0)[EDX] & CPUID_CMOV, 0);
    for condition in "o no b ae e ne be a s ns p np l ge le g".split(' ') {
        for (bits, low, source, high) in sizes {
            decodable(&format!("cmov{condition} {low}, {source}"));
            decodable(&format!("cmov{condition} {high}, {}", memory(bits)));
        }
    }
    for (feature, bit, name) in [
        (cpuid(1, 0)[ECX], CPUID_POPCNT, "popcnt"),
        (cpuid(0x8000_0001, 0)[ECX], CPUID_LZCNT, "lzcnt"),
    ] {
        assert_ne!(feature & bit, 0, "{name}");
        for (bits, low, source, high) in sizes {
            decodable(&format!("{name} {low}, {source}"));
            decodable(&format!("{name} {high}, {}", memory(bits)));
        }
    }
    // LAHF and SAHF have one form each and no operands.
    assert_ne!(cpuid(0x8000_0001, 0)[ECX] & CPUID_LAHF_SAHF, 0);
    decodable("lahf");
    decodable("sahf");
    // Long mode: 64-bit operands and the registers only it has.
    assert_ne!(cpuid(0x8000_0001, 0)[EDX] & CPUID_LONG_MODE, 0);
    decodable("add r9, r12");
    decodable("movabs r15, 0x8877665544332211");
}

#[test]
fn unreported_features_each_have_an_instruction_that_is_refused() {
    // (feature, leaf, result register, bit, an instruction of that feature)
    let witnesses: &[(&str, u32, usize, u32, &[u8])] = &[
        ("FPU", 1, EDX, 0, &[0xD9, 0xE8]),               // fld1
        ("TSC", 1, EDX, 4, &[0x0F, 0x31]),               // rdtsc
        ("CX8", 1, EDX, 8, &[0x0F, 0xC7, 0x0E]),         // cmpxchg8b [rsi]
        ("CLFSH", 1, EDX, 19, &[0x0F, 0xAE, 0x3E]),      // clflush [rsi]
        ("MMX", 1, EDX, 23, &[0x0F, 0x77]),              // emms
        ("FXSR", 1, EDX, 24, &[0x0F, 0xAE, 0x06]),       // fxsave [rsi]
        ("SSE", 1, EDX, 25, &[0x0F, 0x52, 0xC1]),        // rsqrtps xmm0, xmm1
        ("SSE2", 1, EDX, 26, &[0x66, 0x0F, 0xF7, 0xC1]), // maskmovdqu xmm0, xmm1
        ("SSE3", 1, ECX, 0, &[0xF2, 0x0F, 0x7C, 0xC1]),  // haddps xmm0, xmm1
        (
            "PCLMULQDQ",
            1,
            ECX,
            1,
            &[0x66, 0x0F, 0x3A, 0x44, 0xC1, 0x00],
        ), // pclmulqdq xmm0, xmm1, 0
        ("SSSE3", 1, ECX, 9, &[0x0F, 0x38, 0x00, 0xC1]), // pshufb mm0, mm1
        ("FMA", 1, ECX, 12, &[0xC4, 0xE2, 0x71, 0x98, 0xC2]), // vfmadd132ps xmm0, xmm1, xmm2
        ("CMPXCHG16B", 1, ECX, 13, &[0x48, 0x0F, 0xC7, 0x0E]), // cmpxchg16b [rsi]
        ("SSE4.2", 1, ECX, 20, &[0xF2, 0x0F, 0x38, 0xF1, 0xC1]), // crc32 eax, ecx
        ("MOVBE", 1, ECX, 22, &[0x0F, 0x38, 0xF0, 0x06]), // movbe eax, [rsi]
        ("AESNI", 1, ECX, 25, &[0x66, 0x0F, 0x38, 0xDC, 0xC1]), // aesenc xmm0, xmm1
        ("XSAVE", 1, ECX, 26, &[0x0F, 0x01, 0xD0]),      // xgetbv
        ("AVX", 1, ECX, 28, &[0xC5, 0xF4, 0x58, 0xC2]),  // vaddps ymm0, ymm1, ymm2
        ("F16C", 1, ECX, 29, &[0xC4, 0xE2, 0x79, 0x13, 0xC1]), // vcvtph2ps xmm0, xmm1
        ("RDRAND", 1, ECX, 30, &[0x0F, 0xC7, 0xF0]),     // rdrand eax
        ("BMI1", 7, EBX, 3, &[0xC4, 0xE2, 0x70, 0xF2, 0xC2]), // andn eax, ecx, edx
        ("AVX2", 7, EBX, 5, &[0xC5, 0xF5, 0xFE, 0xC2]),  // vpaddd ymm0, ymm1, ymm2
        ("BMI2", 7, EBX, 8, &[0xC4, 0xE2, 0x73, 0xF6, 0xC2]), // mulx eax, ecx, edx
        ("RDSEED", 7, EBX, 18, &[0x0F, 0xC7, 0xF8]),     // rdseed eax
        ("ADX", 7, EBX, 19, &[0x66, 0x0F, 0x38, 0xF6, 0xC1]), // adcx eax, ecx
        ("SHA", 7, EBX, 29, &[0x0F, 0x38, 0xC9, 0xC1]),  // sha1msg1 xmm0, xmm1
        ("PREFETCHW", 0x8000_0001, ECX, 8, &[0x0F, 0x0D, 0x0E]), // prefetchw [rsi]
        ("SYSCALL/SYSRET", 0x8000_0001, EDX, 11, &[0x48, 0x0F, 0x07]), // sysretq
        ("RDTSCP", 0x8000_0001, EDX, 27, &[0x0F, 0x01, 0xF9]), // rdtscp
    ];
    for &(feature, leaf, register, bit, code) in witnesses {
        assert_eq!(
            cpuid(leaf, 0)[register] >> bit & 1,
            0,
            "{feature} is reported"
        );
        let mut padded = code.to_vec();
        padded.extend([0x90; 8]);
        let mut cpu = CpuState::new(ENTRY, 0);
        cpu.gpr[RSI] = ENTRY;
        for (engine, state, exit) in both(&cpu, &padded, 1) {
            assert!(
                matches!(exit, ExitReason::IllegalInstruction(_)),
                "{feature}: {code:02X?} ran on the {engine} ({exit:?}); report the feature only \
                 once all of it is implemented"
            );
            assert_eq!(state, cpu, "{feature} on the {engine}");
        }
    }
}

/// SDM Vol. 1, 12.12.2: an application checks for SSE, SSE2, SSE3 and SSSE3 before
/// SSE4.1. Those are not complete, so the bit stays clear although every SSE4.1
/// instruction runs. Each has a fixture that the tests of
/// `x86_vector.rs` or `x86_float.rs` run in both engines.
#[test]
fn sse4_1_is_complete_but_unreported_while_its_prerequisites_are() {
    let fixtures = [
        include_str!("x86/vector.txt"),
        include_str!("x86/float.txt"),
    ]
    .concat();
    let has = |name: &str, memory: bool| {
        fixtures.lines().any(|line| {
            let text = line.split_once(" | ").unwrap().1;
            text.split(' ').next() == Some(name) && text.contains(" ptr ") == memory
        })
    };
    // Every instruction the SDM assigns to the feature. MOVNTDQA has no register
    // form; PEXTRW belongs to it only as a store.
    let names = "blendpd blendps blendvpd blendvps dppd dpps extractps insertps mpsadbw packusdw \
                 pblendvb pblendw pcmpeqq pextrb pextrd pextrq phminposuw pinsrb pinsrd pinsrq \
                 pmaxsb pmaxsd pmaxud pmaxuw pminsb pminsd pminud pminuw pmovsxbw pmovsxbd \
                 pmovsxbq pmovsxwd pmovsxwq pmovsxdq pmovzxbw pmovzxbd pmovzxbq pmovzxwd \
                 pmovzxwq pmovzxdq pmuldq pmulld ptest roundpd roundps roundsd roundss";
    for name in names.split_whitespace() {
        assert!(has(name, false) && has(name, true), "{name}");
    }
    assert!(has("movntdqa", true) && has("pextrw", true));

    let [_, _, ecx, edx] = cpuid(1, 0);
    let prerequisites = [edx >> 25, edx >> 26, ecx, ecx >> 9].map(|bit| bit & 1);
    assert_eq!(
        (prerequisites, ecx >> 19 & 1),
        ([0; 4], 0),
        "SSE, SSE2, SSE3 and SSSE3 come before SSE4.1"
    );
}

#[test]
fn a_guest_can_use_an_instruction_after_testing_its_feature_bit() {
    //       mov eax, 1
    //       cpuid
    //       bt ecx, 23        ; POPCNT
    //       jnc done
    //       popcnt rax, rsi
    // done: hlt
    let code = [
        0xB8, 0x01, 0x00, 0x00, 0x00, 0x0F, 0xA2, 0x0F, 0xBA, 0xE1, 0x17, 0x73, 0x05, 0xF3, 0x48,
        0x0F, 0xB8, 0xC6, 0xF4,
    ];
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr[RSI] = 0xF0F0_0000_0000_0001;
    for (engine, state, exit) in both(&cpu, &code, 16) {
        assert_eq!(exit, ExitReason::Halt, "{engine}");
        // Nine bits are set in RSI. Leaf 1 leaves EAX zero, so a skipped
        // POPCNT would show as 0.
        assert_eq!(state.gpr[RAX], 9, "{engine}");
        assert_eq!(state.rip, ENTRY + code.len() as u64, "{engine}");
    }
}
