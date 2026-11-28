// SPDX-License-Identifier: GPL-3.0-or-later
#include "payload.h"

extern "C" __declspec(dllimport) unsigned ProduceOuter(Record &, bool);
extern "C" __declspec(dllimport) void ProduceInner(Record &);

extern "C" __declspec(dllexport) unsigned Bridge(Record &record, bool raise) {
    Guard frame{record, 0xa};
    try {
        Guard outer_call{record, 3};
        return ProduceOuter(record, raise);
    } catch (const Outer &outer) {
        if (&outer.record != &record || outer.value != 71)
            return 21;
        record.outer_address = reinterpret_cast<uintptr_t>(&outer);
        mark(record, 4);
        try {
            ProduceInner(record);
            return 22;
        } catch (const Inner &inner) {
            if (&inner.record != &record || inner.value != 52 ||
                reinterpret_cast<uintptr_t>(&inner) == record.outer_address)
                return 23;
            mark(record, 7);
        }
        mark(record, 9);
        throw;
    }
}
