// SPDX-License-Identifier: GPL-3.0-or-later
//
// Hosted checks for the byte-order string comparison, classification and
// case mapping services (package 7, third part, of #459). Built with
// mingw-w64, every call is an import of kernel32. A wrong check prints its
// number and the program exits with it. With CRT_COMPARE_BREAK the
// expectation of one check is wrong on purpose, as a control.

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
    wchar_t wide[16];
    char narrow[16];
    WORD types[8];

    // 1. The simple operations.
    CHECK("lstrlenA", lstrlenA("hello") == 5);
    CHECK("lstrlenW", lstrlenW(L"abc") == 3);
    CHECK("lstrcpyA", lstrcpyA(narrow, "copy") == narrow && strcmp(narrow, "copy") == 0);

    // 2. lstrcmp is ordinal: lowercase letters sort after uppercase.
    CHECK("lstrcmp equal", lstrcmpA("same", "same") == 0);
    CHECK("lstrcmp ordinal", lstrcmpA("Hello", "HELLO") > 0);
    CHECK("lstrcmpi folds", lstrcmpiA("Hello", "HELLO") == 0);
    CHECK("lstrcmpi different", lstrcmpiA("Hello", "world") != 0);

    // 3. CompareString answers 1/2/3 and folds only when asked.
    CHECK("compare equal", CompareStringA(0x0409, 0, "same", -1, "same", -1) == CSTR_EQUAL);
    CHECK("compare ordinal", CompareStringA(0x0409, 0, "Hello", -1, "HELLO", -1) == CSTR_GREATER_THAN);
    CHECK("compare less", CompareStringA(0x0409, 0, "apple", -1, "banana", -1) == CSTR_LESS_THAN);
    CHECK("compare folded", CompareStringA(0x0409, NORM_IGNORECASE, "Hello", -1, "HELLO", -1) == CSTR_EQUAL);
    CHECK("compare wide", CompareStringW(0x0409, NORM_IGNORECASE, L"AbC", -1, L"aBc", -1) == CSTR_EQUAL);
    CHECK("compare wide ordinal", CompareStringW(0x0409, 0, L"AbC", -1, L"aBc", -1) == CSTR_LESS_THAN);

    // 4. Its failures: another locale, unsupported flags.
    SetLastError(0);
    CHECK("compare french fails",
          CompareStringA(0x040C, 0, "a", -1, "a", -1) == 0
              && GetLastError() == ERROR_INVALID_PARAMETER);
    SetLastError(0);
    CHECK("compare flags fail",
          CompareStringA(0x0409, NORM_IGNORESYMBOLS, "a", -1, "a", -1) == 0
              && GetLastError() == ERROR_INVALID_PARAMETER);

    // 5. GetStringTypeW classifies ASCII, terminator included for -1.
    CHECK("stringtype runs", GetStringTypeW(CT_CTYPE1, L"A9 ", -1, types) != 0);
    CHECK("type upper", (types[0] & (C1_UPPER | C1_ALPHA)) == (C1_UPPER | C1_ALPHA));
    CHECK("type digit", (types[1] & C1_DIGIT) == C1_DIGIT);
    CHECK("type space", (types[2] & C1_SPACE) == C1_SPACE);
    CHECK("type terminator", (types[3] & C1_CNTRL) == C1_CNTRL);
    SetLastError(0);
    CHECK("ctype2 refused",
          GetStringTypeW(CT_CTYPE2, L"A", -1, types) == 0
              && GetLastError() == ERROR_INVALID_PARAMETER);

    // 6. LCMapStringW maps ASCII case, counts the terminator for -1.
    int wrote = LCMapStringW(0x0409, LCMAP_UPPERCASE, L"aBc", -1, wide, 16);
    CHECK("lcmap upper", wrote == 4 && wide[0] == L'A' && wide[1] == L'B' && wide[3] == 0);
    wrote = LCMapStringW(0x0409, LCMAP_LOWERCASE, L"aBc", -1, wide, 16);
    CHECK("lcmap lower", wrote == 4 && wide[0] == L'a' && wide[1] == L'b' && wide[2] == L'c');
    CHECK("lcmap size query", LCMapStringW(0x0409, LCMAP_UPPERCASE, L"aBc", -1, NULL, 0) == 4);
    SetLastError(0);
    CHECK("lcmap small buffer",
          LCMapStringW(0x0409, LCMAP_UPPERCASE, L"aBc", -1, wide, 2) == 0
              && GetLastError() == ERROR_INSUFFICIENT_BUFFER);
    SetLastError(0);
    CHECK("lcmap sortkey refused",
          LCMapStringW(0x0409, LCMAP_SORTKEY, L"a", -1, wide, 16) == 0
              && GetLastError() == ERROR_INVALID_PARAMETER);

#ifdef CRT_COMPARE_BREAK
    CHECK("the control fails", wrote == 999);
#endif

    if (failures) {
        printf("crt-compare: %u/%u checks failed, first %u\n", failures, checks, failed);
        return failed;
    }
    printf("crt-compare: %u/%u checks passed\n", checks, checks);
    return 0;
}
