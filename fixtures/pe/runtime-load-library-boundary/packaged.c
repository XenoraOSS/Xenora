/* SPDX-License-Identifier: GPL-3.0-or-later */
#include "fixture.h"

__declspec(dllimport) unsigned int BoundaryValue(void);

void entry(void) {
    if (BoundaryValue() != 42)
        ExitProcess(20);
    ExitProcess(MARKER("consumer: package export returned 42\n") ? 0 : 21);
}
