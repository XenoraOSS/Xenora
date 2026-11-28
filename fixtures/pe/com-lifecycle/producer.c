// SPDX-License-Identifier: GPL-3.0-or-later
#include "interfaces.h"

__declspec(dllimport) HResult CoGetClassObject(const Guid *, U32, void *, const Guid *, void **);
__declspec(dllimport) HResult CoCreateInstance(const Guid *, Unknown *, U32, const Guid *, void **);

static const Guid fixture_class = {0x58713751, 0x7490, 0x414e, {0xa0, 0x58, 0x63, 0x6f, 0x6d, 0, 0, 1}};
static const Guid iid_factory = {1, 0, 0, {0xc0, 0, 0, 0, 0, 0, 0, 0x46}};
enum { CLSCTX_INPROC_SERVER = 1 };

__declspec(dllexport) HResult AcquireFactory(void **factory) {
    return CoGetClassObject(&fixture_class, CLSCTX_INPROC_SERVER, 0, &iid_factory, factory);
}

__declspec(dllexport) HResult ActivateObject(void **object) {
    return CoCreateInstance(&fixture_class, 0, CLSCTX_INPROC_SERVER, &iid_unknown, object);
}
