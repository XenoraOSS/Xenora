// SPDX-License-Identifier: GPL-3.0-or-later
#include "journal.hpp"

extern "C" __declspec(dllimport) unsigned int ConsumerConstructed();
extern "C" __declspec(dllimport) __declspec(noreturn) void ExitProcess(Dword status);

extern "C" void entry() {
    if (ConsumerConstructed() != 2) {
        ExitProcess(1);
    }
    ExitProcess(append_event('M') ? 0 : 2);
}
