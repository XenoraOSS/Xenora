// SPDX-License-Identifier: GPL-3.0-or-later
//
// Hosted checks for the Windows file services on the synthetic drive
// (package 8, second part, of #459). Built with mingw-w64, every call is an
// import of kernel32. A wrong check prints its number and the program exits
// with it. With CRT_WINFILES_BREAK one expectation is wrong on purpose.

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
    char buffer[256];
    DWORD count = 0;
    BY_HANDLE_FILE_INFORMATION info;
    WIN32_FIND_DATAA data;

    // 1. A directory and a file with content, through the wide forms.
    CHECK("mkdir", CreateDirectoryW(L"c:\\dir", NULL) != 0);
    HANDLE file = CreateFileW(L"c:\\dir\\a.txt", GENERIC_WRITE, 0, NULL,
                              CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    CHECK("create", file != INVALID_HANDLE_VALUE);
    CHECK("write", WriteFile(file, "drive-bytes", 11, &count, NULL) != 0 && count == 11);

    // 2. The information and the pointer of the open file.
    CHECK("info", GetFileInformationByHandle(file, &info) != 0);
    CHECK("info size", info.nFileSizeLow == 11 && info.nFileSizeHigh == 0);
    CHECK("seek", SetFilePointer(file, 5, NULL, FILE_BEGIN) == 5);
    CloseHandle(file);

    // 3. CopyFileA copies inside the drive and honors fail-if-exists.
    CHECK("copy", CopyFileA("c:\\dir\\a.txt", "c:\\dir\\b.txt", TRUE) != 0);
    HANDLE verify = CreateFileW(L"c:\\dir\\b.txt", GENERIC_READ, 0, NULL,
                                OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL);
    CHECK("verify open", verify != INVALID_HANDLE_VALUE);
    CHECK("verify read", ReadFile(verify, buffer, 32, &count, NULL) != 0 && count == 11);
    CHECK("verify content", memcmp(buffer, "drive-bytes", 11) == 0);
    CloseHandle(verify);
    SetLastError(0);
    CHECK("copy exists fails",
          CopyFileA("c:\\dir\\a.txt", "c:\\dir\\b.txt", TRUE) == 0
              && GetLastError() == ERROR_FILE_EXISTS);
    SetLastError(0);
    CHECK("copy self fails",
          CopyFileA("c:\\dir\\a.txt", "c:\\dir\\a.txt", FALSE) == 0
              && GetLastError() == ERROR_SHARING_VIOLATION);

    // 4. MoveFileA moves the copy into a narrow-made subdirectory.
    CHECK("mkdir narrow", CreateDirectoryA("c:\\dir\\sub", NULL) != 0);
    CHECK("move", MoveFileA("c:\\dir\\b.txt", "c:\\dir\\sub\\moved.txt") != 0);
    verify = CreateFileW(L"c:\\dir\\sub\\moved.txt", GENERIC_READ, 0, NULL,
                         OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL);
    CHECK("moved there", verify != INVALID_HANDLE_VALUE);
    CloseHandle(verify);

    // 5. FindFirstFileA lists what stayed, in sorted order, then ends.
    HANDLE search = FindFirstFileA("c:\\dir\\*.txt", &data);
    CHECK("find first", search != INVALID_HANDLE_VALUE);
    CHECK("find name", strcmp(data.cFileName, "a.txt") == 0);
    SetLastError(0);
    CHECK("find end",
          FindNextFileA(search, &data) == 0
              && GetLastError() == ERROR_NO_MORE_FILES);
    CHECK("find close", FindClose(search) != 0);

    // 6. GetFullPathNameA resolves against the root and counts correctly.
    DWORD wrote = GetFullPathNameA("dir\\a.txt", 64, buffer, NULL);
    CHECK("full path", wrote == 12 && strcmp(buffer, "c:\\dir\\a.txt") == 0);
    CHECK("full path size", GetFullPathNameA("dir\\a.txt", 4, buffer, NULL) == 13);

    // 7. GetTempFileNameA answers unique existing names.
    UINT one = GetTempFileNameA("c:\\dir", "ab", 0, buffer);
    CHECK("temp one", one != 0 && buffer[0] != 0);
    char second[64];
    UINT two = GetTempFileNameA("c:\\dir", "ab", 0, second);
    CHECK("temp unique", two != 0 && two != one);

    // 8. SetFileAttributesW succeeds on a file, fails a missing one.
    CHECK("set attr", SetFileAttributesW(L"c:\\dir\\a.txt", FILE_ATTRIBUTE_NORMAL) != 0);
    CHECK("set attr missing",
          SetFileAttributesW(L"c:\\dir\\missing.txt", FILE_ATTRIBUTE_NORMAL) == 0);

#ifdef CRT_WINFILES_BREAK
    CHECK("the control fails", wrote == 999);
#endif

    if (failures) {
        printf("crt-winfiles: %u/%u checks failed, first %u\n", failures, checks, failed);
        return failed;
    }
    printf("crt-winfiles: %u/%u checks passed\n", checks, checks);
    return 0;
}
