// SPDX-License-Identifier: GPL-3.0-or-later
#include "payload.h"
extern "C" __declspec(dllimport) unsigned ThrowFromDll(unsigned *, bool);
struct CatchGuard {
    unsigned *trace;
    ~CatchGuard() { *trace = *trace * 10 + 3; }
};
__attribute__((noinline)) unsigned try_dll(unsigned *trace, bool raise) {
    try {
        CatchGuard guard{trace};
        return ThrowFromDll(trace, raise);
    } catch (const Payload &caught) {
        *trace = *trace * 10 + 4;
        return caught.value;
    }
}
int main() {
    // Entry into the DLL (1), DLL guard (2), then EXE guard (3).
    unsigned trace = 0;
    if (try_dll(&trace, false) != 19 || trace != 123)
        return 10;
    // The typed catch (4) runs before the exception object destructor (5).
    trace = 0;
    if (try_dll(&trace, true) != 71 || trace != 12345)
        return 11;
    return 0;
}
