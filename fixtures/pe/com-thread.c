// SPDX-License-Identifier: GPL-3.0-or-later
// The scheduler reuses its first worker slot: the second thread must not
// inherit the first thread's unbalanced COM initialization.
typedef unsigned int u32;
typedef unsigned long long u64;
typedef int i32;
__declspec(dllimport) __declspec(noreturn) void ExitProcess(u32);
__declspec(dllimport) u64 CreateThread(void*, u64, u32 (*)(void*), void*, u32, u32*);
__declspec(dllimport) u32 WaitForSingleObject(u64, u32);
__declspec(dllimport) u32 GetExitCodeThread(u64, u32*);
__declspec(dllimport) u32 CloseHandle(u64);
__declspec(dllimport) i32 CoInitializeEx(void*, u32);
static u64 first_teb;
static u32 worker(void *model) {
    u64 teb;
    __asm__("movq %%gs:0x30, %0" : "=r"(teb));
    if ((u64)model == 2) first_teb = teb;
    else if (!first_teb || first_teb != teb) return 0xfeed;
    return (u32)CoInitializeEx(0, (u32)(u64)model);
}
void entry(void) {
    for (u64 model = 2; ; model = 0) {
        u32 result = ~0u;
        u64 thread = CreateThread(0, 0, worker, (void*)model, 0, 0);
        if (!thread) ExitProcess(1);
        if (WaitForSingleObject(thread, ~0u) != 0) ExitProcess(2);
        if (!GetExitCodeThread(thread, &result) || result != 0) ExitProcess(3);
        if (!CloseHandle(thread)) ExitProcess(4);
        if (!model) break;
    }
    ExitProcess(0);
}
