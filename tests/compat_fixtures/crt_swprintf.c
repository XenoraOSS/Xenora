// SPDX-License-Identifier: GPL-3.0-or-later
//
// A hosted C program for #459: the wide buffer forms of printf. swprintf
// counts in wide units, %s is the wide string and %S the narrow one in a
// wide format, and truncation follows the standard snprintf rules. Prints
// how many checks passed on stderr and returns the first that failed, or 0.
#include <stdio.h>
#include <wchar.h>

static unsigned passed, total, first;

static void check(int ok, const char *what) {
    total++;
    if (ok) {
        passed++;
    } else {
        if (!first) first = total;
        fprintf(stderr, "check %u failed: %s\n", total, what);
    }
}

static int same_wide(const wchar_t *a, const wchar_t *b) {
    while (*a && *a == *b) a++, b++;
    return *a == *b;
}

int main(void) {
    wchar_t buf[80];

#ifdef CRT_SWPRINTF_BREAK
    check(swprintf(buf, 80, L"n=%d", 42) == 0, "integer conversion");
#else
    check(swprintf(buf, 80, L"n=%d", 42) == 4 && same_wide(buf, L"n=42"), "integer conversion");
#endif
    check(swprintf(buf, 80, L"w=%s s=%S", L"wide", "narrow") == 15 &&
          same_wide(buf, L"w=wide s=narrow"), "both string kinds");
    check(swprintf(buf, 80, L"c=%c%C", L'W', 'n') == 4 && same_wide(buf, L"c=Wn"), "both char kinds");
    check(swprintf(buf, 80, L"%5u|%-5d|", 7, 7) == 12 && same_wide(buf, L"    7|7    |"), "padding");
    check(swprintf(buf, 80, L"%x %X %o", 255, 255, 8) == 8 && same_wide(buf, L"ff FF 10"), "bases");
    check(swprintf(buf, 80, L"%*d", 6, 9) == 6 && same_wide(buf, L"     9"), "star width");
    check(swprintf(buf, 80, L"%.2s", L"abcdef") == 2 && same_wide(buf, L"ab"), "precision on wide string");

    // Truncation answers -1 in the CRT's default mode, as Microsoft's
    // swprintf page describes, and the buffer is left terminated where the
    // text was cut.
    int n = swprintf(buf, 4, L"abcdef");
    check(n == -1 && buf[0] == L'a' && buf[2] == L'c' && buf[3] == L'd',
          "truncation writes count characters without a terminator");
    // There is no size query in this mode: a null buffer answers -1.
    check(swprintf(NULL, 0, L"%d%d", 1, 22) == -1, "no size query by default");

    fprintf(stderr, "crt-swprintf: %u/%u checks passed\n", passed, total);
    return (int)first;
}
