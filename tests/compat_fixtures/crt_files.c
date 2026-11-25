// SPDX-License-Identifier: GPL-3.0-or-later
//
// A hosted C program for #493: file positions with _ftelli64, _fseeki64,
// fgetpos and fsetpos, and the files themselves with rename and remove. Each
// expected value follows from the bytes this program writes and the Microsoft
// pages for each function; it prints how many checks passed and returns the
// number of the first that failed, or 0.
#include <stdio.h>

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
    char line[32];
    fpos_t mark;

    // Binary: positions are byte offsets.
    FILE *f = fopen("pos.bin", "w+b");
    check(f != NULL, "fopen w+b");
    check(_ftelli64(f) == 0, "new stream at 0");
    check(fwrite("line one\nline two\n", 1, 18, f) == 18, "fwrite");
    check(_ftelli64(f) == 18, "after write");
    check(fgetpos(f, &mark) == 0, "fgetpos");
    check(_fseeki64(f, 0, SEEK_SET) == 0, "seek start");
    check(fgets(line, sizeof line, f) == line && same(line, "line one\n"), "first line");
    check(_ftelli64(f) == 9, "after first line");
    check(_fseeki64(f, -4, SEEK_END) == 0 && fgets(line, sizeof line, f) && same(line, "two\n"),
          "seek from end");
    check(_fseeki64(f, -9, SEEK_CUR) == 0 && fgets(line, sizeof line, f) && same(line, "line two\n"),
          "seek back from current");
    check(_fseeki64(f, -1, SEEK_SET) == -1 && _ftelli64(f) == 18, "before start refused");
    check(_fseeki64(f, 0, 7) == -1, "unknown origin refused");
    check(fsetpos(f, &mark) == 0 && _ftelli64(f) == 18, "fsetpos");
    check(fgets(line, sizeof line, f) == NULL, "end after fsetpos");
    check(fclose(f) == 0, "fclose binary");

    // Text: a line feed is written as CR LF, and positions count both bytes.
    FILE *t = fopen("pos.txt", "w+");
    check(fputs("ab\ncd\n", t) >= 0, "fputs text");
#ifdef CRT_FILES_BREAK
    // Control: the position without the CRs is wrong, so check 17 fails.
    check(_ftelli64(t) == 6, "text position counts the CR");
#else
    check(_ftelli64(t) == 8, "text position counts the CR");
#endif
    check(_fseeki64(t, 0, SEEK_SET) == 0 && fgets(line, sizeof line, t) && same(line, "ab\n"),
          "text line");
    check(_ftelli64(t) == 4, "text position after a line");
    check(fclose(t) == 0, "fclose text");

    // rename moves the bytes; the old name is gone afterwards.
    check(rename("pos.bin", "moved.bin") == 0, "rename");
    check(fopen("pos.bin", "rb") == NULL, "old name gone");
    check(rename("moved.bin", "pos.txt") == -1, "rename onto existing refused");
    FILE *m = fopen("moved.bin", "rb");
    check(m && fgets(line, sizeof line, m) && same(line, "line one\n"), "renamed bytes");
    if (m) fclose(m);

    // remove deletes once.
    check(remove("moved.bin") == 0, "remove");
    check(remove("moved.bin") == -1, "remove twice refused");
    check(fopen("moved.bin", "rb") == NULL, "removed file gone");
    check(remove("pos.txt") == 0, "remove text");

    printf("crt_files: %u of %u checks passed\n", passed, total);
    return (int)first;
}
