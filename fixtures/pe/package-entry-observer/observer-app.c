// SPDX-License-Identifier: GPL-3.0-or-later

typedef unsigned int u32;

__declspec(dllimport) u32 ProviderValue(void);
__declspec(dllimport) void PhaseProbe(u32 marker);
__declspec(dllimport) __declspec(noreturn) void ExitProcess(u32 status);

void entry(void) {
    if (ProviderValue() != 7)
        ExitProcess(11);
    PhaseProbe(2);
    ExitProcess(0);
}
