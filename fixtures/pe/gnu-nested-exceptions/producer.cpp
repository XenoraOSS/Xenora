// SPDX-License-Identifier: GPL-3.0-or-later
#include "payload.h"

Outer::~Outer() { mark(record, 0xc); }
Inner::~Inner() { mark(record, 8); }

extern "C" __declspec(dllexport) unsigned ProduceOuter(Record &record, bool raise) {
    Guard guard{record, 2};
    mark(record, 1);
    if (raise)
        throw Outer(record, 71);
    return 19;
}

extern "C" __declspec(dllexport) void ProduceInner(Record &record) {
    Guard guard{record, 6};
    mark(record, 5);
    throw Inner(record, 52);
}
