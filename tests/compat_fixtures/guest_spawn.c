// SPDX-License-Identifier: GPL-3.0-or-later
//
// A hosted guest that re-executes itself through `_spawnve` in _P_WAIT mode
// (issue #1040). The parent writes a file on the shared drive, spawns the
// child with "child" as its first argument and reports the status the spawn
// returns. The child reads the parent's file - proving the machine-wide
// filesystem is shared - and exits 42.
//
// Every expected output line is stated next to the printf that produces it.

#include <stdio.h>
#include <string.h>
#include <process.h>
#include <fcntl.h>
#include <io.h>
#include <windows.h>

enum { inherited_fd = 7 };

static int read_byte(int fd, char expected) {
    char byte = 0;
    return _read(fd, &byte, 1) == 1 && byte == expected;
}

static int wrapped_child(const char *mode) {
    if (strcmp(mode, "wrapped-close") == 0) {
        if (!read_byte(inherited_fd, 'B') || _close(inherited_fd) != 0)
            return 21;
        puts("wrapped child: read B and closed descriptor");
        return 41;
    }

    if (!read_byte(inherited_fd, 'D'))
        return 22;
    puts("wrapped child: read D and left descriptor open");
    return 42;
}

static int wrapped_parent(const char *program) {
    FILE *file = fopen("spawn-wrapped.txt", "wb");
    if (!file || fputs("ABCDE", file) == EOF || fclose(file) != 0)
        return 11;
    HANDLE handle = CreateFileA("spawn-wrapped.txt", GENERIC_READ, FILE_SHARE_READ,
                                NULL, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL);
    if (handle == INVALID_HANDLE_VALUE)
        return 12;
    int fd = _open_osfhandle((intptr_t)handle, _O_RDONLY | _O_BINARY);
    if (fd < 0) {
        CloseHandle(handle);
        return 13;
    }
    if (!read_byte(fd, 'A') || _dup2(fd, inherited_fd) < 0)
        return 14;

    const char *child_argv[] = { program, "wrapped-close", NULL };
    if (_spawnve(_P_WAIT, program, child_argv, NULL) != 41)
        return 15;
    if (!read_byte(fd, 'C'))
        return 16;
    puts("wrapped parent: read C after child close");

    int duplicate = _dup(inherited_fd);
    if (duplicate < 0 || _close(fd) != 0)
        return 17;
    child_argv[1] = "wrapped-exit";
    if (_spawnve(_P_WAIT, program, child_argv, NULL) != 42)
        return 18;
    if (_close(inherited_fd) != 0 || !read_byte(duplicate, 'E'))
        return 19;
    puts("wrapped parent: read E through surviving duplicate");
    if (_close(duplicate) != 0)
        return 20;

    char byte = 0;
    DWORD read = 0;
    if (ReadFile(handle, &byte, 1, &read, NULL) || GetLastError() != ERROR_INVALID_HANDLE)
        return 23;
    puts("wrapped parent: final close released the handle");
    return 0;
}

int main(int argc, char **argv) {
    if (argc >= 2) {
        if (strcmp(argv[1], "wrapped-parent") == 0)
            return wrapped_parent(argv[0]);
        if (strcmp(argv[1], "wrapped-close") == 0 || strcmp(argv[1], "wrapped-exit") == 0)
            return wrapped_child(argv[1]);
    }
    if (argc >= 2 && strcmp(argv[1], "child") == 0) {
        FILE *in = fopen("spawn-shared.txt", "r");
        if (in == NULL) {
            printf("guest spawn: child found no shared file\n");
            return 1;
        }
        char buffer[64] = {0};
        const char *read = fgets(buffer, sizeof buffer, in);
        fclose(in);
        if (read == NULL || strcmp(buffer, "parent data") != 0) {
            printf("guest spawn: child read the wrong bytes\n");
            return 1;
        }
        /* expected: "guest spawn: child ran\n" */
        printf("guest spawn: child ran\n");
        return 42;
    }

    FILE *out = fopen("spawn-shared.txt", "w");
    if (out == NULL) {
        printf("guest spawn: parent could not write\n");
        return 1;
    }
    fputs("parent data", out);
    fclose(out);

    const char *child_argv[3];
    child_argv[0] = argv[0];
    child_argv[1] = "child";
    child_argv[2] = NULL;
    intptr_t status = _spawnve(_P_WAIT, argv[0], child_argv, NULL);
    /* expected: "guest spawn: child status 42\n" */
    printf("guest spawn: child status %d\n", (int)status);
    return status == 42 ? 0 : 1;
}
