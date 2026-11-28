// SPDX-License-Identifier: GPL-3.0-or-later
#include "cells.hpp"

extern "C" __declspec(dllexport) volatile Id *ConsumerCell(unsigned index) {
    return cell(index);
}
extern "C" __declspec(dllexport) unsigned long long ConsumerRead(unsigned index) {
    return cell(index)->stamp;
}
extern "C" __declspec(dllexport) void ConsumerWrite(unsigned index, unsigned long long value) {
    cell(index)->stamp = value;
}
