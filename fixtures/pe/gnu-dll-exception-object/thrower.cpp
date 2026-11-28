// SPDX-License-Identifier: GPL-3.0-or-later
#include "payload.h"

Payload::~Payload() { *trace = *trace * 10 + 5; }
struct ThrowGuard {
    unsigned *trace;
    ~ThrowGuard() { *trace = *trace * 10 + 2; }
};
extern "C" __declspec(dllexport) unsigned ThrowFromDll(unsigned *trace, bool raise) {
    ThrowGuard guard{trace};
    *trace = *trace * 10 + 1;
    if (raise)
        throw Payload(trace, 71);
    return 19;
}
