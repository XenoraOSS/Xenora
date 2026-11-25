// SPDX-License-Identifier: GPL-3.0-or-later

//! C++ `type_info` helpers through the guest import table. Each type
//! descriptor's data is an 8-byte cached-name slot followed by the decorated
//! name, per the publicly described MSVC RTTI type descriptor layout. The
//! decorated names here are written by hand for these tests.

mod support;

use support::*;
use xenora_loader::cpu::ExitReason;

const IMPORTS: &[(&str, &str)] = &[
    ("vcruntime140.dll", "__std_type_info_compare"),
    ("vcruntime140.dll", "__std_type_info_hash"),
    ("vcruntime140.dll", "__std_type_info_name"),
    ("vcruntime140.dll", "__std_type_info_destroy_list"),
    ("vcruntime140.dll", "__std_exception_copy"),
    ("vcruntime140.dll", "__std_exception_destroy"),
    ("ucrtbase.dll", "malloc"),
    ("ucrtbase.dll", "free"),
];

const WIDGET: u64 = SCRATCH;
const WIDGET_COPY: u64 = SCRATCH + 0x40;
const POINT: u64 = SCRATCH + 0x80;
const COLOR: u64 = SCRATCH + 0xC0;
const TEMPLATE: u64 = SCRATCH + 0x100;
const LIST: u64 = SCRATCH + 0x200;
const ORIGINAL: u64 = SCRATCH + 0x300;
const COPY: u64 = SCRATCH + 0x320;
const MESSAGE: u64 = SCRATCH + 0x340;

fn descriptor(guest: &mut Guest, at: u64, name: &str) {
    guest.memory.write_u64(at, 0).unwrap();
    let mut bytes = name.as_bytes().to_vec();
    bytes.push(0);
    guest.memory.write_bytes(at + 8, &bytes).unwrap();
}

fn text(guest: &Guest, mut at: u64) -> String {
    let mut bytes = Vec::new();
    loop {
        let byte = guest.memory.read_u8(at).unwrap();
        if byte == 0 {
            return String::from_utf8(bytes).unwrap();
        }
        bytes.push(byte);
        at += 1;
    }
}

fn guest() -> Guest {
    let mut guest = Guest::new(IMPORTS, &[]);
    descriptor(&mut guest, WIDGET, ".?AVWidget@ui@@");
    // A second descriptor for the same type, as another module would carry.
    descriptor(&mut guest, WIDGET_COPY, ".?AVWidget@ui@@");
    descriptor(&mut guest, POINT, ".?AUPoint@@");
    descriptor(&mut guest, COLOR, ".?AW4Color@@");
    descriptor(&mut guest, TEMPLATE, ".?AV?$Box@H@@");
    guest
}

fn compare(guest: &mut Guest, left: u64, right: u64) -> i32 {
    guest.call("__std_type_info_compare", &[left, right]) as u32 as i32
}

#[test]
fn equal_types_compare_equal_across_descriptors_and_order_consistently() {
    let mut guest = guest();
    assert_eq!(compare(&mut guest, WIDGET, WIDGET), 0);
    assert_eq!(compare(&mut guest, WIDGET, WIDGET_COPY), 0);
    let forward = compare(&mut guest, WIDGET, POINT);
    let backward = compare(&mut guest, POINT, WIDGET);
    assert_ne!(forward, 0);
    assert_eq!(forward, -backward, "before() is antisymmetric");
    // 'U' (0x55) sorts before 'V' (0x56) in the decorated names.
    assert_eq!(backward, -1);
}

#[test]
fn equal_types_hash_equally() {
    let mut guest = guest();
    let widget = guest.call("__std_type_info_hash", &[WIDGET]);
    assert_eq!(guest.call("__std_type_info_hash", &[WIDGET_COPY]), widget);
    assert_ne!(guest.call("__std_type_info_hash", &[POINT]), widget);
}

#[test]
fn name_undecorates_caches_and_is_released_by_destroy_list() {
    let mut guest = guest();
    let name = guest.call("__std_type_info_name", &[WIDGET, LIST]);
    assert_ne!(name, 0);
    assert_eq!(text(&guest, name), "class ui::Widget");
    assert_eq!(guest.u64_at(WIDGET), name, "cached in the descriptor");
    assert_eq!(guest.call("__std_type_info_name", &[WIDGET, LIST]), name);
    let point = guest.call("__std_type_info_name", &[POINT, LIST]);
    assert_eq!(text(&guest, point), "struct Point");
    let color = guest.call("__std_type_info_name", &[COLOR, LIST]);
    assert_eq!(text(&guest, color), "enum Color");

    guest.call("__std_type_info_destroy_list", &[LIST]);
    assert!(
        guest.memory.read_u8(name).is_err(),
        "destroy_list frees the names it allocated"
    );
}

#[test]
fn unmodelled_names_and_bad_pointers_stop_the_guest() {
    let mut guest = guest();
    assert_eq!(
        guest.fails("__std_type_info_name", &[TEMPLATE, LIST]),
        RUNTIME_LIMIT
    );
    assert_eq!(guest.u64_at(TEMPLATE), 0, "nothing cached");
    assert!(matches!(
        guest.fails("__std_type_info_compare", &[WIDGET, 0x10]),
        ExitReason::MemoryFault(_)
    ));
    assert!(matches!(
        guest.fails("__std_type_info_hash", &[0]),
        ExitReason::MemoryFault(_)
    ));
}

fn record(guest: &mut Guest, at: u64, text: u64, owned: bool) {
    guest.memory.write_u64(at, text).unwrap();
    guest.memory.write_u8(at + 8, u8::from(owned)).unwrap();
}

fn owned_flag(guest: &Guest, at: u64) -> u8 {
    guest.memory.read_u8(at + 8).unwrap()
}

#[test]
fn exception_copies_duplicate_owned_messages_and_share_borrowed_ones() {
    let mut guest = guest();
    // A borrowed literal is shared, never freed.
    guest.memory.write_bytes(MESSAGE, b"literal\0").unwrap();
    record(&mut guest, ORIGINAL, MESSAGE, false);
    record(&mut guest, COPY, 0xDEAD, true);
    guest.call("__std_exception_copy", &[ORIGINAL, COPY]);
    assert_eq!(guest.u64_at(COPY), MESSAGE);
    assert_eq!(owned_flag(&guest, COPY), 0);
    guest.call("__std_exception_destroy", &[COPY]);
    assert_eq!(text(&guest, MESSAGE), "literal");
    assert_eq!((guest.u64_at(COPY), owned_flag(&guest, COPY)), (0, 0));

    // An owned message from guest malloc gets its own copy, so destroying
    // the original leaves the copy's what() intact.
    let owned = guest.call("malloc", &[16]);
    guest.memory.write_bytes(owned, b"bad_alloc\0").unwrap();
    record(&mut guest, ORIGINAL, owned, true);
    guest.call("__std_exception_copy", &[ORIGINAL, COPY]);
    let duplicate = guest.u64_at(COPY);
    assert_ne!(duplicate, owned);
    assert_eq!(owned_flag(&guest, COPY), 1);
    guest.call("__std_exception_destroy", &[ORIGINAL]);
    assert!(guest.memory.read_u8(owned).is_err(), "original text freed");
    assert_eq!(text(&guest, duplicate), "bad_alloc");
    // The duplicate came from the CRT heap, so guest free releases it.
    guest.call("free", &[duplicate]);
    assert!(guest.memory.read_u8(duplicate).is_err());
}

#[test]
fn exception_records_with_no_message_or_bad_pointers() {
    let mut guest = guest();
    record(&mut guest, ORIGINAL, 0, true);
    record(&mut guest, COPY, 0xDEAD, true);
    guest.call("__std_exception_copy", &[ORIGINAL, COPY]);
    assert_eq!((guest.u64_at(COPY), owned_flag(&guest, COPY)), (0, 0));
    guest.call("__std_exception_destroy", &[COPY]);

    // An owned pointer the heap never allocated is a guest bug, not a no-op.
    record(&mut guest, ORIGINAL, MESSAGE, true);
    assert!(matches!(
        guest.fails("__std_exception_destroy", &[ORIGINAL]),
        ExitReason::MemoryFault(_)
    ));
    assert!(matches!(
        guest.fails("__std_exception_copy", &[0x10, COPY]),
        ExitReason::MemoryFault(_)
    ));
}
