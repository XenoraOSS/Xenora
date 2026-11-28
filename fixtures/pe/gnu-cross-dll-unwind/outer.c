// SPDX-License-Identifier: GPL-3.0-or-later

__declspec(dllimport) int RelayInner(void (*callback)(void), volatile unsigned int *trace);

__declspec(dllexport) int RelayOuter(void (*callback)(void), volatile unsigned int *trace) {
    volatile int result = 7;
    *trace = *trace * 10 + 3;
    return RelayInner(callback, trace) + result;
}
