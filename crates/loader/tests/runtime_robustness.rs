// SPDX-License-Identifier: GPL-3.0-or-later

//! Stable-toolchain replay of the `runtime_services` fuzz driver: seeded
//! pseudo-random call sequences against the standard service registry.

mod support;

use support::robustness::{ARG_CODE, ARG_REUSE_RESULT, ARG_ZERO, Driver};

/// xorshift64*, so every run uses the same inputs.
fn inputs(seed: u64, count: usize, length: usize) -> impl Iterator<Item = Vec<u8>> {
    let mut state = seed;
    (0..count).map(move |_| {
        (0..length)
            .map(|_| {
                state ^= state >> 12;
                state ^= state << 25;
                state ^= state >> 27;
                (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8
            })
            .collect()
    })
}

#[test]
fn random_service_sequences_keep_memory_and_register_invariants() {
    let driver = Driver::new();
    assert!(driver.service_count() > 150, "{}", driver.service_count());
    let mut total_calls = 0usize;
    let mut max_distinct = 0usize;
    for input in inputs(0x5EED_CAFE, 300, 1024) {
        let stats = driver.run(&input);
        total_calls += stats.calls;
        max_distinct = max_distinct.max(stats.distinct_services);
    }
    // With memory init no longer consuming the corpus, 1 KiB inputs must
    // reach the dispatch loop (each call needs 24 bytes plus a GS byte).
    assert!(
        total_calls >= 300 * 20,
        "expected handlers to run; total_calls={total_calls}"
    );
    assert!(
        max_distinct >= 8,
        "expected multi-service coverage; max_distinct={max_distinct}"
    );
    assert_eq!(driver.run(&[]).calls, 0);
}

#[test]
fn encoded_sequences_invoke_handlers_and_reuse_handles() {
    let driver = Driver::new();
    let create = driver
        .service_index("kernel32.dll", "CreateEventW")
        .expect("CreateEventW");
    let close = driver
        .service_index("kernel32.dll", "CloseHandle")
        .expect("CloseHandle");
    let get_tick = driver
        .service_index("kernel32.dll", "GetTickCount64")
        .expect("GetTickCount64");
    let query = driver
        .service_index("kernel32.dll", "QueryPerformanceCounter")
        .expect("QueryPerformanceCounter");

    // CreateEventW(null, 0, 0, null) → CloseHandle(prior result) → GetTickCount64
    // → QueryPerformanceCounter(writable via mode 2 is not used; zeros ok).
    let zeros = [ARG_ZERO; 7];
    let mut close_args = [ARG_ZERO; 7];
    close_args[0] = ARG_REUSE_RESULT; // hObject from CreateEventW
    let input = driver.encode_sequence(&[
        (create, zeros),
        (close, close_args),
        (get_tick, zeros),
        (query, zeros),
    ]);
    let stats = driver.run(&input);
    assert_eq!(stats.calls, 4);
    assert_eq!(stats.distinct_services, 4);
}

#[test]
fn debug_break_may_enter_an_installed_exception_filter() {
    let driver = Driver::new();
    let set_filter = driver
        .service_index("kernel32.dll", "SetUnhandledExceptionFilter")
        .expect("SetUnhandledExceptionFilter");
    let debug_break = driver
        .service_index("kernel32.dll", "DebugBreak")
        .expect("DebugBreak");
    let mut filter = [ARG_ZERO; 7];
    filter[0] = ARG_CODE;
    let input = driver.encode_sequence(&[(set_filter, filter), (debug_break, [ARG_ZERO; 7])]);

    // The raw DebugBreak handler enters the guest filter by installing its
    // instruction pointer and stack. Both calls still run in the driver.
    let stats = driver.run(&input);
    assert_eq!(stats.calls, 2);
    assert_eq!(stats.distinct_services, 2);
}

#[test]
fn xmem_alloc_oom_artifact_fails_closed() {
    // libFuzzer OOM from a ~3.7 GiB XMemAlloc before the virtual budget gated
    // the mapping (fuzz/artifacts/runtime_services/oom-022d4084...).
    let driver = Driver::new();
    let stats = driver.run(&[242, 2, 24, 0, 248, 8, 9, 9, 8, 0, 0, 129, 191]);
    assert!(stats.calls >= 1, "{stats:?}");
}
