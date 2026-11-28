/* SPDX-License-Identifier: GPL-3.0-or-later */

typedef unsigned int dword;
typedef unsigned long long ulong_ptr;

#if TEST_WIDE
typedef __WCHAR_TYPE__ unit;
#define TEXT(value) L##value
#define FORMAT FormatMessageW
#else
typedef char unit;
#define TEXT(value) value
#define FORMAT FormatMessageA
#endif

__declspec(dllimport) dword FORMAT(
    dword, const void *, dword, dword, unit *, dword, const ulong_ptr *);
__declspec(dllimport) dword GetLastError(void);
__declspec(dllimport) __declspec(noreturn) void ExitProcess(dword);

enum {
    FROM_STRING = 0x400,
    IGNORE_INSERTS = 0x200,
    ARGUMENT_ARRAY = 0x2000,
    ERROR_INSUFFICIENT_BUFFER = 122,
};

static int checks_message(dword flags, const unit *format, const unit *expected,
                          dword length, const ulong_ptr *arguments) {
    unit buffer[64];
    for (dword i = 0; i <= length + 1; ++i)
        buffer[i] = (unit)0x5a;
    dword count = FORMAT(FROM_STRING | flags, format, 0, 0, buffer, length + 1, arguments);
    if (count != length || buffer[length + 1] != (unit)0x5a)
        return 0;
    for (dword i = 0; i <= length; ++i)
        if (buffer[i] != expected[i])
            return 0;
    return 1;
}

#define CHECK_MESSAGE(flags, format, expected, args) \
    checks_message(flags, TEXT(format), TEXT(expected), \
                   (dword)(sizeof(TEXT(expected)) / sizeof(unit) - 1), args)

void entry(void) {
    dword failures = 0;
    if (!CHECK_MESSAGE(0, "prompt%0ignored", "prompt", 0))
        failures |= 1;
    if (!CHECK_MESSAGE(0, "A%%B%bC%.D%!E%rF%tG%nH%q", "A%B C.D!E\rF\tG\r\nHq", 0))
        failures |= 2;
    const unit argument[] = TEXT("value");
    const ulong_ptr arguments[] = {(ulong_ptr)argument};
    if (!CHECK_MESSAGE(ARGUMENT_ARRAY, "[%1!s!]", "[value]", arguments))
        failures |= 4;
    if (!CHECK_MESSAGE(IGNORE_INSERTS, "%1!s!%tword%0discard", "%1!s!\tword", 0))
        failures |= 8;

    unit short_buffer[4];
    if (FORMAT(FROM_STRING, TEXT("done%0ignored"), 0, 0, short_buffer, 4, 0) != 0 ||
        GetLastError() != ERROR_INSUFFICIENT_BUFFER)
        failures |= 16;
    ExitProcess(failures);
}
