// SPDX-License-Identifier: GPL-3.0-or-later
//
// A hosted C program for the third part of #493: the descriptor layer.
// _open, _read, _write, _lseek, _close, _fileno, _isatty, _access and
// _unlink, and the state a descriptor shares with its stream. Every
// expected value follows from the bytes this program writes, the Microsoft
// pages for each function and the <fcntl.h> values of mingw-w64. It prints
// how many checks passed and returns the number of the first that failed,
// or 0.
#include <stdio.h>
#include <io.h>
#include <fcntl.h>
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
    char buf[32];

    // Write in binary, read back through a seek.
    int fd = _open("fd.bin", _O_RDWR | _O_CREAT | _O_TRUNC | _O_BINARY, 0);
    check(fd >= 3, "_open binary");
    check(_write(fd, "abcdef", 6) == 6, "_write");
    check(_lseek(fd, 0, SEEK_SET) == 0, "_lseek start");
    memset(buf, 0, sizeof buf);
    check(_read(fd, buf, 8) == 6 && same(buf, "abcdef"), "_read");
    check(_read(fd, buf, 8) == 0, "end is a zero count");
    check(_lseek(fd, -2, SEEK_END) == 4, "_lseek from end");
    check(_lseek(fd, -1, SEEK_CUR) == 3, "_lseek from current");
    check(_lseek(fd, -100, SEEK_SET) == -1 && errno == EINVAL, "before start refused");
    check(_close(fd) == 0, "_close");
    errno = 0;
    check(_close(fd) == -1 && errno == EBADF, "closed fd is EBADF");

    // Text mode translates both ways.
    fd = _open("fd.txt", _O_RDWR | _O_CREAT | _O_TRUNC | _O_TEXT, 0);
    check(fd >= 3, "_open text");
    check(_write(fd, "x\ny", 3) == 3, "text write");
    check(_lseek(fd, 0, SEEK_END) == 4, "text end counts the CR");
    check(_lseek(fd, 0, SEEK_SET) == 0, "text seek back");
    memset(buf, 0, sizeof buf);
    check(_read(fd, buf, 8) == 3 && same(buf, "x\ny"), "text read");
    check(_close(fd) == 0, "text close");
    fd = _open("fd.txt", _O_RDONLY | _O_BINARY, 0);
    memset(buf, 0, sizeof buf);
    check(_read(fd, buf, 8) == 4 && buf[1] == '\r', "the drive holds CR LF");
    check(_close(fd) == 0, "binary close");

    // A stream and its descriptor share the file's state.
    FILE *f = fopen("shared.bin", "w+b");
    check(f != NULL, "fopen shared");
    check(fwrite("0123456789", 1, 10, f) == 10, "stream write");
    fclose(f);
    f = fopen("shared.bin", "r+b");
    int shared = _fileno(f);
    check(shared >= 3, "_fileno");
    check(fgets(buf, 4, f) && same(buf, "012"), "stream reads three");
    check(_write(shared, "ZZ", 2) == 2, "descriptor writes over 34");
    memset(buf, 0, sizeof buf);
    check(fgets(buf, 8, f) && same(buf, "56789"), "stream position moved with the descriptor");
    check(_fileno(f) == shared, "same stream, same descriptor");
    check(_close(shared) == 0, "close shared descriptor");

    // _fileno of the standard streams, _isatty is never a terminal here.
    check(_fileno(stdin) == 0 && _fileno(stdout) == 1 && _fileno(stderr) == 2, "standard fds");
    check(_isatty(0) == 0 && _isatty(1) == 0 && _isatty(2) == 0, "nothing is a terminal");

    // Failures carry errno.
    errno = 0;
    check(_open("missing.bin", _O_RDONLY, 0) == -1 && errno == ENOENT, "missing is ENOENT");
    errno = 0;
    check(_open("shared.bin", _O_CREAT | _O_EXCL, 0) == -1 && errno == EEXIST, "existing with EXCL is EEXIST");
    errno = 0;
    check(_access("shared.bin", 0) == 0 && _access("shared.bin", 6) == 0, "_access existing");
    errno = 0;
    check(_access("missing.bin", 0) == -1 && errno == ENOENT, "_access missing");
    check(_unlink("shared.bin") == 0, "_unlink");
    errno = 0;
    check(_unlink("shared.bin") == -1 && errno == ENOENT, "_unlink twice");
    check(_unlink("fd.bin") == 0 && _unlink("fd.txt") == 0, "cleanup");

#ifdef CRT_FDS_BREAK
    // Control: a wrong expectation must be detected.
    check(1 == 2, "control");
#endif
    printf("crt_fds: %u of %u checks passed\n", passed, total);
    return (int)first;
}
