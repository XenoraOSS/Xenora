// SPDX-License-Identifier: GPL-3.0-or-later
// Original Windows x64 guest checks its processor and active processor count.
using u32 = unsigned int;
extern "C" {
__declspec(dllimport) u32 GetCurrentProcessorNumber();
__declspec(dllimport) u32 GetActiveProcessorCount(unsigned short);
__declspec(dllimport) __declspec(noreturn) void ExitProcess(u32);
}
extern "C" void entry() {
    const auto processor = GetCurrentProcessorNumber();
    if (processor != 0 || processor >= GetActiveProcessorCount(0)) ExitProcess(1);
    ExitProcess(0);
}
