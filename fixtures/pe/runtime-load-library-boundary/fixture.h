/* SPDX-License-Identifier: GPL-3.0-or-later */
#ifndef XENORA_RUNTIME_LOAD_LIBRARY_BOUNDARY_H
#define XENORA_RUNTIME_LOAD_LIBRARY_BOUNDARY_H

typedef unsigned int dword;
typedef void *handle;

__declspec(dllimport) handle GetStdHandle(dword);
__declspec(dllimport) int WriteConsoleA(handle, const void *, dword, dword *, void *);
__declspec(dllimport) __declspec(noreturn) void ExitProcess(dword);

static int write_marker(const char *text, dword length) {
    dword written = 0;
    return WriteConsoleA(GetStdHandle((dword)-11), text, length, &written, 0) && written == length;
}

#define MARKER(text) write_marker(text, (dword)(sizeof(text) - 1))

#endif
