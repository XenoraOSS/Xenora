// SPDX-License-Identifier: GPL-3.0-or-later
//
// A hosted C program for the second part of #493: the secure open fopen_s,
// the end-of-file indicator feof observes, and clearerr. Every expected
// value follows from the bytes this program writes and the Microsoft pages
// for fopen_s, feof and clearerr. It prints how many checks passed and
// returns the number of the first that failed, or 0.
#include <stdio.h>
#include <errno.h>

static unsigned passed, total, first;

static void check(int ok, const char *what) {
    total++;
    if (ok) {
        passed++;
    } else {
        if (!first) first = total;
        printf("check %u failed: %s\n", total, what);
    }
}

static int same(const char *a, const char *b) {
    while (*a && *a == *b) a++, b++;
    return *a == *b;
}

int main(void) {
    char line[16];
    FILE *f = (FILE *)1;

    // A good open: 0, the pointer holds the stream.
    errno = 0;
    check(fopen_s(&f, "secure.bin", "w+b") == 0 && f != NULL, "fopen_s w+b");
    check(fwrite("one\ntwo", 1, 7, f) == 7, "fwrite");
    fclose(f);
    check(fopen_s(&f, "secure.bin", "rb") == 0, "reopen");
    check(fgets(line, sizeof line, f) && same(line, "one\n"), "first line");
    check(feof(f) == 0, "not yet at the end");
    check(fgets(line, sizeof line, f) && same(line, "two"), "last line without a newline");
    check(feof(f) == 0, "indicator only when a read meets the end");
    check(fgets(line, sizeof line, f) == NULL, "read at the end");
    check(feof(f) != 0, "end-of-file indicator set");
    clearerr(f);
    check(feof(f) == 0, "clearerr clears the indicator");
    fclose(f);

    // Failures: the error number is the result and errno, the pointer null.
    errno = 0;
    f = (FILE *)1;
    errno_t e = fopen_s(&f, "missing.bin", "rb");
    check(e == ENOENT && errno == ENOENT && f == NULL, "missing file is ENOENT");
    errno = 0;
    e = fopen_s(&f, "secure.bin", "zz");
    check(e == EINVAL && errno == EINVAL && f == NULL, "unknown mode is EINVAL");
    errno = 0;
    e = fopen_s(NULL, "secure.bin", "rb");
    check(e == EINVAL && errno == EINVAL, "null output is EINVAL");
    errno = 0;
    e = fopen_s(&f, NULL, "rb");
    check(e == EINVAL && errno == EINVAL && f == NULL, "null name is EINVAL");
    errno = 0;
    e = fopen_s(&f, "secure.bin", NULL);
    check(e == EINVAL && errno == EINVAL && f == NULL, "null mode is EINVAL");
    errno = 0;
    e = fopen_s(&f, "", "rb");
    check(e == EINVAL && errno == EINVAL && f == NULL, "empty name is EINVAL");

    // `remove` is the first part of #493, not this one; the file stays.
#ifdef CRT_SECURE_BREAK
    // Control: a wrong expectation must be detected.
    check(1 == 2, "control");
#endif
    printf("crt_secure: %u of %u checks passed\n", passed, total);
    return (int)first;
}
