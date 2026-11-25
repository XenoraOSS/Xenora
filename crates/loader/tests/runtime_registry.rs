// SPDX-License-Identifier: GPL-3.0-or-later

//! The honestly empty registry through the guest import table.

mod support;

use support::*;

const ADVAPI32: &str = "advapi32.dll";
const KERNEL32: &str = "kernel32.dll";

const IMPORTS: &[(&str, &str)] = &[
    (ADVAPI32, "RegOpenKeyExA"),
    (ADVAPI32, "RegOpenKeyExW"),
    (ADVAPI32, "RegQueryValueExA"),
    (ADVAPI32, "RegCloseKey"),
    (KERNEL32, "GetLastError"),
];

const HKCU: u64 = 0x8000_0001;
const HKLM: u64 = 0x8000_0002;

fn narrow(guest: &mut Guest, at: u64, text: &str) {
    guest.memory.write_bytes(at, text.as_bytes()).unwrap();
    guest.memory.write_u8(at + text.len() as u64, 0).unwrap();
}

#[test]
fn the_empty_registry_opens_its_roots_and_answers_absent() {
    let mut guest = Guest::new(IMPORTS, &[]);

    // A predefined root opens when the subkey is null or empty.
    let output = SCRATCH;
    assert_eq!(guest.call("RegOpenKeyExA", &[HKCU, 0, 0, 0, output]), 0);
    let key = guest.u64_at(output);
    assert_ne!(key, 0);
    assert_ne!(key, HKCU);

    narrow(&mut guest, SCRATCH + 0x100, "");
    assert_eq!(
        guest.call("RegOpenKeyExA", &[HKCU, SCRATCH + 0x100, 0, 0, output]),
        0
    );

    // Any named subkey is absent in the empty registry.
    narrow(&mut guest, SCRATCH + 0x100, "Software\\Xenora");
    assert_eq!(
        guest.call("RegOpenKeyExA", &[HKCU, SCRATCH + 0x100, 0, 0, output]),
        1
    );
    assert_eq!(guest.error(), 2);

    // Every value is absent, even on the roots themselves.
    narrow(&mut guest, SCRATCH + 0x100, "MachineGuid");
    assert_eq!(
        guest.call("RegQueryValueExA", &[HKLM, SCRATCH + 0x100, 0, 0, 0, 0]),
        1
    );
    assert_eq!(guest.error(), 2);

    // A foreign handle is invalid everywhere.
    assert_eq!(guest.call("RegOpenKeyExA", &[0xdead, 0, 0, 0, output]), 1);
    assert_eq!(guest.error(), 6);

    // Opened keys close once; predefined roots close as a no-op.
    assert_eq!(guest.call("RegCloseKey", &[key]), 0);
    assert_eq!(guest.call("RegCloseKey", &[key]), 6);
    assert_eq!(guest.call("RegCloseKey", &[HKCU]), 0);
}
