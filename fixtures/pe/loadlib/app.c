// SPDX-License-Identifier: GPL-3.0-or-later
//
// Original Xenora fixture. A no-CRT EXE that loads an authorized DLL after
// process entry, resolves its function and data exports by name, calls the
// function indirectly, checks initializer evidence and frees the module. The
// exit status is an exact assertion: 70 means every step succeeded, while
// distinct small statuses identify the first failed step.

typedef unsigned int u32;

__declspec(dllimport) void *LoadLibraryA(const char *name);
__declspec(dllimport) void *GetProcAddress(void *module, const char *name);
__declspec(dllimport) int FreeLibrary(void *module);
__declspec(dllimport) void ExitProcess(u32 status);

typedef u32 (*Triple)(u32);

void entry(void) {
    void *module = LoadLibraryA("xenoralib.dll");
    if (!module) {
        ExitProcess(1);
    }

    Triple triple = (Triple)GetProcAddress(module, "XenoraTriple");
    u32 *init = (u32 *)GetProcAddress(module, "XenoraLibInit");
    u32 *tls = (u32 *)GetProcAddress(module, "XenoraLibTls");
    if (!triple || !init || !tls) {
        ExitProcess(2);
    }

    u32 value = triple(14);
    if (value != 42u) {
        ExitProcess(3);
    }
    if (*init != 0xA5A5u) {
        ExitProcess(4);
    }
    if (*tls != 7u) {
        ExitProcess(5);
    }
    if (!FreeLibrary(module)) {
        ExitProcess(6);
    }

    ExitProcess(70u);
}
