// SPDX-License-Identifier: GPL-3.0-or-later

typedef void (*atexit_callback)(void);

#define IMPORT __declspec(dllimport)
#define CHECK(condition, code) do { if (!(condition)) ExitProcess(code); } while (0)

IMPORT int _crt_atexit(atexit_callback callback);
IMPORT void _c_exit(void);
IMPORT void _cexit(void);
IMPORT int _open(const char *name, int flags, int mode);
IMPORT int _close(int fd);
IMPORT long _lseek(int fd, long offset, int origin);
IMPORT int _read(int fd, void *bytes, unsigned int length);
IMPORT int _write(int fd, const void *bytes, unsigned int length);
IMPORT __declspec(noreturn) void ExitProcess(unsigned int status);

enum {
    O_RDWR = 2,
    O_CREAT = 0x100,
    O_BINARY = 0x8000,
    S_READ_WRITE = 0x180,
    SEEK_SET = 0,
};

static volatile unsigned int callback_order;

static void first(void) {
    callback_order = callback_order * 10 + 1;
}

static void second(void) {
    callback_order = callback_order * 10 + 2;
}

void entry(void) {
    const int fd = _open("crt-cleanup.bin", O_RDWR | O_CREAT | O_BINARY, S_READ_WRITE);
    CHECK(fd >= 3, 10);
    CHECK(_write(fd, "O", 1) == 1, 11);
    CHECK(_crt_atexit(first) == 0, 12);
    CHECK(_crt_atexit(second) == 0, 13);

    // Quick cleanup returns without running or consuming either registration.
    _c_exit();
    CHECK(callback_order == 0, 14);
    CHECK(_write(fd, "K", 1) == 1, 15);
    CHECK(_lseek(fd, 0, SEEK_SET) == 0, 16);
    char bytes[2] = {0, 0};
    CHECK(_read(fd, bytes, sizeof(bytes)) == sizeof(bytes), 17);
    CHECK(bytes[0] == 'O' && bytes[1] == 'K', 18);
    CHECK(_close(fd) == 0, 19);

    _cexit();
    CHECK(callback_order == 21, 20);
    _cexit();
    CHECK(callback_order == 21, 21);
    ExitProcess(0);
}
