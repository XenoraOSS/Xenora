// SPDX-License-Identifier: GPL-3.0-or-later

// Original AMD64 Windows guest. A failed check exits with its source line.
typedef unsigned int u32;
typedef unsigned short u16;
typedef unsigned long long u64;
typedef unsigned char byte;

struct variant {
    u16 type, reserved1, reserved2, reserved3;
    union {
        void *guid;
        struct {
            u32 count;
            byte *bytes;
        } blob;
    } value;
};

_Static_assert(sizeof(struct variant) == 24, "AMD64 PROPVARIANT");
_Static_assert(__builtin_offsetof(struct variant, value.guid) == 8, "CLSID pointer offset");
_Static_assert(__builtin_offsetof(struct variant, value.blob.count) == 8, "BLOB count offset");
_Static_assert(__builtin_offsetof(struct variant, value.blob.bytes) == 16, "BLOB pointer offset");

#define IMPORT __declspec(dllimport)
IMPORT void *CoTaskMemAlloc(u64);
IMPORT int PropVariantClear(struct variant *);
IMPORT void *GetProcessHeap(void);
IMPORT void *HeapAlloc(void *, u32, u64);
IMPORT void *VirtualAlloc(void *, u64, u32, u32);
IMPORT int VirtualProtect(void *, u64, u32, u32 *);
IMPORT __declspec(noreturn) void ExitProcess(u32);

#define CHECK(condition) do { if (!(condition)) ExitProcess(__LINE__); } while (0)
#define VT_BLOB 65u
#define VT_CLSID 72u
#define E_INVALIDARG 0x80070057u
#define MEM_RESERVE_COMMIT 0x3000u
#define PAGE_READWRITE 4u
#define PAGE_READONLY 2u

#pragma section(".gate", read, write)
__declspec(allocate(".gate"))
static volatile u64 gate[5] = {0x50564f44u, 0, 0, 0, 0};

static void initialize(struct variant *value, u16 type, u32 size, byte *payload) {
    byte *record = (byte *)value;
    for (u32 i = 0; i < sizeof(*value); ++i)
        record[i] = 0;
    value->type = type;
    if (type == VT_CLSID)
        value->value.guid = payload;
    else {
        value->value.blob.count = size;
        value->value.blob.bytes = payload;
    }
}

#if !defined(WRONG_OWNER) && !defined(READONLY_DESCRIPTOR) && !defined(OVERLAP_DESCRIPTOR)
static void clear_twice(struct variant *value) {
    for (u32 call = 0; call < 2; ++call) {
        CHECK(PropVariantClear(value) == 0);
        for (u32 i = 0; i < sizeof(*value); ++i)
            CHECK(((const byte *)value)[i] == 0);
    }
}
#endif

void entry(void) {
#if defined(WRONG_OWNER)
    byte *payload = HeapAlloc(GetProcessHeap(), 0, 16);
    static struct variant record;
    CHECK(payload);
    payload[0] = 0x7e;
    initialize(&record, VT_CLSID, 16, payload);
    gate[1] = (u64)payload;
    gate[2] = (u64)&record;
    (void)PropVariantClear(&record);
    ExitProcess(200);
#elif defined(READONLY_DESCRIPTOR)
    byte *payload = CoTaskMemAlloc(16);
    struct variant *record = VirtualAlloc(0, 4096, MEM_RESERVE_COMMIT, PAGE_READWRITE);
    CHECK(payload && record);
    payload[0] = 0x7e;
    initialize(record, VT_BLOB, 16, payload);
    u32 previous = 0;
    CHECK(VirtualProtect(record, 4096, PAGE_READONLY, &previous) && previous == PAGE_READWRITE);
    gate[1] = (u64)payload;
    gate[2] = (u64)record;
    (void)PropVariantClear(record);
    ExitProcess(201);
#elif defined(OVERLAP_DESCRIPTOR)
    byte *payload = CoTaskMemAlloc(48);
    CHECK(payload);
    payload[0] = 0x7e;
    payload[47] = 0xe7;
    struct variant *record = (struct variant *)(payload + 8);
    initialize(record, VT_BLOB, 48, payload);
    gate[1] = (u64)payload;
    gate[2] = (u64)record;
    CHECK((u32)PropVariantClear(record) == E_INVALIDARG);
    ExitProcess(0);
#else
    byte *guid = CoTaskMemAlloc(16);
    byte *blob = CoTaskMemAlloc(7);
    byte *neighbor = CoTaskMemAlloc(9);
    CHECK(guid && blob && neighbor && guid != blob && blob != neighbor && guid != neighbor);
    for (u32 i = 0; i < 16; ++i)
        guid[i] = (byte)i;
    for (u32 i = 0; i < 7; ++i)
        blob[i] = (byte)(0x70 + i);
    for (u32 i = 0; i < 9; ++i)
        neighbor[i] = (byte)(0x90 + i);
    static struct variant guid_record, blob_record;
    initialize(&guid_record, VT_CLSID, 16, guid);
    initialize(&blob_record, VT_BLOB, 7, blob);
    gate[1] = (u64)guid;
    gate[2] = (u64)blob;
    gate[3] = (u64)neighbor;
    gate[4] = (u64)&blob_record;
    clear_twice(&guid_record);
    clear_twice(&blob_record);
    ExitProcess(0);
#endif
}
