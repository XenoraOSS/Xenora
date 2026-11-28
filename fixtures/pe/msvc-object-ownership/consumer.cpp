// SPDX-License-Identifier: GPL-3.0-or-later
#include "object.hpp"

extern "C" __declspec(dllexport) int XenoraConsume(Object *object, unsigned int permit) {
    if (!object || permit != 17) {
        return -1;
    }
    unsigned int value = object->value();
    delete object;
    return static_cast<int>(value);
}
