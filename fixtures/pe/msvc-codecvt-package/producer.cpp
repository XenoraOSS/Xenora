// SPDX-License-Identifier: GPL-3.0-or-later
#include "cells.hpp"

extern "C" __declspec(dllexport) volatile Id *ProducerCell(unsigned index) {
    return cell(index);
}
extern "C" __declspec(dllexport) unsigned long long ProducerRead(unsigned index) {
    return cell(index)->stamp;
}
extern "C" __declspec(dllexport) void ProducerWrite(unsigned index, unsigned long long value) {
    cell(index)->stamp = value;
}
