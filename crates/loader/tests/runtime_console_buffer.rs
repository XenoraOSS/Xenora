// SPDX-License-Identifier: GPL-3.0-or-later

//! Synthetic guest calls for console screen buffers. Expectations use
//! Microsoft's public contracts:
//! https://learn.microsoft.com/windows/console/createconsolescreenbuffer
//! https://learn.microsoft.com/windows/console/getconsolescreenbufferinfo
//!
//! The default 80x25 grid and the capture having no colors are Xenora facts,
//! not claims about every Windows console host.

mod support;

use support::{Guest, SCRATCH};

const KERNEL32: &str = "kernel32.dll";

const IMPORTS: &[(&str, &str)] = &[
    (KERNEL32, "GetLastError"),
    (KERNEL32, "CloseHandle"),
    (KERNEL32, "CreateConsoleScreenBuffer"),
    (KERNEL32, "GetConsoleScreenBufferInfo"),
    (KERNEL32, "SetConsoleCursorPosition"),
    (KERNEL32, "SetConsoleTextAttribute"),
    (KERNEL32, "SetConsoleScreenBufferSize"),
    (KERNEL32, "SetConsoleActiveScreenBuffer"),
    (KERNEL32, "GetStdHandle"),
    (KERNEL32, "FillConsoleOutputCharacterW"),
    (KERNEL32, "FillConsoleOutputCharacterA"),
    (KERNEL32, "FillConsoleOutputAttribute"),
    (KERNEL32, "WriteConsoleOutputW"),
    (KERNEL32, "ReadConsoleOutputW"),
];

const INFO: u64 = SCRATCH;
const ERROR_INVALID_HANDLE: u64 = 6;
const STD_OUTPUT_HANDLE: u64 = 0xFFFF_FFF5;
const GENERIC_RW: u64 = 0xC000_0000;

fn info_u16(guest: &Guest, offset: u64) -> u16 {
    guest.u16_at(INFO + offset)
}

#[test]
fn a_screen_buffer_reports_its_grid_cursor_and_attribute() {
    let mut guest = Guest::new(IMPORTS, &[]);
    let buffer = guest.call("CreateConsoleScreenBuffer", &[GENERIC_RW, 0, 0, 1, 0]);
    assert_ne!(buffer, 0);
    assert_ne!(buffer, u64::MAX);
    // The default grid is 80x25 with the cursor home and light gray text.
    assert_eq!(guest.call("GetConsoleScreenBufferInfo", &[buffer, INFO]), 1);
    assert_eq!(info_u16(&guest, 0), 80);
    assert_eq!(info_u16(&guest, 2), 25);
    assert_eq!(info_u16(&guest, 4), 0, "cursor x home");
    assert_eq!(info_u16(&guest, 6), 0, "cursor y home");
    assert_eq!(info_u16(&guest, 8), 7, "light gray on black");
    // The cursor moves inside the grid and not outside it.
    let position = 10 | (4 << 16);
    assert_eq!(
        guest.call("SetConsoleCursorPosition", &[buffer, position]),
        1
    );
    assert_eq!(guest.call("GetConsoleScreenBufferInfo", &[buffer, INFO]), 1);
    assert_eq!(info_u16(&guest, 4), 10);
    assert_eq!(info_u16(&guest, 6), 4);
    assert_eq!(guest.call("SetConsoleCursorPosition", &[buffer, 80]), 0);
    // The attribute round-trips.
    assert_eq!(guest.call("SetConsoleTextAttribute", &[buffer, 0x1F]), 1);
    assert_eq!(guest.call("GetConsoleScreenBufferInfo", &[buffer, INFO]), 1);
    assert_eq!(info_u16(&guest, 8), 0x1F);
    // Resizing keeps the content's grid coherent.
    let size = 120 | (40 << 16);
    assert_eq!(guest.call("SetConsoleScreenBufferSize", &[buffer, size]), 1);
    assert_eq!(guest.call("GetConsoleScreenBufferInfo", &[buffer, INFO]), 1);
    assert_eq!(info_u16(&guest, 0), 120);
    assert_eq!(info_u16(&guest, 2), 40);
    // Making it active is accepted, and closing it ends it.
    assert_eq!(guest.call("SetConsoleActiveScreenBuffer", &[buffer]), 1);
    assert_eq!(guest.call("CloseHandle", &[buffer]), 1);
    assert_eq!(guest.call("GetConsoleScreenBufferInfo", &[buffer, INFO]), 0);
    assert_eq!(
        guest.call("GetLastError", &[]) & 0xFFFF_FFFF,
        ERROR_INVALID_HANDLE
    );
}

#[test]
fn the_standard_output_reports_the_default_grid() {
    let mut guest = Guest::new(IMPORTS, &[]);
    let console = guest.call("GetStdHandle", &[STD_OUTPUT_HANDLE]);
    assert_eq!(
        guest.call("GetConsoleScreenBufferInfo", &[console, INFO]),
        1
    );
    assert_eq!(info_u16(&guest, 0), 80);
    assert_eq!(info_u16(&guest, 2), 25);
    // The capture has no colors: the attribute set is accepted and inert.
    assert_eq!(guest.call("SetConsoleTextAttribute", &[console, 0x4E]), 1);
}

#[test]
fn fills_write_their_count_and_stay_in_the_grid() {
    const WRITTEN: u64 = SCRATCH + 0x100;
    let mut guest = Guest::new(IMPORTS, &[]);
    let buffer = guest.call("CreateConsoleScreenBuffer", &[GENERIC_RW, 0, 0, 1, 0]);
    assert_ne!(buffer, u64::MAX);
    // Filling 10 cells from (0, 0) writes the count and succeeds.
    assert_eq!(
        guest.call(
            "FillConsoleOutputCharacterW",
            &[buffer, u64::from(b'x'), 10, 0, WRITTEN]
        ),
        1
    );
    assert_eq!(guest.u32_at(WRITTEN), 10);
    // A fill that runs past the end of the grid stops at the end:
    // from (70, 24) there are exactly 10 cells left in an 80x25 grid.
    let origin = 70 | (24 << 16);
    assert_eq!(
        guest.call(
            "FillConsoleOutputAttribute",
            &[buffer, 0x1F, 50, origin, WRITTEN]
        ),
        1
    );
    assert_eq!(guest.u32_at(WRITTEN), 10, "the fill stops at the grid end");
    // A start outside the grid is rejected.
    assert_eq!(
        guest.call(
            "FillConsoleOutputCharacterA",
            &[buffer, u64::from(b'y'), 5, 200, WRITTEN]
        ),
        0
    );
}

#[test]
fn console_output_round_trips_through_the_grid() {
    const CELLS: u64 = SCRATCH + 0x200;
    const REGION: u64 = SCRATCH + 0x600;
    let mut guest = Guest::new(IMPORTS, &[]);
    let buffer = guest.call("CreateConsoleScreenBuffer", &[GENERIC_RW, 0, 0, 1, 0]);
    assert_ne!(buffer, u64::MAX);
    // A 2x2 source of distinct cells: characters A B C D with attributes 1..4.
    for (index, (ch, attr)) in [(b'A', 1u16), (b'B', 2), (b'C', 3), (b'D', 4)]
        .iter()
        .enumerate()
    {
        guest
            .memory
            .write_u16(CELLS + index as u64 * 4, u64::from(*ch) as u16)
            .unwrap();
        guest
            .memory
            .write_u16(CELLS + index as u64 * 4 + 2, *attr)
            .unwrap();
    }
    // Write the 2x2 rectangle at grid position (5, 3): buffer size 2x2, buffer
    // coord (0,0), region left 5 top 3 right 6 bottom 4.
    for (offset, value) in [(0, 5u16), (2, 3), (4, 6), (6, 4)] {
        guest.memory.write_u16(REGION + offset, value).unwrap();
    }
    let buffer_size = 2 | (2 << 16);
    assert_eq!(
        guest.call(
            "WriteConsoleOutputW",
            &[buffer, CELLS, buffer_size, 0, REGION]
        ),
        1
    );
    // Read the same rectangle back into a zeroed target.
    for index in 0..4 {
        guest
            .memory
            .write_u16(CELLS + 0x100 + index as u64 * 4, 0)
            .unwrap();
        guest
            .memory
            .write_u16(CELLS + 0x100 + index as u64 * 4 + 2, 0)
            .unwrap();
    }
    for (offset, value) in [(0, 5u16), (2, 3), (4, 6), (6, 4)] {
        guest.memory.write_u16(REGION + offset, value).unwrap();
    }
    assert_eq!(
        guest.call(
            "ReadConsoleOutputW",
            &[buffer, CELLS + 0x100, buffer_size, 0, REGION]
        ),
        1
    );
    assert_eq!(
        guest.memory.read_u16(CELLS + 0x100).unwrap(),
        u64::from(b'A') as u16
    );
    assert_eq!(guest.memory.read_u16(CELLS + 0x102).unwrap(), 1);
    assert_eq!(
        guest.memory.read_u16(CELLS + 0x104).unwrap(),
        u64::from(b'B') as u16
    );
    assert_eq!(
        guest.memory.read_u16(CELLS + 0x108).unwrap(),
        u64::from(b'C') as u16
    );
    assert_eq!(
        guest.memory.read_u16(CELLS + 0x10C).unwrap(),
        u64::from(b'D') as u16
    );
    assert_eq!(guest.memory.read_u16(CELLS + 0x10E).unwrap(), 4);
}
