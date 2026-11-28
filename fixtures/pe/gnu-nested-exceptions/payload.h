// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once

#include <stdint.h>

struct Record {
    uint64_t trace = 0;
    uintptr_t outer_address = 0;
};

inline void mark(Record &record, unsigned digit) {
    record.trace = (record.trace << 4) | digit;
}

struct Guard {
    Record &record;
    unsigned digit;
    ~Guard() { mark(record, digit); }
};

struct Outer {
    Record &record;
    const unsigned value;
    Outer(Record &record, unsigned value) noexcept : record(record), value(value) {}
    Outer(const Outer &) = delete;
    ~Outer();
};

struct Inner {
    Record &record;
    const unsigned value;
    Inner(Record &record, unsigned value) noexcept : record(record), value(value) {}
    Inner(const Inner &) = delete;
    ~Inner();
};
