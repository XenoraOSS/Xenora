// SPDX-License-Identifier: GPL-3.0-or-later
// Original guest exercising atomic-wait time arithmetic and the guest clock.
using u32 = unsigned int;
using u64 = unsigned long long;
extern "C" {
__declspec(dllimport) u64 __std_atomic_wait_get_deadline(u64);
__declspec(dllimport) u32 __std_atomic_wait_get_remaining_timeout(u64);
__declspec(dllimport) void Sleep(u32);
__declspec(dllimport) __declspec(noreturn) void ExitProcess(u32);
}
extern "C" void entry() {
    const u64 forever = ~u64(0);
    if (__std_atomic_wait_get_deadline(forever) != forever) ExitProcess(1);
    if (__std_atomic_wait_get_remaining_timeout(forever) != ~u32(0)) ExitProcess(2);
    const auto deadline = __std_atomic_wait_get_deadline(100);
    if (__std_atomic_wait_get_remaining_timeout(deadline) != 100) ExitProcess(3);
    Sleep(25);
    if (__std_atomic_wait_get_remaining_timeout(deadline) != 75) ExitProcess(4);
    Sleep(75);
    if (__std_atomic_wait_get_remaining_timeout(deadline) != 0) ExitProcess(5);
    if (__std_atomic_wait_get_remaining_timeout(__std_atomic_wait_get_deadline(864000001)) != 864000000) ExitProcess(6);
    ExitProcess(0);
}
