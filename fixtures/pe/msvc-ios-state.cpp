// SPDX-License-Identifier: GPL-3.0-or-later
// Original guest checking state accessors against Microsoft x64 field offsets.
using u32 = unsigned int;
using u64 = unsigned long long;
extern "C" {
__declspec(dllimport) int state(const void *) __asm("?rdstate@ios_base@std@@QEBAHXZ");
__declspec(dllimport) bool good(const void *) __asm("?good@ios_base@std@@QEBA_NXZ");
__declspec(dllimport) bool eof(const void *) __asm("?eof@ios_base@std@@QEBA_NXZ");
__declspec(dllimport) bool fail(const void *) __asm("?fail@ios_base@std@@QEBA_NXZ");
__declspec(dllimport) bool bad(const void *) __asm("?bad@ios_base@std@@QEBA_NXZ");
__declspec(dllimport) __declspec(noreturn) void ExitProcess(u32);
}
struct IosState { u64 vtable; u64 standard; u32 state; };
extern "C" void entry() {
#ifdef XENORA_FAULT
    if (good(reinterpret_cast<const void *>(~u64(0)))) ExitProcess(8);
    ExitProcess(9);
#else
    IosState obj{};
    if (!good(&obj) || eof(&obj) || fail(&obj) || bad(&obj) || state(&obj)) ExitProcess(1);
    obj.state = 1;
    if (good(&obj) || !eof(&obj) || fail(&obj) || bad(&obj) || state(&obj) != 1) ExitProcess(2);
    obj.state = 2;
    if (good(&obj) || eof(&obj) || !fail(&obj) || bad(&obj)) ExitProcess(3);
    obj.state = 4;
    if (good(&obj) || eof(&obj) || !fail(&obj) || !bad(&obj)) ExitProcess(4);
    ExitProcess(0);
#endif
}
