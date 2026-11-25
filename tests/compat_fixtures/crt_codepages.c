// SPDX-License-Identifier: GPL-3.0-or-later
//
// Hosted checks for the encoding conversions (package 7 of the roadmap in
// #459). Built with mingw-w64, every call is an import of kernel32. The
// program converts between bytes and wide characters in UTF-8 and in the
// ANSI page and compares results, sizes and error codes with what
// Microsoft's MultiByteToWideChar and WideCharToMultiByte pages describe. A
// wrong check prints its number and the program exits with it. With
// CRT_CODEPAGES_BREAK the expectation of one check is wrong on purpose, as
// a control.

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
    // UTF-8 "hé€": three characters in six bytes, plus the terminator.
    static const char utf8[] = "h\xc3\xa9\xe2\x82\xac";
    wchar_t wide[16];
    memset(wide, 0xAA, sizeof wide);

    // 1. A size query counts the terminator under a -1 byte count.
    int asked = MultiByteToWideChar(CP_UTF8, 0, utf8, -1, NULL, 0);
    CHECK("size query counts the terminator", asked == 4);

    // 2. The conversion itself.
    int wrote = MultiByteToWideChar(CP_UTF8, 0, utf8, -1, wide, asked);
    CHECK("conversion returns the same count", wrote == 4);
    CHECK("h", wide[0] == L'h');
    CHECK("e acute", wide[1] == 0xE9);
    CHECK("euro", wide[2] == 0x20AC);
    CHECK("terminated", wide[3] == 0);

    // 3. A positive byte count converts exactly that many bytes.
    CHECK("exact byte count",
          MultiByteToWideChar(CP_UTF8, 0, utf8, 1, wide, 8) == 1);

    // 4. The ANSI page: byte 0x80 is the euro sign on Windows-1252.
    static const char ansi_bytes[] = {(char)0x80, (char)0xE9, 0};
    wrote = MultiByteToWideChar(1252, 0, ansi_bytes, -1, wide, 8);
    CHECK("cp1252 euro", wrote == 3 && wide[0] == 0x20AC && wide[1] == 0xE9);

    // 5. A malformed sequence is replaced without the strict flag and fails
    // with it.
    static const char bad[] = {(char)0xFF, 0};
    wrote = MultiByteToWideChar(CP_UTF8, 0, bad, 1, wide, 8);
    CHECK("malformed replaced", wrote == 1 && wide[0] == 0xFFFD);
    SetLastError(0);
    wrote = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, bad, 1, wide, 8);
    CHECK("strict malformed fails",
          wrote == 0 && GetLastError() == ERROR_NO_UNICODE_TRANSLATION);

    // 6. A zero byte count fails, as the page documents.
    SetLastError(0);
    CHECK("zero count fails",
          MultiByteToWideChar(CP_UTF8, 0, utf8, 0, wide, 8) == 0
              && GetLastError() == ERROR_INVALID_PARAMETER);

    // 7. A small output buffer fails.
    SetLastError(0);
    CHECK("small output fails",
          MultiByteToWideChar(CP_UTF8, 0, utf8, -1, wide, 2) == 0
              && GetLastError() == ERROR_INSUFFICIENT_BUFFER);

    // 8. The way back: wide to UTF-8.
    char bytes[16];
    memset(bytes, 0xAA, sizeof bytes);
    wrote = WideCharToMultiByte(CP_UTF8, 0, L"h\xe9", -1, bytes, sizeof bytes, NULL, NULL);
    CHECK("wide to utf8 count", wrote == 4);
    CHECK("wide to utf8 bytes",
          bytes[0] == 'h' && (unsigned char)bytes[1] == 0xC3
              && (unsigned char)bytes[2] == 0xA9 && bytes[3] == 0);

    // 9. A character the ANSI page cannot represent becomes the default and
    // sets the flag.
    BOOL used = 0;
    wchar_t eastern[2] = {0x4E2D, 0};
    wrote = WideCharToMultiByte(1252, 0, eastern, -1, bytes, sizeof bytes, NULL, &used);
    CHECK("unrepresentable becomes default",
          wrote == 2 && bytes[0] == '?' && used != 0);

    // 10. The strict form of that failure.
    SetLastError(0);
    wrote = WideCharToMultiByte(1252, WC_ERR_INVALID_CHARS, eastern, -1,
                                bytes, sizeof bytes, NULL, NULL);
    CHECK("strict unrepresentable fails",
          wrote == 0 && GetLastError() == ERROR_NO_UNICODE_TRANSLATION);

    // 11. The pages answer their numbers.
    CHECK("ansi page is utf8", GetACP() == CP_UTF8);
    CHECK("oem page", GetOEMCP() == 437);
    CHECK("valid pages",
          IsValidCodePage(1252) && IsValidCodePage(437) && IsValidCodePage(CP_UTF8));
    CHECK("invalid page", !IsValidCodePage(99999));

    // 12. GetCPInfo: the widest character of each page.
    CPINFO info;
    CHECK("cpinfo utf8", GetCPInfo(CP_UTF8, &info) && info.MaxCharSize == 4);
    CHECK("cpinfo ansi", GetCPInfo(1252, &info) && info.MaxCharSize == 1);
    SetLastError(0);
    CHECK("cpinfo invalid fails",
          !GetCPInfo(99999, &info) && GetLastError() == ERROR_INVALID_PARAMETER);

#ifdef CRT_CODEPAGES_BREAK
    CHECK("the control fails", asked == 999);
#endif

    if (failures) {
        printf("crt-codepages: %u/%u checks failed, first %u\n", failures, checks, failed);
        return failed;
    }
    printf("crt-codepages: %u/%u checks passed\n", checks, checks);
    return 0;
}
