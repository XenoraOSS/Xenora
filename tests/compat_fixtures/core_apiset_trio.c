// SPDX-License-Identifier: GPL-3.0-or-later
// Each probe links its tested symbol through Kernel32 or its exact API set.

typedef unsigned long long uintptr;
__declspec(dllimport) __declspec(noreturn) void ExitProcess(unsigned int);

#if defined(PROBE_FORMAT_MESSAGE)
__declspec(dllimport) unsigned int FormatMessageA(unsigned int, const void *,
    unsigned int, unsigned int, char *, unsigned int, void *);
__declspec(dllimport) void *LocalFree(void *);

void entry(void) {
    static const char expected[] = "The system cannot find the file specified.\r\n";
    char text[sizeof(expected)];
    unsigned int count = FormatMessageA(0x1000, 0, 2, 0, text, sizeof(text), 0);
    if (count != sizeof(expected) - 1)
        ExitProcess(1);
    for (unsigned int i = 0; i < sizeof(expected); i++)
        if (text[i] != expected[i])
            ExitProcess(2);
    char *allocated = 0;
    count = FormatMessageA(0x1100, 0, 2, 0, (char *)&allocated, 0, 0);
    if (!allocated || count != sizeof(expected) - 1)
        ExitProcess(3);
    for (unsigned int i = 0; i < sizeof(expected); i++)
        if (allocated[i] != expected[i])
            ExitProcess(4);
    if (LocalFree(allocated))
        ExitProcess(5);
    ExitProcess(0);
}
#elif defined(PROBE_LOCAL_FREE)
__declspec(dllimport) void *LocalAlloc(unsigned int, uintptr);
__declspec(dllimport) void *LocalFree(void *);

void entry(void) {
    volatile unsigned char *memory = LocalAlloc(0x40, 16);
    if (!memory || memory[15] != 0)
        ExitProcess(1);
    memory[15] = 0x5a;
    if (memory[15] != 0x5a || LocalFree(0))
        ExitProcess(2);
    ExitProcess(LocalFree((void *)memory) != 0);
}
#elif defined(PROBE_STACK_LIMITS)
__declspec(dllimport) void GetCurrentThreadStackLimits(uintptr *, uintptr *);

void entry(void) {
    uintptr low = 0, high = 0, stack;
    __asm__ volatile("movq %%rsp, %0" : "=r"(stack));
    GetCurrentThreadStackLimits(&low, &high);
    ExitProcess(low && low <= stack && stack < high && high - low >= 4096 ? 0 : 1);
}
#else
#error Select one probe.
#endif
