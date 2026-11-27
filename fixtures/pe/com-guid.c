// SPDX-License-Identifier: GPL-3.0-or-later

// Original PE32+ guest that exercises the COM base surface: the per-thread
// apartment, the GUID constructors and parsers, the task allocator and the
// PROPVARIANT clear.
//
// The imports are deliberately split across the three names a program uses:
// `CoInitializeEx` from the API set, `CoUninitialize` from `ole32.dll` and the
// rest from the canonical `combase.dll`, so the run also proves the alias and
// compatibility-module resolution.

typedef unsigned char u8;
typedef unsigned short u16;
typedef unsigned int u32;
typedef unsigned long long u64;
typedef int i32;

#define IMPORT __declspec(dllimport)

typedef struct {
    u32 data1;
    u16 data2;
    u16 data3;
    u8 data4[8];
} GUID;

// The x64 PROPVARIANT: the type word, three reserved words, then a 16-byte
// union.
typedef struct {
    u16 vt;
    u16 reserved1;
    u16 reserved2;
    u16 reserved3;
    union {
        u64 value;
        void *pointer;
        u64 storage[2];
    } payload;
} PROPVARIANT;

IMPORT void __declspec(noreturn) ExitProcess(u32 status);
IMPORT i32 CoInitializeEx(void *reserved, u32 model);
IMPORT void CoUninitialize(void);
IMPORT i32 CoCreateGuid(GUID *guid);
IMPORT i32 StringFromGUID2(const GUID *guid, u16 *text, i32 capacity);
IMPORT i32 CLSIDFromString(const u16 *text, GUID *guid);
IMPORT void *CoTaskMemAlloc(u64 size);
IMPORT void CoTaskMemFree(void *block);
IMPORT void *CoTaskMemRealloc(void *block, u64 size);
IMPORT i32 CoCreateInstance(const GUID *clsid, void *outer, u32 context,
                            const GUID *iid, void **object);
IMPORT i32 PropVariantClear(PROPVARIANT *variant);

#define S_OK 0
#define S_FALSE 1
#define E_INVALIDARG ((i32)0x80070057)
#define E_POINTER ((i32)0x80004003)
#define RPC_E_CHANGED_MODE ((i32)0x80010106)
#define CO_E_CLASSSTRING ((i32)0x800401f3)
#define REGDB_E_CLASSNOTREG ((i32)0x80040154)

#define COINIT_MULTITHREADED 0x0
#define COINIT_APARTMENTTHREADED 0x2

#define VT_EMPTY 0
#define VT_I4 3
#define VT_LPSTR 30

#define GUID_TEXT_UNITS 39
#define BLOCK_BYTES 64u
#define BLOCK_GROWN 128u

#define REQUIRE(step, condition) \
    do {                         \
        if (!(condition))        \
            ExitProcess(step);   \
    } while (0)

static u16 text[GUID_TEXT_UNITS + 8];
static GUID created;
static GUID parsed;

static int guid_equal(const GUID *left, const GUID *right) {
    u32 index;
    if (left->data1 != right->data1 || left->data2 != right->data2 ||
        left->data3 != right->data3)
        return 0;
    for (index = 0; index < 8; index++)
        if (left->data4[index] != right->data4[index])
            return 0;
    return 1;
}

static void fill(u8 *block, u32 size, u8 value) {
    u32 index;
    for (index = 0; index < size; index++)
        block[index] = value;
}

static int filled(const u8 *block, u32 size, u8 value) {
    u32 index;
    for (index = 0; index < size; index++)
        if (block[index] != value)
            return 0;
    return 1;
}

static void clear_guid(GUID *guid) {
    u8 *bytes = (u8 *)guid;
    u32 index;
    for (index = 0; index < 16; index++)
        bytes[index] = 0;
}

void entry(void) {
    void *object;
    u8 *block;
    u8 *grown;
    PROPVARIANT variant;
    GUID known;

    // 1..6: the apartment is per thread and reference counted.
    REQUIRE(1, CoInitializeEx(0, COINIT_MULTITHREADED) == S_OK);
    REQUIRE(2, CoInitializeEx(0, COINIT_MULTITHREADED) == S_FALSE);
    REQUIRE(3, CoInitializeEx(0, COINIT_APARTMENTTHREADED) == RPC_E_CHANGED_MODE);
    REQUIRE(4, CoInitializeEx(0, 0x40) == E_INVALIDARG);
    REQUIRE(5, CoInitializeEx((void *)1, COINIT_MULTITHREADED) == E_INVALIDARG);
    CoUninitialize();
    CoUninitialize();
    // 6: once the last reference is gone the thread may pick the other model.
    REQUIRE(6, CoInitializeEx(0, COINIT_APARTMENTTHREADED) == S_OK);
    CoUninitialize();

    // 7..9: a fresh GUID is version 4 with the RFC 4122 variant.
    clear_guid(&created);
    REQUIRE(7, CoCreateGuid(&created) == S_OK);
    REQUIRE(8, (created.data3 >> 12) == 4);
    REQUIRE(9, (created.data4[0] & 0xc0) == 0x80);

    // 10..13: the documented text form is 38 characters plus a terminator.
    fill((u8 *)text, sizeof text, 0x5a);
    REQUIRE(10, StringFromGUID2(&created, text, GUID_TEXT_UNITS) == GUID_TEXT_UNITS);
    REQUIRE(11, text[0] == '{' && text[37] == '}' && text[38] == 0);
    REQUIRE(12, StringFromGUID2(&created, text, GUID_TEXT_UNITS - 1) == 0);
    REQUIRE(13, CLSIDFromString(text, &parsed) == S_OK && guid_equal(&created, &parsed));

    // 14..17: the parser accepts the braced and bare forms and rejects a short
    // group.
    clear_guid(&parsed);
    REQUIRE(14, CLSIDFromString(
                    (const u16 *)L"{00112233-4455-6677-8899-aabbccddeeff}", &parsed) == S_OK);
    REQUIRE(15, parsed.data1 == 0x00112233u && parsed.data2 == 0x4455 &&
                    parsed.data3 == 0x6677 && parsed.data4[0] == 0x88 &&
                    parsed.data4[7] == 0xff);
    clear_guid(&parsed);
    REQUIRE(16, CLSIDFromString(
                    (const u16 *)L"00112233-4455-6677-8899-aabbccddeeff", &parsed) == S_OK);
    REQUIRE(17, CLSIDFromString(
                    (const u16 *)L"{00112233-4455-6677-8899-aabbccddeef}", &parsed) ==
                    CO_E_CLASSSTRING);

    // 18..23: the task allocator hands out distinct, growable blocks.
    block = (u8 *)CoTaskMemAlloc(BLOCK_BYTES);
    REQUIRE(18, block != 0);
    fill(block, BLOCK_BYTES, 0xa5);
    grown = (u8 *)CoTaskMemRealloc(block, BLOCK_GROWN);
    REQUIRE(19, grown != 0);
    REQUIRE(20, filled(grown, BLOCK_BYTES, 0xa5));
    grown = (u8 *)CoTaskMemRealloc(grown, 0);
    REQUIRE(21, grown == 0);
    block = (u8 *)CoTaskMemAlloc(0);
    REQUIRE(22, block != 0);
    CoTaskMemFree(block);
    CoTaskMemFree(0);

    // 24..26: no class is registered, and a failed call nulls the out pointer.
    REQUIRE(35, CoInitializeEx(0, COINIT_MULTITHREADED) == S_OK);
    object = (void *)1;
    REQUIRE(24, CoCreateInstance(&parsed, 0, 1, &parsed, &object) == REGDB_E_CLASSNOTREG);
    REQUIRE(25, object == 0);
    REQUIRE(26, CoCreateInstance(&parsed, 0, 1, &parsed, 0) == E_POINTER);
    REQUIRE(27, CoCreateInstance(0, 0, 1, &parsed, &object) == E_INVALIDARG);
    CoUninitialize();

    // 28..31: a task-allocated string is released and the variant cleared.
    block = (u8 *)CoTaskMemAlloc(8);
    REQUIRE(28, block != 0);
    fill(block, 8, 0);
    variant.vt = VT_LPSTR;
    variant.reserved1 = 0;
    variant.reserved2 = 0;
    variant.reserved3 = 0;
    variant.payload.pointer = block;
    REQUIRE(29, PropVariantClear(&variant) == S_OK);
    REQUIRE(30, variant.vt == VT_EMPTY && variant.payload.pointer == 0);
    // 31: a value type carries no owned storage and clears in place.
    variant.vt = VT_I4;
    variant.payload.value = 0x1122334455667788ull;
    REQUIRE(31, PropVariantClear(&variant) == S_OK && variant.vt == VT_EMPTY &&
                    variant.payload.value == 0);

    // 32: a known GUID round-trips through the text form and the parser.
    clear_guid(&known);
    REQUIRE(32, CLSIDFromString(
                    (const u16 *)L"{deadbeef-cafe-babe-0123-456789abcdef}", &known) == S_OK);
    REQUIRE(33, StringFromGUID2(&known, text, GUID_TEXT_UNITS) == GUID_TEXT_UNITS);
    clear_guid(&parsed);
    REQUIRE(34, CLSIDFromString(text, &parsed) == S_OK && guid_equal(&known, &parsed));

    ExitProcess(0);
}
