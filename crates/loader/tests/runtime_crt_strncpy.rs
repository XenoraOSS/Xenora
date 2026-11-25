// SPDX-License-Identifier: GPL-3.0-or-later

//! strncpy and wcsncpy through the guest import table. C requires both to pad
//! the destination with zeros up to the count. The count comes from the guest,
//! so a count far past the destination must fault the guest at the first byte
//! it cannot write, not exhaust host memory. The runtime_services fuzz replay
//! found the original host crash with an all-ones count.

mod support;

use support::*;
use xenora_loader::cpu::ExitReason;

const UCRT: &str = "ucrtbase.dll";
const IMPORTS: &[(&str, &str)] = &[(UCRT, "strncpy"), (UCRT, "wcsncpy")];

const SRC: u64 = SCRATCH;
const DEST: u64 = SCRATCH + 0x100;

#[test]
fn strncpy_pads_to_the_count() {
    let mut guest = Guest::new(IMPORTS, &[]);
    guest.memory.write_bytes(SRC, b"ab\0").unwrap();
    guest.memory.write_bytes(DEST, &[b'#'; 16]).unwrap();
    assert_eq!(guest.call("strncpy", &[DEST, SRC, 8]), DEST);
    let mut out = [0u8; 9];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = guest.memory.read_u8(DEST + i as u64).unwrap();
    }
    assert_eq!(
        &out, b"ab\0\0\0\0\0\0#",
        "padded to eight bytes, no further"
    );
}

#[test]
fn wcsncpy_pads_to_the_count_in_characters() {
    let mut guest = Guest::new(IMPORTS, &[]);
    guest.memory.write_u16(SRC, u16::from(b'x')).unwrap();
    guest.memory.write_u16(SRC + 2, 0).unwrap();
    guest.memory.write_bytes(DEST, &[0xFF; 16]).unwrap();
    assert_eq!(guest.call("wcsncpy", &[DEST, SRC, 4]), DEST);
    assert_eq!(guest.u16_at(DEST), u16::from(b'x'));
    for i in 1..4 {
        assert_eq!(guest.u16_at(DEST + 2 * i), 0);
    }
    assert_eq!(
        guest.u16_at(DEST + 8),
        0xFFFF,
        "four characters, no further"
    );
}

#[test]
fn a_count_past_the_destination_faults_the_guest() {
    let mut guest = Guest::new(IMPORTS, &[]);
    guest.memory.write_bytes(SRC, b"ab\0").unwrap();
    guest.memory.write_u16(SRC + 0x10, 0).unwrap();
    for (symbol, source) in [("strncpy", SRC), ("wcsncpy", SRC + 0x10)] {
        for count in [u64::MAX, SCRATCH_LEN * 4] {
            let reason = guest.fails(symbol, &[DEST, source, count]);
            assert!(
                matches!(reason, ExitReason::MemoryFault(_)),
                "{symbol} count {count:#x}: {reason:?}"
            );
        }
    }
}
