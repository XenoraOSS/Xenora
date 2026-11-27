// SPDX-License-Identifier: GPL-3.0-or-later
//
// Original no-CRT C++ fixture for the Itanium C++ ABI guard-variable services
// (issue #934). The program is compiled with thread-safe function-local
// statics, so the compiler emits calls to __cxa_guard_acquire and
// __cxa_guard_release. It also calls the three guard services directly with
// its own aligned guard variables.
//
// Every outside function is imported by name from libstdc++-6.dll or
// kernel32.dll. The program is built with
// tests/compat_fixtures/build-cxx-guard.sh and reports through a .gate section.
//
// Normal exit status:
//   0   every check passed
//   1..8 the numbered check failed
// Control exit status:
//   42  the reentrant control observed -1 from its second acquire
//   43  the reentrant control did not observe -1
//   44  the first acquire of the control guard did not return 1
//   90  an invalid release reached the process exit after it (it must not)
//   91  an invalid abort reached the process exit after it (it must not)

typedef unsigned int u32;
typedef unsigned long long u64;

extern "C" __declspec(dllimport) int __cxa_guard_acquire(u64 *guard);
extern "C" __declspec(dllimport) void __cxa_guard_release(u64 *guard);
extern "C" __declspec(dllimport) void __cxa_guard_abort(u64 *guard);
extern "C" __declspec(dllimport) void ExitProcess(u32 status);

#define MAGIC 0x63786764u // "cgxd"

struct ResultBlock {
    volatile u32 magic;
    volatile u32 lazy_first;
    volatile u32 lazy_second;
    volatile u32 init_count;
    volatile u32 direct_first;
    volatile u32 direct_second;
    volatile u32 abort_first;
    volatile u32 abort_after;
    volatile u32 unmapped;
    volatile u32 unaligned;
    volatile u32 reentrant;
    volatile u32 reserved;
    volatile u64 direct_guard;
    volatile u64 abort_guard;
};

#pragma section(".gate", read, write)
__declspec(allocate(".gate")) ResultBlock result = {
    MAGIC, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};

// No C runtime: these live in the image's zero-filled data.
static volatile u32 initialization_count;
alignas(8) u64 direct_guard;
alignas(8) u64 abort_guard;
#ifdef REENTRANT_CONTROL
alignas(8) u64 reentrant_guard;
#endif

// Noinline so the compiler cannot replace the dynamic initialization with the
// literal at optimization levels O1, O2 and Os.
__declspec(noinline) static u32 make_lazy_value() {
    initialization_count = initialization_count + 1;
    return 41;
}

__declspec(noinline) static u32 lazy_value() {
    static u32 value = make_lazy_value();
    return value;
}

extern "C" void guest_cxx_guard_entry() {
    const u32 first = lazy_value();
    const u32 second = lazy_value();
    result.lazy_first = first;
    result.lazy_second = second;
    result.init_count = initialization_count;
    if (initialization_count != 1 || first != 41 || second != 41)
        ExitProcess(1);

    // A fresh guard follows the public ABI: acquire writes nothing, release
    // sets the first byte, and a later acquire reports completion.
    const int acquired = __cxa_guard_acquire(&direct_guard);
    result.direct_first = static_cast<u32>(acquired);
    result.direct_guard = direct_guard;
    if (acquired != 1)
        ExitProcess(2);
    if ((direct_guard & 0xffu) != 0)
        ExitProcess(3);
    __cxa_guard_release(&direct_guard);
    const int acquired_again = __cxa_guard_acquire(&direct_guard);
    result.direct_second = static_cast<u32>(acquired_again);
    result.direct_guard = direct_guard;
    if (acquired_again != 0 || (direct_guard & 0xffu) == 0)
        ExitProcess(4);

    // Abort leaves the byte clear and the guard available again.
    const int aborted = __cxa_guard_acquire(&abort_guard);
    result.abort_first = static_cast<u32>(aborted);
    if (aborted != 1)
        ExitProcess(5);
    __cxa_guard_abort(&abort_guard);
    if ((abort_guard & 0xffu) != 0)
        ExitProcess(6);
    const int after_abort = __cxa_guard_acquire(&abort_guard);
    result.abort_after = static_cast<u32>(after_abort);
    if (after_abort != 1)
        ExitProcess(7);
    __cxa_guard_release(&abort_guard);
    result.abort_guard = abort_guard;

    // Invalid addresses fail with -1 instead of faulting or initializing.
    const int unmapped = __cxa_guard_acquire(reinterpret_cast<u64 *>(0x7000'0000u));
    result.unmapped = static_cast<u32>(unmapped);
    if (unmapped != -1)
        ExitProcess(8);
    const int unaligned = __cxa_guard_acquire(
        reinterpret_cast<u64 *>(reinterpret_cast<unsigned char *>(&direct_guard) + 1));
    result.unaligned = static_cast<u32>(unaligned);
    if (unaligned != -1)
        ExitProcess(8);

#ifdef REENTRANT_CONTROL
    if (__cxa_guard_acquire(&reentrant_guard) != 1)
        ExitProcess(44);
    const int second_acquire = __cxa_guard_acquire(&reentrant_guard);
    result.reentrant = static_cast<u32>(second_acquire);
    if (second_acquire != -1)
        ExitProcess(43);
    __cxa_guard_release(&reentrant_guard);
    ExitProcess(42);
#endif

#ifdef INVALID_RELEASE_CONTROL
    __cxa_guard_release(&abort_guard);
    ExitProcess(90);
#endif

#ifdef INVALID_ABORT_CONTROL
    __cxa_guard_abort(&abort_guard);
    ExitProcess(91);
#endif

    ExitProcess(0);
}
