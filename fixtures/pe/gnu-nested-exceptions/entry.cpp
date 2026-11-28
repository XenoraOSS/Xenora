// SPDX-License-Identifier: GPL-3.0-or-later
#include "payload.h"

extern "C" __declspec(dllimport) unsigned Bridge(Record &, bool);

int main() {
    Record normal;
    if (Bridge(normal, false) != 19 || normal.trace != 0x123a || normal.outer_address != 0)
        return 10;

    Record nested;
    try {
        const unsigned unexpected = Bridge(nested, true);
        return unexpected == 0 ? 15 : unexpected;
    } catch (const Outer &outer) {
        if (&outer.record != &nested || outer.value != 71)
            return 11;
        if (reinterpret_cast<uintptr_t>(&outer) != nested.outer_address)
            return 12;
        if (nested.trace != 0x123456789a)
            return 13;
        mark(nested, 0xb);
    }
    // The inner object dies while the outer handler is active. The producer's
    // outer destructor runs once, after the EXE catches that same object.
    return nested.trace == 0x123456789abc ? 0 : 14;
}
