// SPDX-License-Identifier: GPL-3.0-or-later
// Original MSVC-ABI guest checks facet reference counts and narrow widening.
using u32 = unsigned int;
using u64 = unsigned long long;
extern "C" {
__declspec(dllimport) void incref(void *) __asm("?_Incref@facet@locale@std@@UEAAXXZ");
__declspec(dllimport) void *decref(void *) __asm("?_Decref@facet@locale@std@@UEAAPEAV_Facet_base@3@XZ");
__declspec(dllimport) char widen(void *, char) __asm("?do_widen@?$ctype@D@std@@MEBADD@Z");
__declspec(dllimport) __declspec(noreturn) void ExitProcess(u32);
}
struct Facet { u64 vtable; u32 references; u32 padding; };
extern "C" void entry() {
    Facet f{};
    incref(&f); incref(&f);
    if (f.references != 2 || decref(&f) != nullptr || f.references != 1) ExitProcess(1);
    if (decref(&f) != &f || f.references != 0) ExitProcess(2);
    if (widen(&f, ' ') != ' ' || static_cast<unsigned char>(widen(&f, static_cast<char>(0x80))) != 0x80) ExitProcess(3);
    ExitProcess(0);
}
