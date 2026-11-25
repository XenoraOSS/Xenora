// SPDX-License-Identifier: GPL-3.0-or-later
//
// Hosted checks for the Windows error, environment and system information
// services (package 9 of #459). Built with mingw-w64, every call is an
// import of kernel32. A wrong check prints its number and the program exits
// with it. With CRT_WINSYS_BREAK one expectation is wrong on purpose.

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
    char buffer[512];
    wchar_t wide[256];

    // 1. FormatMessageA answers the system text of an error, newline included.
    DWORD wrote = FormatMessageA(FORMAT_MESSAGE_FROM_SYSTEM, NULL,
                                 ERROR_FILE_NOT_FOUND, 0, buffer, 256, NULL);
    CHECK("format written", wrote == strlen("The system cannot find the file specified.\r\n"));
    CHECK("format text", strcmp(buffer, "The system cannot find the file specified.\r\n") == 0);

    // 2. The wide form answers the same text in wide units.
    wrote = FormatMessageW(FORMAT_MESSAGE_FROM_SYSTEM, NULL,
                           ERROR_INVALID_HANDLE, 0, wide, 256, NULL);
    CHECK("format wide written", wrote == strlen("The handle is invalid.\r\n"));
    CHECK("format wide text", wide[0] == L'T' && wide[1] == L'h');

    // 3. An unknown identifier fails with the missing-message error.
    SetLastError(0);
    CHECK("format unknown",
          FormatMessageA(FORMAT_MESSAGE_FROM_SYSTEM, NULL, 999999, 0, buffer, 256, NULL) == 0
              && GetLastError() == ERROR_MR_MID_NOT_FOUND);

    // 4. The allocate form hands memory LocalFree releases.
    char *allocated = NULL;
    wrote = FormatMessageA(FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_ALLOCATE_BUFFER,
                           NULL, ERROR_ACCESS_DENIED, 0, (char *)&allocated, 0, NULL);
    CHECK("format allocate", wrote != 0 && allocated != NULL);
    CHECK("format allocate text", strcmp(allocated, "Access is denied.\r\n") == 0);
    CHECK("format allocate freed", LocalFree(allocated) == NULL);

    // 5. The error mode exchanges and remembers.
    CHECK("error mode default", SetErrorMode(SEM_FAILCRITICALERRORS) == 0);
    CHECK("error mode exchange", SetErrorMode(0) == SEM_FAILCRITICALERRORS);

    // 6. Expansion over the empty environment is the documented identity.
    wrote = ExpandEnvironmentStringsA("%TEMP%\\file.txt", buffer, 64);
    CHECK("expand count", wrote == strlen("%TEMP%\\file.txt") + 1);
    CHECK("expand text", strcmp(buffer, "%TEMP%\\file.txt") == 0);
    CHECK("expand small", ExpandEnvironmentStringsA("%TEMP%\\file.txt", buffer, 4) == 16);

    // 7. The environment block is empty, owned and freeable.
    wchar_t *block = GetEnvironmentStringsW();
    CHECK("env block", block != NULL && block[0] == 0 && block[1] == 0);
    CHECK("env freed", FreeEnvironmentStringsW(block) != 0);

    // 8. The version is the simulated Windows 10 era system.
    OSVERSIONINFOA version;
    memset(&version, 0, sizeof version);
    version.dwOSVersionInfoSize = sizeof version;
    CHECK("version", GetVersionExA(&version) != 0);
    CHECK("version numbers",
          version.dwMajorVersion == 10 && version.dwBuildNumber == 19041);

    // 9. The directories of the simulated installation.
    wrote = GetWindowsDirectoryA(buffer, 64);
    CHECK("windows dir", wrote == 10 && strcmp(buffer, "c:\\windows") == 0);
    wrote = GetSystemDirectoryA(buffer, 64);
    CHECK("system dir", wrote == 19 && strcmp(buffer, "c:\\windows\\system32") == 0);
    CHECK("windows dir small", GetWindowsDirectoryA(buffer, 4) == 11);

    // 10. The computer name, counted without its terminator.
    DWORD size = 64;
    CHECK("computer name", GetComputerNameA(buffer, &size) != 0);
    CHECK("computer name text", size == 6 && strcmp(buffer, "XENORA") == 0);

    // 11. Processors and affinity: one processor in one group.
    CHECK("active processors", GetActiveProcessorCount(0) == 1);
    DWORD_PTR process_mask = 0, system_mask = 0;
    CHECK("affinity",
          GetProcessAffinityMask(GetCurrentProcess(), &process_mask, &system_mask) != 0
              && process_mask == 1 && system_mask == 1);
    CHECK("set affinity", SetThreadAffinityMask(GetCurrentThread(), 1) != 0);

#ifdef CRT_WINSYS_BREAK
    CHECK("the control fails", wrote == 999);
#endif

    if (failures) {
        printf("crt-winsys: %u/%u checks failed, first %u\n", failures, checks, failed);
        return failed;
    }
    printf("crt-winsys: %u/%u checks passed\n", checks, checks);
    return 0;
}
