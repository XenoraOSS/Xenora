// SPDX-License-Identifier: GPL-3.0-or-later

#include <stdint.h>

__declspec(dllimport) int GetStringTypeW(unsigned, const uint16_t *, int, uint16_t *);
__declspec(dllimport) void ExitProcess(unsigned);

enum {
    CTYPE1 = 1,
    SPACE = 0x0008,
    PUNCT = 0x0010,
    CONTROL = 0x0020,
    BLANK = 0x0040,
    CANARY = 0x55aa,
    COUNT = 0xa1
};

static unsigned check(void) {
    uint16_t text[COUNT + 1];
    uint16_t result[COUNT + 3];
    text[0] = '!';
    for (unsigned index = 0; index < COUNT; ++index)
        text[index + 1] = (uint16_t)index;
    for (unsigned index = 0; index < COUNT + 3; ++index)
        result[index] = CANARY;
    if (!GetStringTypeW(CTYPE1, text, COUNT + 1, result + 1))
        return 10;
    if (result[0] != CANARY || result[COUNT + 2] != CANARY)
        return 11;

    unsigned controls = 0;
    for (unsigned unit = 0; unit < COUNT; ++unit) {
        const uint16_t flags = result[unit + 2];
        if (flags == CANARY)
            return 20;
        const int control = unit <= 0x1f || (unit >= 0x7f && unit <= 0x9f);
        if (control && (!(flags & CONTROL) || (flags & PUNCT)))
            return unit == 9 ? 12 : 13;
        if (!control && (flags & CONTROL))
            return 14;
        if (flags & CONTROL)
            ++controls;
    }
    if (controls != 65)
        return 15;
    if (result[9 + 2] != (SPACE | BLANK | CONTROL) ||
        result[0x20 + 2] != (SPACE | BLANK) ||
        result[0x85 + 2] != (SPACE | CONTROL) ||
        !(result[1] & PUNCT))
        return 16;
    if (result[0xa0 + 2] != 0)
        return 17;

    const uint16_t mixed[] = {0x00ff, 0x0100, 0x03a9, 0, '!'};
    uint16_t terminated[] = {CANARY, CANARY, CANARY, CANARY, CANARY, CANARY};
    if (!GetStringTypeW(CTYPE1, mixed, -1, terminated + 1))
        return 18;
    if (terminated[0] != CANARY || terminated[1] != 0 || terminated[2] != 0 ||
        terminated[3] != 0 || terminated[4] != CONTROL || terminated[5] != CANARY)
        return 19;

    return 0;
}

void entry(void) {
    ExitProcess(check());
}
