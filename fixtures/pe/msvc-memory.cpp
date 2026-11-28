// SPDX-License-Identifier: GPL-3.0-or-later
// Original Windows x64 guest for VCRUNTIME's memory exports.
using size = unsigned long long;
extern "C" {
__declspec(dllimport) void *memcpy(void *, const void *, size);
__declspec(dllimport) void *memmove(void *, const void *, size);
__declspec(dllimport) void *memset(void *, int, size);
__declspec(dllimport) int memcmp(const void *, const void *, size);
__declspec(dllimport) void *memchr(const void *, int, size);
__declspec(dllimport) __declspec(noreturn) void ExitProcess(unsigned);
}
extern "C" void entry() {
#ifdef XENORA_FAULT
    memchr(reinterpret_cast<const void *>(0x1234), 0, 1);
    ExitProcess(8);
#else
    char value[8];
    if (memset(value, 'a', 8) != value) ExitProcess(1);
    const char source[] = "abcde";
    if (memcpy(value, source, 5) != value) ExitProcess(2);
    if (memmove(value + 1, value, 5) != value + 1) ExitProcess(3);
    const char expected[] = "aabcdeaa";
    if (memcmp(value, expected, 8) != 0) ExitProcess(4);
    if (memchr(value, 'c', 8) != value + 3) ExitProcess(5);
    if (memchr(value, 'z', 8) != nullptr) ExitProcess(6);
    if (memcmp(value, expected, 0) != 0) ExitProcess(7);
    ExitProcess(0);
#endif
}
