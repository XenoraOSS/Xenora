// SPDX-License-Identifier: GPL-3.0-or-later
// Original guest checks the UCRT stream-buffer pointer contract.
using u32 = unsigned int;
using uptr = unsigned long long;
extern "C" {
__declspec(dllimport) void *__acrt_iob_func(u32);
__declspec(dllimport) int _get_stream_buffer_pointers(void *, char ***, char ***, int **);
__declspec(dllimport) int fputc(int, void *);
__declspec(dllimport) int fflush(void *);
__declspec(dllimport) int *_errno();
__declspec(dllimport) __declspec(noreturn) void ExitProcess(u32);
}
extern "C" void entry() {
#ifdef XENORA_BAD_OUTPUT
    void *out = __acrt_iob_func(1);
    _get_stream_buffer_pointers(out, reinterpret_cast<char ***>(~uptr(0)), nullptr, nullptr);
    ExitProcess(7);
#elif defined(XENORA_INVALID)
    char **base = nullptr;
    _get_stream_buffer_pointers(reinterpret_cast<void *>(0x1234), &base, nullptr, nullptr);
    ExitProcess(8);
#else
    void *out = __acrt_iob_func(1);
    char **base = nullptr, **next = nullptr;
    int *count = nullptr;
    if (_get_stream_buffer_pointers(out, &base, &next, &count) != 0) ExitProcess(1);
    if (!base || !next || !count || base == next || !*base || *base != *next || *count) ExitProcess(2);
    if (reinterpret_cast<uptr>(base) != reinterpret_cast<uptr>(out) + 8 ||
        reinterpret_cast<uptr>(next) != reinterpret_cast<uptr>(out) ||
        reinterpret_cast<uptr>(count) != reinterpret_cast<uptr>(out) + 16 ||
        reinterpret_cast<uptr>(*base) != reinterpret_cast<uptr>(out) + 28) ExitProcess(10);
    void *error = __acrt_iob_func(2); char **error_base = nullptr;
    if (_get_stream_buffer_pointers(error, &error_base, nullptr, nullptr) != 0 || error_base == base || *error_base == *base) ExitProcess(11);
    char **base2 = nullptr, **next2 = nullptr; int *count2 = nullptr;
    if (_get_stream_buffer_pointers(out, &base2, &next2, &count2) != 0 || base2 != base || next2 != next || count2 != count) ExitProcess(3);
    if (_get_stream_buffer_pointers(out, nullptr, nullptr, nullptr) != 0) ExitProcess(4);
    if (fputc('X', out) != 'X' || fflush(out) != 0 || !*base || *base != *next || *count) ExitProcess(5);
    if (_get_stream_buffer_pointers(nullptr, &base2, nullptr, nullptr) != 22 || *_errno() != 22 || base2 != base) ExitProcess(6);
    ExitProcess(0);
#endif
}
