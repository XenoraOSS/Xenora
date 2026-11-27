// SPDX-License-Identifier: GPL-3.0-or-later

// Original PE32+ guest that exercises the legacy three-argument
// advapi32.dll!RegOpenKeyW import against the synthetic empty registry.

typedef unsigned int u32;
typedef unsigned long long u64;
typedef unsigned short u16;

#define IMPORT __declspec(dllimport)

IMPORT u32 RegOpenKeyW(u64 key, const u16 *subkey, u64 *output);
IMPORT void __declspec(noreturn) ExitProcess(u32 status);

#define HKCU 0xffffffff80000001ull
#define ERROR_SUCCESS 0u
#define ERROR_FILE_NOT_FOUND 2u
#define ERROR_INVALID_HANDLE 6u
#define ERROR_INVALID_PARAMETER 87u
#define UNWRITTEN 0x1122334455667788ull

#define REQUIRE(step, condition) \
    do {                         \
        if (!(condition))        \
            ExitProcess(step);   \
    } while (0)

static const u16 empty[] = {0};
static const u16 named[] = {'S', 'o', 'f', 't', 'w', 'a', 'r', 'e', '\\',
                            'X', 'e', 'n', 'o', 'r', 'a', 0};

void entry(void) {
    u64 output = UNWRITTEN;

    // 1..4: a null or empty subkey reopens the predefined root itself.
    REQUIRE(1, RegOpenKeyW(HKCU, 0, &output) == ERROR_SUCCESS);
    REQUIRE(2, output == HKCU);
    output = UNWRITTEN;
    REQUIRE(3, RegOpenKeyW(HKCU, empty, &output) == ERROR_SUCCESS);
    REQUIRE(4, output == HKCU);

    // 5..7: the empty synthetic registry has no named subkeys, and failure
    // must preserve the caller's output value.
    output = UNWRITTEN;
    REQUIRE(5, RegOpenKeyW(HKCU, named, &output) == ERROR_FILE_NOT_FOUND);
    REQUIRE(6, output == UNWRITTEN);
    REQUIRE(7, RegOpenKeyW(HKCU, 0, 0) == ERROR_INVALID_PARAMETER);

    // 8..9: a value sharing only low bits with a predefined root is foreign.
    output = UNWRITTEN;
    REQUIRE(8, RegOpenKeyW(0x1234567880000001ull, 0, &output) == ERROR_INVALID_HANDLE);
    REQUIRE(9, output == UNWRITTEN);

    ExitProcess(0);
}
