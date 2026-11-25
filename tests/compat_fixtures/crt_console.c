// SPDX-License-Identifier: GPL-3.0-or-later
//
// Hosted checks for the console identity and the standard handles
// (package 8, first part, of #459). Built with mingw-w64, every call is an
// import of kernel32. A wrong check prints its number and the program
// exits with it. With CRT_CONSOLE_BREAK one expectation is wrong on
// purpose, as a control. The program's output mixes printf and WriteConsole
// on purpose: the runtime documents one coherent capture for both.

#include <stdio.h>
#include <string.h>
#include <windows.h>

static unsigned checks;
static unsigned failed;
static unsigned failures;

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

int main(void) {
    DWORD mode = 0;
    DWORD count = 0;
    char line[64];

    // 1. The three devices answer distinct nonzero handles.
    HANDLE in = GetStdHandle(STD_INPUT_HANDLE);
    HANDLE out = GetStdHandle(STD_OUTPUT_HANDLE);
    HANDLE err = GetStdHandle(STD_ERROR_HANDLE);
    CHECK("input handle", in != NULL);
    CHECK("output handle", out != NULL);
    CHECK("error handle", err != NULL);
    CHECK("handles distinct", in != out && out != err && in != err);
    SetLastError(0);
    CHECK("unknown device", GetStdHandle(0xFFFFFF00) == NULL
          && GetLastError() == ERROR_INVALID_PARAMETER);

    // 2. The console code pages are UTF-8.
    CHECK("console cp", GetConsoleCP() == 65001);
    CHECK("console output cp", GetConsoleOutputCP() == 65001);

    // 3. The modes round-trip; an unknown handle fails.
    CHECK("get mode", GetConsoleMode(out, &mode) != 0);
    CHECK("set mode", SetConsoleMode(out, 0x3) != 0);
    mode = 0;
    CHECK("mode round-trips", GetConsoleMode(out, &mode) != 0 && mode == 0x3);
    SetLastError(0);
    CHECK("mode unknown handle",
          GetConsoleMode((HANDLE)0xDEAD, &mode) == 0
              && GetLastError() == ERROR_INVALID_HANDLE);

    // 4. WriteConsole reports the characters consumed, narrow and wide.
    CHECK("write narrow", WriteConsoleA(out, "narrow\n", 7, &count, NULL) != 0 && count == 7);
    CHECK("write wide", WriteConsoleW(out, L"wide\n", 5, &count, NULL) != 0 && count == 5);
    CHECK("write error", WriteConsoleA(err, "to-error\n", 9, &count, NULL) != 0);
    SetLastError(0);
    CHECK("write input fails",
          WriteConsoleA(in, "x", 1, &count, NULL) == 0
              && GetLastError() == ERROR_INVALID_HANDLE);
    SetLastError(0);
    CHECK("write reserved fails",
          WriteConsoleA(out, "x", 1, &count, (void *)1) == 0
              && GetLastError() == ERROR_INVALID_PARAMETER);

    // 5. ReadConsole hands one line back with CR LF restored. The run feeds
    // no input here, so the read ends empty at the end of the input.
    CHECK("read empty", ReadConsoleA(in, line, 32, &count, NULL) != 0 && count == 0);

    // 6. SetStdHandle stores a replacement and GetStdHandle answers it.
    CHECK("set std handle", SetStdHandle(STD_OUTPUT_HANDLE, (HANDLE)0x5555) != 0);
    CHECK("get redirected", GetStdHandle(STD_OUTPUT_HANDLE) == (HANDLE)0x5555);
    SetLastError(0);
    CHECK("write redirected fails",
          WriteConsoleA((HANDLE)0x5555, "x", 1, &count, NULL) == 0
              && GetLastError() == ERROR_INVALID_HANDLE);
    SetStdHandle(STD_OUTPUT_HANDLE, out);

#ifdef CRT_CONSOLE_BREAK
    CHECK("the control fails", count == 999);
#endif

    if (failures) {
        printf("crt-console: %u/%u checks failed, first %u\n", failures, checks, failed);
        return failed;
    }
    printf("crt-console: %u/%u checks passed\n", checks, checks);
    return 0;
}
