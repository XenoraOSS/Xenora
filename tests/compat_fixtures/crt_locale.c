// SPDX-License-Identifier: GPL-3.0-or-later
//
// Hosted checks for the locale identity (package 7 of the roadmap in #459).
// Built with mingw-w64, every call is an import of kernel32. The program
// asks for the runtime's one locale and its strings and compares them with
// what Microsoft's GetLocaleInfo pages describe for English (United
// States). A wrong check prints its number and the program exits with it.
// With CRT_LOCALE_BREAK the expectation of one check is wrong on purpose,
// as a control.

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
    wchar_t wide[64];
    char narrow[64];

    // 1. The identity: one locale, en-US.
    CHECK("system lcid", GetSystemDefaultLCID() == 0x0409);
    CHECK("user lcid", GetUserDefaultLCID() == 0x0409);
    CHECK("user langid", GetUserDefaultLangID() == 0x0409);

    // 2. Its name, with the count including the terminator.
    int wrote = GetUserDefaultLocaleName(wide, 64);
    CHECK("locale name", wrote == 6 && wide[0] == L'e' && wide[3] == L'U' && wide[5] == 0);
    CHECK("locale name query", GetUserDefaultLocaleName(NULL, 0) == 6);
    SetLastError(0);
    CHECK("locale name small buffer",
          GetUserDefaultLocaleName(wide, 4) == 0
              && GetLastError() == ERROR_INSUFFICIENT_BUFFER);

    // 3. The strings of en-US, wide and narrow, terminator included.
    wrote = GetLocaleInfoW(0x0409, LOCALE_SENGLANGUAGE, wide, 64);
    CHECK("english wide", wrote == 8 && wide[0] == L'E' && wide[7] == 0);
    wrote = GetLocaleInfoA(0x0409, LOCALE_SENGLANGUAGE, narrow, 64);
    CHECK("english narrow", wrote == 8 && strcmp(narrow, "English") == 0);
    wrote = GetLocaleInfoA(0x0409, LOCALE_SDECIMAL, narrow, 64);
    CHECK("decimal point", wrote == 2 && narrow[0] == '.');
    wrote = GetLocaleInfoA(0x0409, LOCALE_SCURRENCY, narrow, 64);
    CHECK("currency", wrote == 2 && narrow[0] == '$');
    wrote = GetLocaleInfoA(0x0409, LOCALE_SSHORTDATE, narrow, 64);
    CHECK("short date", strcmp(narrow, "M/d/yyyy") == 0);

    // 4. The ex form: null is the user default, en-US any case works,
    // another name fails.
    wrote = GetLocaleInfoEx(NULL, LOCALE_SNAME, wide, 64);
    CHECK("ex null name", wrote == 6 && wide[0] == L'e');
    wrote = GetLocaleInfoEx(L"en-us", LOCALE_SNAME, wide, 64);
    CHECK("ex lowercase", wrote == 6);
    SetLastError(0);
    wrote = GetLocaleInfoEx(L"fr-FR", LOCALE_SNAME, wide, 64);
    CHECK("ex french fails", wrote == 0 && GetLastError() == ERROR_INVALID_PARAMETER);

    // 5. The failures: another locale, an unknown type, a small buffer.
    SetLastError(0);
    wrote = GetLocaleInfoW(0x040C, LOCALE_SENGLANGUAGE, wide, 64);
    CHECK("french lcid fails", wrote == 0 && GetLastError() == ERROR_INVALID_PARAMETER);
    SetLastError(0);
    wrote = GetLocaleInfoW(0x0409, 0x7777, wide, 64);
    CHECK("unknown type fails", wrote == 0 && GetLastError() == ERROR_INVALID_PARAMETER);
    SetLastError(0);
    wrote = GetLocaleInfoW(0x0409, LOCALE_SENGLANGUAGE, wide, 4);
    CHECK("small buffer fails", wrote == 0 && GetLastError() == ERROR_INSUFFICIENT_BUFFER);

    // 6. The size query agrees with the write.
    CHECK("size query", GetLocaleInfoW(0x0409, LOCALE_SENGLANGUAGE, NULL, 0) == 8);

#ifdef CRT_LOCALE_BREAK
    CHECK("the control fails", wrote == 999);
#endif

    if (failures) {
        printf("crt-locale: %u/%u checks failed, first %u\n", failures, checks, failed);
        return failed;
    }
    printf("crt-locale: %u/%u checks passed\n", checks, checks);
    return 0;
}
