/* SPDX-License-Identifier: GPL-3.0-or-later */
#include "fixture.h"

__declspec(dllimport) handle LoadLibraryA(const char *);
__declspec(dllimport) handle GetModuleHandleA(const char *);
__declspec(dllimport) dword GetLastError(void);
__declspec(dllimport) void SetLastError(dword);

void entry(void) {
    static const char module[] = "xenora_runtime_boundary.dll";
    // Seed separately so a stale last error cannot satisfy either check.
    SetLastError(0);
    if (LoadLibraryA(module) != 0 || GetLastError() != 126)
        ExitProcess(10);
    SetLastError(0);
    if (GetModuleHandleA(module) != 0 || GetLastError() != 126)
        ExitProcess(11);
    ExitProcess(MARKER("consumer: provisioned DLL unavailable\n") ? 0 : 12);
}
