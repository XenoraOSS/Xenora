// SPDX-License-Identifier: GPL-3.0-or-later
//
// Hosted checks for the buffer forms of printf (#466). Built with mingw-w64
// and -fno-builtin, every call is an import of the C runtime. The program
// formats into a buffer with snprintf, sprintf, _snprintf and sprintf_s and
// compares the result, the contents and the guard bytes around the buffer
// with what Microsoft's snprintf, _snprintf and sprintf_s pages describe. A
// wrong check prints its number and the program exits with it. With
// CRT_SPRINTF_BREAK the expectation of one check is wrong on purpose, as a
// control.

#include <stdio.h>
#include <string.h>

static unsigned failed;
static unsigned failures;

// One check: what the call must return, and what the bytes must be. `size`
// is the count argument; the guards are the four bytes before the buffer
// and the byte at buffer[size], which no call may touch.
static unsigned checks;
#define CHECK(note, expr)                                                \
    do {                                                                 \
        checks++;                                                        \
        if (!(expr)) {                                                   \
            if (!failed)                                                 \
                failed = checks;                                         \
            failures++;                                                  \
            printf("check %u failed: %s\n", checks, note);              \
        }                                                                \
    } while (0)

static char guard[4] = {'g', 'u', 'a', 'r'};
static char buf[64];
static char tail[4] = {'d', 'n', 'e', 'd'};

static void scrub(void) {
    memset(buf, 0xAA, sizeof buf);
}

static int guards_intact(void) {
    return guard[0] == 'g' && guard[3] == 'r' && tail[0] == 'd' && tail[3] == 'd';
}

int main(void) {
    int standard = 0;

    // snprintf, the standard form: the result is always the length the text
    // would have, and a terminator is written whenever count is not zero.
    scrub();
    CHECK("snprintf fit", snprintf(buf, 8, "<%d>", 42) == 4);
    CHECK("snprintf fit contents", memcmp(buf, "<42>\0", 5) == 0);
    CHECK("snprintf fit guard", (unsigned char)buf[5] == 0xAA && guards_intact());
    scrub();
    CHECK("snprintf exact", snprintf(buf, 5, "<%d>", 42) == 4);
    CHECK("snprintf exact contents", memcmp(buf, "<42>\0", 5) == 0);
    scrub();
    CHECK("snprintf one short", snprintf(buf, 4, "<%d>", 42) == 4);
    CHECK("snprintf one short contents", memcmp(buf, "<42\0", 4) == 0);
    scrub();
    CHECK("snprintf two", snprintf(buf, 2, "<%d>", 42) == 4);
    CHECK("snprintf two contents", memcmp(buf, "<\0", 2) == 0);
    scrub();
    CHECK("snprintf zero count", snprintf(buf, 0, "<%d>", 42) == 4);
    CHECK("snprintf zero count writes nothing", (unsigned char)buf[0] == 0xAA);
    CHECK("snprintf size query", snprintf(NULL, 0, "<%d>", 42) == 4);

    // sprintf: no truncation, and a form with arguments of several kinds.
    scrub();
    CHECK("sprintf", sprintf(buf, "%d %s %.2f", 7, "seven", 0.5) == 12);
    CHECK("sprintf contents", strcmp(buf, "7 seven 0.50") == 0);

    // _snprintf, the legacy form: the terminator is written only when the
    // text fits, and a truncation returns a negative value.
    scrub();
    CHECK("_snprintf fit", _snprintf(buf, 6, "<%d>", 42) == 4);
    CHECK("_snprintf fit contents", memcmp(buf, "<42>\0", 5) == 0);
    scrub();
    CHECK("_snprintf exact", _snprintf(buf, 4, "<%d>", 42) == 4);
    CHECK("_snprintf exact unterminated", memcmp(buf, "<42>", 4) == 0 && (unsigned char)buf[4] == 0xAA);
    scrub();
    CHECK("_snprintf short is truncated", _snprintf(buf, 3, "<%d>", 42) < 0);
    CHECK("_snprintf short contents", memcmp(buf, "<42", 3) == 0 && (unsigned char)buf[3] == 0xAA);
    CHECK("_snprintf size query", _snprintf(NULL, 0, "<%d>", 42) == 4);
    scrub();
    CHECK("_snprintf zero fits empty output", _snprintf(buf, 0, "%s", "") == 0 && (unsigned char)buf[0] == 0xAA);

    // sprintf_s, the secure form: a small buffer becomes the empty string.
    scrub();
    CHECK("sprintf_s fit", sprintf_s(buf, 8, "<%d>", 42) == 4);
    CHECK("sprintf_s fit contents", memcmp(buf, "<42>\0", 5) == 0);
    scrub();
    CHECK("sprintf_s small", sprintf_s(buf, 4, "<%d>", 42) == -1);
    CHECK("sprintf_s small empties", buf[0] == 0 && (unsigned char)buf[1] == 0xAA);

    // putchar echoes its character through stdout.
    CHECK("putchar result", putchar('!') == '!');
    putchar('\n');

    // The size query drives a two-pass use, as real programs do.
    {
        int len = snprintf(NULL, 0, "[%d]", 1234);
        scrub();
        CHECK("two-pass length", len == 6);
        CHECK("two-pass write", snprintf(buf, (size_t)len + 1, "[%d]", 1234) == 6);
        CHECK("two-pass contents", strcmp(buf, "[1234]") == 0);
    }

    // A control: with CRT_SPRINTF_BREAK this check expects the wrong
    // length, so it must fail; the harness asserts that it does.
    scrub();
    standard = snprintf(buf, 8, "<%d>", 42);
#ifdef CRT_SPRINTF_BREAK
    CHECK("control", standard == 3);
#else
    CHECK("control", standard == 4);
#endif

    printf("crt_sprintf: %u of %u checks passed\n", checks - failures, checks);
    return (int)failed;
}
