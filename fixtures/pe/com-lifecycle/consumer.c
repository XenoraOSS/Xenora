// SPDX-License-Identifier: GPL-3.0-or-later
#include "interfaces.h"

__declspec(dllexport) U32 ConsumeObject(Unknown *object) {
    static const Guid unsupported = {0x6f11ad0c, 0x2dc9, 0x4f26, {1, 2, 3, 4, 5, 6, 7, 8}};
    void *result = object;
    if (object->vtable->query(object, &unsupported, &result) != E_NOINTERFACE || result)
        return 1;
    if (object->vtable->query(object, &iid_unknown, 0) != E_POINTER)
        return 2;
    if (object->vtable->query(object, &iid_unknown, &result) != S_OK || result != object)
        return 3;

    Unknown *identity = result;
    // The exact counts describe the opt-in synthetic object.
    if (identity->vtable->add_ref(identity) != 3)
        return 4;
    if (identity->vtable->release(identity) != 2)
        return 5;
    if (identity->vtable->release(identity) != 1)
        return 6;
    return object->vtable->release(object) == 0 ? 0 : 7;
}
