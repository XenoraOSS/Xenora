/* SPDX-License-Identifier: GPL-3.0-or-later */

typedef unsigned int dword;
typedef unsigned short wide;

__declspec(dllimport) int MultiByteToWideChar(dword, dword, const char *, int, wide *, int);
__declspec(dllimport) dword GetLastError(void);
__declspec(dllimport) __declspec(noreturn) void ExitProcess(dword);

enum {
    CP_UTF8 = 65001,
    MB_ERR_INVALID_CHARS = 8,
    ERROR_NO_UNICODE_TRANSLATION = 1113,
    REPLACEMENT = 0xfffd,
    GUARD = 0x5a5a,
};

static int check(const unsigned char *input, int bytes, const wide *expected, int units) {
    wide output[8];
    for (int i = 0; i < 8; ++i)
        output[i] = GUARD;
    if (MultiByteToWideChar(CP_UTF8, 0, (const char *)input, bytes, 0, 0) != units ||
        MultiByteToWideChar(CP_UTF8, 0, (const char *)input, bytes, output, units) != units ||
        output[units] != GUARD)
        return 0;
    for (int i = 0; i < units; ++i)
        if (output[i] != expected[i])
            return 0;
    return 1;
}

#define CHECK(input, output) \
    check(input, (int)sizeof(input), output, (int)(sizeof(output) / sizeof(wide)))

void entry(void) {
    // Unicode 17, section 3.9.6. These ranges cannot begin a valid scalar.
    const unsigned char overlong[] = {0xc0, 0xaf, 'A'};
    const unsigned char surrogate[] = {0xed, 0xa0, 0x80, 'B'};
    const unsigned char too_high[] = {0xf4, 0x91, 0x92, 0x93, 'C'};
    const unsigned char truncated[] = {0xe1, 0x80, 'D'};
    const unsigned char supplementary[] = {0xf0, 0x9f, 0x98, 0x80};
    const unsigned char nul_input[] = {0xe1, 0x80, 0, 'E'};
    const wide overlong_expected[] = {REPLACEMENT, REPLACEMENT, 'A'};
    const wide surrogate_expected[] = {REPLACEMENT, REPLACEMENT, REPLACEMENT, 'B'};
    const wide too_high_expected[] = {REPLACEMENT, REPLACEMENT, REPLACEMENT, REPLACEMENT, 'C'};
    const wide truncated_expected[] = {REPLACEMENT, 'D'};
    const wide supplementary_expected[] = {0xd83d, 0xde00};
    const wide terminated_expected[] = {REPLACEMENT, 0};
    const wide counted_expected[] = {REPLACEMENT, 0, 'E'};

    dword failures = 0;
    if (!CHECK(overlong, overlong_expected))
        failures |= 1;
    if (!CHECK(surrogate, surrogate_expected))
        failures |= 2;
    if (!CHECK(too_high, too_high_expected))
        failures |= 4;
    if (!CHECK(truncated, truncated_expected) || !CHECK(supplementary, supplementary_expected))
        failures |= 8;
    if (!check(nul_input, -1, terminated_expected, 2) || !CHECK(nul_input, counted_expected))
        failures |= 16;

    wide strict[4] = {GUARD, GUARD, GUARD, GUARD};
    if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, (const char *)surrogate,
                            sizeof(surrogate), strict, 4) != 0 ||
        GetLastError() != ERROR_NO_UNICODE_TRANSLATION ||
        strict[0] != GUARD || strict[1] != GUARD || strict[2] != GUARD || strict[3] != GUARD)
        failures |= 32;
    ExitProcess(failures);
}
