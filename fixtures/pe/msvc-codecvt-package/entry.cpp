// SPDX-License-Identifier: GPL-3.0-or-later
#include "cells.hpp"

extern "C" __declspec(dllimport) volatile Id *ProducerCell(unsigned);
extern "C" __declspec(dllimport) unsigned long long ProducerRead(unsigned);
extern "C" __declspec(dllimport) void ProducerWrite(unsigned, unsigned long long);
extern "C" __declspec(dllimport) volatile Id *ConsumerCell(unsigned);
extern "C" __declspec(dllimport) unsigned long long ConsumerRead(unsigned);
extern "C" __declspec(dllimport) void ConsumerWrite(unsigned, unsigned long long);

extern "C" int entry() {
    for (unsigned i = 0; i < 4; ++i) {
        const volatile Id *address = cell(i);
        if (ProducerCell(i) != address || ConsumerCell(i) != address)
            return 10 + static_cast<int>(i);
        if (address->stamp || ProducerRead(i) || ConsumerRead(i))
            return 20 + static_cast<int>(i);
        for (unsigned j = 0; j < i; ++j) {
            if (address == cell(j))
                return 30 + static_cast<int>(i);
        }
    }

    const unsigned long long first = 0x123456789abcdef0ull;
    ProducerWrite(0, first);
    if (cell(0)->stamp != first || ConsumerRead(0) != first)
        return 40;

    const unsigned long long second = 0xfedcba9876543210ull;
    ConsumerWrite(2, second);
    if (cell(2)->stamp != second || ProducerRead(2) != second)
        return 41;

    if (cell(0)->stamp != first || ProducerRead(0) != first || ConsumerRead(0) != first)
        return 42;
    for (unsigned i = 1; i < 4; i += 2) {
        if (cell(i)->stamp || ProducerRead(i) || ConsumerRead(i))
            return 50 + static_cast<int>(i);
    }

#ifdef UNSUPPORTED_SPECIALIZATION
    // wchar_t + char is outside this satellite's four supported exports.
    return static_cast<int>(
        static_cast<volatile Id *>(&std::codecvt<wchar_t, char, _Mbstatet>::id)->stamp);
#else
    return 0;
#endif
}
