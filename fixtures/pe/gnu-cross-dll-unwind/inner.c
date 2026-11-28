// SPDX-License-Identifier: GPL-3.0-or-later

__declspec(dllexport) int RelayInner(void (*callback)(void), volatile unsigned int *trace) {
    volatile int result = 29;
    *trace = *trace * 10 + 4;
    callback();
    return result;
}
