/* SPDX-License-Identifier: GPL-3.0-or-later */
#include "fixture.h"

static unsigned int attached;

int DllMain(handle image, dword reason, void *reserved) {
    (void)image;
    (void)reserved;
    if (reason == 1) {
        if (!MARKER("provider: attach\n"))
            return 0;
        attached = 37;
    }
    return 1;
}

__declspec(dllexport) unsigned int BoundaryValue(void) {
    if (attached != 37 || !MARKER("provider: export\n"))
        return 0;
    return attached + 5;
}
