// SPDX-License-Identifier: GPL-3.0-or-later
//
// A hosted C program for #459: wide stream io in the C locale. fputws and
// fgetws convert every byte and its wide character one-to-one, putwchar
// writes to stdout, and a stream oriented one way refuses the other kind of
// io. Diagnostics go to stderr: stdout stays free for the wide orientation
// that putwchar sets. Prints how many checks passed and returns the first
// that failed, or 0.
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
    wchar_t line[32];

    FILE *f = fopen("wide.bin", "w+b");
    check(f != NULL, "fopen w+b");
#ifdef CRT_WSTREAM_BREAK
    check(fputws(L"wide one\nwide two\n", f) == (wint_t)WEOF, "fputws");
#else
    check(fputws(L"wide one\nwide two\n", f) >= 0, "fputws");
#endif
    check(_fseeki64(f, 0, SEEK_SET) == 0, "seek start");
    check(fgetws(line, 32, f) == line && same_wide(line, L"wide one\n"), "first wide line");
    check(fgetws(line, 32, f) == line && same_wide(line, L"wide two\n"), "second wide line");
    check(fgetws(line, 32, f) == NULL, "end of wide file");
    check(fclose(f) == 0, "fclose");
    check(remove("wide.bin") == 0, "remove");

    // Orientation: a wide-oriented stream refuses byte io, and the byte
    // orientation refuses wide io.
    f = fopen("orient.bin", "w+b");
    check(f != NULL, "fopen orient");
    check(fputws(L"w\n", f) >= 0, "orient wide first");
    check(fputs("byte\n", f) == EOF, "byte write refused after wide");
    check(fclose(f) == 0, "fclose orient");
    f = fopen("orient2.bin", "w+b");
    check(fputs("byte\n", f) >= 0, "byte first");
    check(fputws(L"w\n", f) == (wint_t)WEOF || fputws(L"", f) == (wint_t)WEOF, "wide write refused after byte");
    check(fclose(f) == 0, "fclose orient2");
    remove("orient.bin");
    remove("orient2.bin");

    // putwchar converts in the C locale; a unit above 255 cannot convert.
    check(putwchar(L'z') == L'z', "putwchar");
    check(putwchar(0x100) == (wint_t)WEOF, "putwchar above the C locale");

    fprintf(stderr, "crt-wstream: %u/%u checks passed\n", passed, total);
    return (int)first;
}
