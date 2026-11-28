// SPDX-License-Identifier: GPL-3.0-or-later

// Original AMD64 guest. Each failed check exits with its source line.
typedef unsigned int u32;
typedef unsigned long long u64;
typedef long long i64;
typedef void *handle;

#define IMPORT __declspec(dllimport)
IMPORT int rand(void);
IMPORT void srand(u32);
IMPORT int legacy_rand(void);
IMPORT void legacy_srand(u32);
IMPORT int ucrt_rand(void);
IMPORT void ucrt_srand(u32);
IMPORT i64 _time64(i64 *);
IMPORT i64 ucrt_time64(i64 *);
IMPORT handle CreateThread(void *, u64, u32 (*)(void *), void *, u32, u32 *);
IMPORT u32 WaitForSingleObject(handle, u32);
IMPORT int CloseHandle(handle);
IMPORT int _write(int, const void *, u32);
IMPORT __declspec(noreturn) void ExitProcess(u32);

#define CHECK(condition) do { if (!(condition)) ExitProcess(__LINE__); } while (0)

#ifdef RNG_FIXTURE
static volatile u32 completed_workers;

static u32 check_worker(void *parameter) {
    (void)parameter;
    CHECK(rand() == 41);
    CHECK(legacy_rand() == 41);
    srand(7);
    CHECK(ucrt_rand() == 61);
    CHECK(legacy_rand() == 18467);
    legacy_srand(99);
    CHECK(legacy_rand() == 361);
    CHECK(rand() == 17422);
    ++completed_workers;
    return 0;
}

void rng_entry(void) {
    CHECK(rand() == 41);
    CHECK(legacy_rand() == 41);
    CHECK(ucrt_rand() == 18467);
    CHECK(legacy_rand() == 18467);

    ucrt_srand(42);
    CHECK(rand() == 175);
    CHECK(legacy_rand() == 6334);
    legacy_srand(99);
    CHECK(legacy_rand() == 361);
    CHECK(rand() == 400);

    srand(42);
    CHECK(ucrt_rand() == 175 && rand() == 400);
    for (u32 i = 1; i <= 2; ++i) {
        handle worker = CreateThread(0, 0, check_worker, 0, 0, 0);
        CHECK(worker && WaitForSingleObject(worker, 0xffffffffu) == 0);
        CHECK(CloseHandle(worker) && completed_workers == i);
    }
    CHECK(rand() == 17869);
    CHECK(legacy_rand() == 23235);
    ExitProcess(0);
}

#else
void time_entry(void) {
    struct { u64 before; i64 result; u64 after; } output = {
        0x0123456789abcdefull, 42, 0xfedcba9876543210ull,
    };
    i64 timestamp = _time64(&output.result);
    CHECK(output.result == timestamp);
    CHECK(output.before == 0x0123456789abcdefull && output.after == 0xfedcba9876543210ull);
    CHECK(_time64(0) == timestamp);
    CHECK(ucrt_time64(0) == timestamp);
    CHECK(_write(1, &timestamp, sizeof(timestamp)) == sizeof(timestamp));
    ExitProcess(0);
}
#endif
