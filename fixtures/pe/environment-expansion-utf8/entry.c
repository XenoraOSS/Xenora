/* SPDX-License-Identifier: GPL-3.0-or-later */

typedef unsigned int dword;
typedef __WCHAR_TYPE__ wide;
__declspec(dllimport) int SetEnvironmentVariableW(const wide *, const wide *);
__declspec(dllimport) dword ExpandEnvironmentStringsA(const char *, char *, dword);
__declspec(dllimport) dword ExpandEnvironmentStringsW(const wide *, wide *, dword);
__declspec(dllimport) __declspec(noreturn) void ExitProcess(dword);

static int check_a(const char *input, const char *expected, dword required) {
    char output[64];
    for (dword i = 0; i <= required; ++i)
        output[i] = '!';
    if (ExpandEnvironmentStringsA(input, 0, 0) != required ||
        ExpandEnvironmentStringsA(input, output, required - 1) != required ||
        ExpandEnvironmentStringsA(input, output, required + 1) != required ||
        output[required] != '!')
        return 0;
    for (dword i = 0; i < required; ++i)
        if (output[i] != expected[i])
            return 0;
    return 1;
}

static int check_w(const wide *input, const wide *expected, dword required) {
    wide output[64];
    for (dword i = 0; i <= required; ++i)
        output[i] = L'!';
    if (ExpandEnvironmentStringsW(input, 0, 0) != required ||
        ExpandEnvironmentStringsW(input, output, required - 1) != required ||
        ExpandEnvironmentStringsW(input, output, required) != required ||
        output[required] != L'!')
        return 0;
    for (dword i = 0; i < required; ++i)
        if (output[i] != expected[i])
            return 0;
    return 1;
}

#define CHECK_A(input, expected) check_a(input, expected, sizeof(expected))
#define CHECK_W(input, expected) check_w(input, expected, sizeof(expected) / sizeof(wide))

void entry(void) {
    if (!SetEnvironmentVariableW(L"XENORA_TAG", L"\u20ac\U0001f600") ||
        !SetEnvironmentVariableW(L"XENORA_\u00e9", L"accent") ||
        !SetEnvironmentVariableW(L"XENORA_\U0001f600", L"\u20ac\U0001f600"))
        ExitProcess(32);

    dword failures = 0;
    // ASCII spelling matches independently of encoding the inserted value.
    if (!CHECK_A("%xenora_tag%", "\xe2\x82\xac\xf0\x9f\x98\x80"))
        failures |= 1;
    // UTF-8 name, adjacent UTF-8 literal, and an unresolved token.
    if (!CHECK_A("\xc3\xa9:%xenora_\xc3\xa9%/%unknown%",
                 "\xc3\xa9:accent/%unknown%"))
        failures |= 2;
    if (!CHECK_W(L"%xenora_\U0001f600%/%unknown%", L"\u20ac\U0001f600/%unknown%"))
        failures |= 4;
    if (!CHECK_W(L"%xenora_tag%", L"\u20ac\U0001f600"))
        failures |= 8;
    // Xenora preserves raw literal bytes and unresolved names.
    if (!CHECK_A("\x80:%unknown%", "\x80:%unknown%"))
        failures |= 16;
    ExitProcess(failures);
}
