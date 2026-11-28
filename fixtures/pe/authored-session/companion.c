// SPDX-License-Identifier: GPL-3.0-or-later

#include "xenora-session-v1.h"

static uint32_t color;

__declspec(dllexport) uint32_t CompanionColor(uint32_t input) {
    color = (color + input) & 255u;
    return color;
}

int DllMain(void *module, uint32_t reason, void *reserved) {
    (void)module;
    (void)reserved;
    if (reason == 1) {
#ifdef REFUSE_ATTACH
        return 0;
#elif defined(CALL_DURING_ATTACH)
        uint32_t input;
        (void)XenoraSessionNextInput(&input);
#endif
        color = 0x40;
    }
#ifdef CALL_DURING_DETACH
    if (reason == 0)
        (void)XenoraSessionPresent();
#elif defined(FAULT_DURING_DETACH)
    if (reason == 0)
        __builtin_trap();
#endif
    return 1;
}
