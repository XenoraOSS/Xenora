/* SPDX-License-Identifier: GPL-3.0-or-later */

typedef unsigned long long u64;
typedef __WCHAR_TYPE__ wide;

enum { STANDARD_SNPRINTF_BEHAVIOR = 2, OUTPUT_LIMIT = 1048576 };
__declspec(dllimport) __declspec(noreturn) void ExitProcess(unsigned int);

#if TEST_CASE != 2
__declspec(dllimport) int __stdio_common_vsprintf(
    u64, char *, u64, const char *, void *, __builtin_ms_va_list);

static int narrow_size(const char *format, ...) {
    __builtin_ms_va_list args;
    __builtin_ms_va_start(args, format);
    int size = __stdio_common_vsprintf(STANDARD_SNPRINTF_BEHAVIOR, 0, 0, format, 0, args);
    __builtin_ms_va_end(args);
    return size;
}
#endif

#if TEST_CASE != 1
__declspec(dllimport) int __stdio_common_vswprintf(
    u64, wide *, u64, const wide *, void *, __builtin_ms_va_list);

static int wide_size(const wide *format, ...) {
    __builtin_ms_va_list args;
    __builtin_ms_va_start(args, format);
    int size = __stdio_common_vswprintf(STANDARD_SNPRINTF_BEHAVIOR, 0, 0, format, 0, args);
    __builtin_ms_va_end(args);
    return size;
}
#endif

void entry(void) {
#if TEST_CASE == 1
    int size = narrow_size("%1048576d%%", 1);
    ExitProcess(size == OUTPUT_LIMIT + 1 ? 71 : 81);
#elif TEST_CASE == 2
    int size = wide_size(L"%1048576d%%", 1);
    ExitProcess(size == OUTPUT_LIMIT + 1 ? 72 : 82);
#else
    if (narrow_size("%1048575d%%", 1) != OUTPUT_LIMIT)
        ExitProcess(61);
    if (wide_size(L"%1048575d%%", 1) != OUTPUT_LIMIT)
        ExitProcess(62);
    ExitProcess(0);
#endif
}
