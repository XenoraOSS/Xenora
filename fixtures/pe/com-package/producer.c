// SPDX-License-Identifier: GPL-3.0-or-later
#include "interfaces.h"

__declspec(dllimport) HResult CoCreateInstance(
    const Guid *, Unknown *, U32, const Guid *, void **);

__declspec(dllexport) HResult ProduceObject(void **object) {
    // This class ID belongs only to the opt-in Xenora fixture.
    static const Guid fixture_class =
        {0x58713751, 0x7490, 0x414e, {0xa0, 0x58, 0x63, 0x6f, 0x6d, 0, 0, 1}};
    const U32 in_process_server = 1;
    return CoCreateInstance(&fixture_class, 0, in_process_server, &iid_unknown, object);
}
