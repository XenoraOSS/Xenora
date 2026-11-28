// SPDX-License-Identifier: GPL-3.0-or-later
#include "interfaces.h"

__declspec(dllexport) U32 InspectObject(Unknown *object) {
    static const Guid unsupported = {0x6f11ad0c, 0x2dc9, 0x4f26, {1, 2, 3, 4, 5, 6, 7, 8}};
    void *result = object;
    if (object->vtable->query_interface(object, &unsupported, &result) != E_NOINTERFACE ||
        result != 0)
        return 1;
    if (object->vtable->query_interface(object, &iid_unknown, 0) != E_POINTER)
        return 2;
    if (object->vtable->query_interface(object, &iid_unknown, &result) != S_OK ||
        result != object)
        return 3;

    // The initial reference and successful IUnknown query give two refs.
    // The exact counts are properties of the synthetic fixture.
    Unknown *identity = result;
    if (identity->vtable->add_ref(identity) != 3)
        return 4;
    if (identity->vtable->release(identity) != 2)
        return 5;
    if (identity->vtable->release(identity) != 1)
        return 6;
    return 0;
}

__declspec(dllexport) U32 ReleaseObject(Unknown *object) {
    return object->vtable->release(object);
}
