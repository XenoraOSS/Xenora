// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once

using Dword = unsigned int;
using Handle = unsigned long long;

extern "C" __declspec(dllimport) Handle CreateFileW(const wchar_t *, Dword, Dword, void *, Dword, Dword, Handle);
extern "C" __declspec(dllimport) int SetFilePointerEx(Handle, long long, long long *, Dword);
extern "C" __declspec(dllimport) int WriteFile(Handle, const void *, Dword, Dword *, void *);
extern "C" __declspec(dllimport) int CloseHandle(Handle);

inline bool append_event(char event) {
    constexpr Dword generic_write = 0x40000000;
    constexpr Dword open_always = 4;
    constexpr Dword file_end = 2;
    const Handle file = CreateFileW(L"C:\\msvc-crt-lifecycle.bin", generic_write, 0, nullptr,
                                    open_always, 0, 0);
    if (file == 0 || file == ~0ULL) {
        return false;
    }
    Dword written = 0;
    const bool appended = SetFilePointerEx(file, 0, nullptr, file_end) &&
                          WriteFile(file, &event, 1, &written, nullptr) && written == 1;
    const bool closed = CloseHandle(file) != 0;
    return appended && closed;
}
