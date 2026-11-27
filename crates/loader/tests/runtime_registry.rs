// SPDX-License-Identifier: GPL-3.0-or-later

//! The synthetic empty registry through the guest import table.

mod support;

use support::*;

const ADVAPI32: &str = "advapi32.dll";
const REGISTRY_API: &str = "api-ms-win-core-registry-l1-1-0.dll";
const KERNEL32: &str = "kernel32.dll";

const HKCU: u64 = 0x8000_0001;
const WIN64_HKCU: u64 = 0xffff_ffff_8000_0001;
const KEY_READ: u64 = 0x0002_0019;
const NAME: u64 = SCRATCH + 0x100;
const UNWRITTEN: u64 = 0x1122_3344_5566_7788;

const ADVAPI_IMPORTS: [(&str, &str); 7] = [
    (ADVAPI32, "RegOpenKeyW"),
    (ADVAPI32, "RegOpenKeyExA"),
    (ADVAPI32, "RegOpenKeyExW"),
    (ADVAPI32, "RegQueryValueExA"),
    (ADVAPI32, "RegQueryValueExW"),
    (ADVAPI32, "RegCloseKey"),
    (KERNEL32, "GetLastError"),
];

const API_SET_IMPORTS: [(&str, &str); 6] = [
    (REGISTRY_API, "RegOpenKeyExA"),
    (REGISTRY_API, "RegOpenKeyExW"),
    (REGISTRY_API, "RegQueryValueExA"),
    (REGISTRY_API, "RegQueryValueExW"),
    (REGISTRY_API, "RegCloseKey"),
    (KERNEL32, "GetLastError"),
];

const CASES: [(&str, &[(&str, &str)]); 2] = [
    (ADVAPI32, &ADVAPI_IMPORTS),
    (REGISTRY_API, &API_SET_IMPORTS),
];

fn narrow(guest: &mut Guest, text: &str) {
    guest.memory.write_bytes(NAME, text.as_bytes()).unwrap();
    guest.memory.write_u8(NAME + text.len() as u64, 0).unwrap();
}

fn wide(guest: &mut Guest, text: &str) {
    for (index, unit) in text.encode_utf16().chain([0]).enumerate() {
        guest
            .memory
            .write_u16(NAME + index as u64 * 2, unit)
            .unwrap();
    }
}

#[test]
fn reg_open_key_w_uses_its_three_argument_contract() {
    let mut guest = Guest::new(&ADVAPI_IMPORTS, &[]);
    wide(&mut guest, "");
    for root in [HKCU, WIN64_HKCU] {
        for subkey in [0, NAME] {
            guest.memory.write_u64(SCRATCH, UNWRITTEN).unwrap();
            assert_eq!(guest.call("RegOpenKeyW", &[root, subkey, SCRATCH]), 0);
            assert_eq!(guest.u64_at(SCRATCH), root);
        }
    }

    wide(&mut guest, "Software\\Xenora");
    guest.memory.write_u64(SCRATCH, UNWRITTEN).unwrap();
    assert_eq!(guest.call("RegOpenKeyW", &[WIN64_HKCU, NAME, SCRATCH]), 2);
    assert_eq!(guest.u64_at(SCRATCH), UNWRITTEN);
    assert_eq!(guest.error(), 2);

    assert_eq!(guest.call("RegOpenKeyW", &[WIN64_HKCU, 0, 0]), 87);
    assert_eq!(guest.error(), 87);

    let foreign = 0x1234_5678_8000_0001;
    guest.memory.write_u64(SCRATCH, UNWRITTEN).unwrap();
    assert_eq!(guest.call("RegOpenKeyW", &[foreign, 0, SCRATCH]), 6);
    assert_eq!(guest.u64_at(SCRATCH), UNWRITTEN);
    assert_eq!(guest.error(), 6);
}

#[test]
fn opening_a_predefined_key_returns_the_same_handle() {
    for (module, imports) in CASES {
        let mut guest = Guest::new(imports, &[]);
        wide(&mut guest, "");
        for root in [HKCU, WIN64_HKCU] {
            for (symbol, subkey) in [
                ("RegOpenKeyExA", 0),
                ("RegOpenKeyExA", NAME),
                ("RegOpenKeyExW", 0),
                ("RegOpenKeyExW", NAME),
            ] {
                guest.memory.write_u64(SCRATCH, UNWRITTEN).unwrap();
                assert_eq!(guest.call(symbol, &[root, subkey, 0, KEY_READ, SCRATCH]), 0);
                assert_eq!(guest.u64_at(SCRATCH), root, "{module} {symbol}");
            }
            assert_eq!(guest.call("RegCloseKey", &[root]), 0);
            assert_eq!(guest.call("RegCloseKey", &[root]), 0);
        }
    }
}

#[test]
fn registry_failures_return_the_selected_windows_status() {
    for (module, imports) in CASES {
        let mut guest = Guest::new(imports, &[]);
        for (open, query, write_name) in [
            (
                "RegOpenKeyExA",
                "RegQueryValueExA",
                narrow as fn(&mut Guest, &str),
            ),
            ("RegOpenKeyExW", "RegQueryValueExW", wide),
        ] {
            write_name(&mut guest, "Software\\Xenora");
            guest.memory.write_u64(SCRATCH, UNWRITTEN).unwrap();
            assert_eq!(
                guest.call(open, &[WIN64_HKCU, NAME, 0, KEY_READ, SCRATCH]),
                2
            );
            assert_eq!(guest.u64_at(SCRATCH), UNWRITTEN, "{module} {open}");
            assert_eq!(guest.error(), 2);

            assert_eq!(guest.call(open, &[WIN64_HKCU, 0, 1, KEY_READ, SCRATCH]), 87);
            assert_eq!(guest.u64_at(SCRATCH), UNWRITTEN);
            assert_eq!(guest.call(open, &[WIN64_HKCU, 0, 0, KEY_READ, 0]), 87);
            assert_eq!(guest.error(), 87);

            write_name(&mut guest, "MissingValue");
            assert_eq!(guest.call(query, &[WIN64_HKCU, NAME, 0, 0, 0, 0]), 2);
            assert_eq!(guest.call(query, &[WIN64_HKCU, 0, 0, 0, 0, 0]), 2);

            // Arbitrary upper bits do not turn a foreign handle into a root.
            let foreign = 0x1234_5678_8000_0001;
            assert_eq!(guest.call(open, &[foreign, 0, 0, KEY_READ, SCRATCH]), 6);
            assert_eq!(guest.u64_at(SCRATCH), UNWRITTEN);
            assert_eq!(guest.call(query, &[foreign, 0, 0, 0, 0, 0]), 6);
            assert_eq!(guest.error(), 6);
            assert_eq!(guest.call("RegCloseKey", &[foreign]), 6);
        }
    }
}
