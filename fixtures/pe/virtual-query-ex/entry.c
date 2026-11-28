// SPDX-License-Identifier: GPL-3.0-or-later

// Original AMD64 Windows guest. A failed check exits with its source line.
typedef unsigned int u32;
typedef unsigned long long usize;
typedef unsigned char byte;
typedef void *handle;

struct memory_info {
    void *base;
    void *allocation_base;
    u32 allocation_protect;
    unsigned short partition_id;
    usize region_size;
    u32 state, protect, type;
};

_Static_assert(sizeof(struct memory_info) == 48, "AMD64 MEMORY_BASIC_INFORMATION");

union query_buffer {
    struct memory_info info;
    byte bytes[64];
};

#define IMPORT __declspec(dllimport)
IMPORT usize VirtualQueryEx(handle, const void *, struct memory_info *, usize);
IMPORT usize VirtualQuery(const void *, struct memory_info *, usize);
IMPORT void *VirtualAlloc(void *, usize, u32, u32);
IMPORT int VirtualProtect(void *, usize, u32, u32 *);
IMPORT int VirtualFree(void *, usize, u32);
IMPORT handle GetCurrentProcess(void);
IMPORT u32 GetCurrentProcessId(void);
IMPORT handle OpenProcess(u32, int, u32);
IMPORT handle CreateEventW(void *, int, int, const unsigned short *);
IMPORT int CloseHandle(handle);
IMPORT u32 GetLastError(void);
IMPORT __declspec(noreturn) void ExitProcess(u32);

#define CHECK(condition) do { if (!(condition)) ExitProcess(__LINE__); } while (0)
#define PAGE_SIZE 4096u
#define PAGE_READONLY 2u
#define PAGE_READWRITE 4u
#define MEM_COMMIT 0x1000u
#define MEM_RESERVE 0x2000u
#define MEM_RELEASE 0x8000u
#define MEM_PRIVATE 0x20000u
#define PROCESS_QUERY_INFORMATION 0x0400u
#define ERROR_INVALID_HANDLE 6u
#define ERROR_NOT_SUPPORTED 50u
#define SENTINEL 0xa5u
#define SHORT_PREFIX 20u

static void fill(union query_buffer *output) {
    for (usize i = 0; i < sizeof(output->bytes); ++i)
        output->bytes[i] = SENTINEL;
}

static void require_canaries(const union query_buffer *output, usize written) {
    for (usize i = written; i < sizeof(output->bytes); ++i)
        CHECK(output->bytes[i] == SENTINEL);
}

static void require_failure(handle process, const void *address, u32 error) {
    union query_buffer output;
    fill(&output);
    CHECK(!VirtualQueryEx(process, address, &output.info, sizeof(output.info)));
    CHECK(GetLastError() == error);
    require_canaries(&output, 0);
}

void entry(void) {
    byte *pages = VirtualAlloc(0, 2 * PAGE_SIZE, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    CHECK(pages);
    u32 old_protect = 0;
    CHECK(VirtualProtect(pages + PAGE_SIZE, PAGE_SIZE, PAGE_READONLY, &old_protect));
    CHECK(old_protect == PAGE_READWRITE);

    handle self = GetCurrentProcess();
    union query_buffer result, canonical;
    fill(&result);
    CHECK(VirtualQueryEx(self, pages + 37, &result.info, sizeof(result.info)) == sizeof(result.info));
    CHECK(result.info.base == pages && result.info.allocation_base == pages);
    CHECK(result.info.region_size == PAGE_SIZE && result.info.protect == PAGE_READWRITE);
    CHECK(result.info.state == MEM_COMMIT && result.info.type == MEM_PRIVATE);
    require_canaries(&result, sizeof(result.info));

    const byte *address = pages + PAGE_SIZE + 37;
    fill(&result);
    fill(&canonical);
    CHECK(VirtualQueryEx(self, address, &result.info, sizeof(result.bytes)) == sizeof(result.info));
    CHECK(VirtualQuery(address, &canonical.info, sizeof(canonical.info)) == sizeof(canonical.info));
    CHECK(result.info.base == pages + PAGE_SIZE && result.info.allocation_base == pages);
    CHECK(result.info.region_size == PAGE_SIZE && result.info.protect == PAGE_READONLY);
    CHECK(result.info.state == MEM_COMMIT && result.info.type == MEM_PRIVATE);
    for (usize i = 0; i < sizeof(result.info); ++i)
        CHECK(result.bytes[i] == canonical.bytes[i]);
    require_canaries(&result, sizeof(result.info));

    // The existing Xenora VirtualQuery contract writes even a short prefix.
    fill(&result);
    CHECK(VirtualQueryEx(self, address, &result.info, SHORT_PREFIX) == SHORT_PREFIX);
    for (usize i = 0; i < SHORT_PREFIX; ++i)
        CHECK(result.bytes[i] == canonical.bytes[i]);
    require_canaries(&result, SHORT_PREFIX);

    require_failure(0, address, ERROR_INVALID_HANDLE);
    require_failure((handle)0x123456789ull, address, ERROR_INVALID_HANDLE);
    handle wrong_type = CreateEventW(0, 0, 0, 0);
    CHECK(wrong_type);
    require_failure(wrong_type, address, ERROR_INVALID_HANDLE);
    CHECK(CloseHandle(wrong_type));
    handle real_self = OpenProcess(PROCESS_QUERY_INFORMATION, 0, GetCurrentProcessId());
    CHECK(real_self && real_self != self);
    require_failure(real_self, address, ERROR_NOT_SUPPORTED);
    CHECK(CloseHandle(real_self));
    require_failure(real_self, address, ERROR_INVALID_HANDLE);

    CHECK(VirtualQueryEx(self, address, &result.info, sizeof(result.info)) == sizeof(result.info));
    CHECK(result.info.base == pages + PAGE_SIZE && result.info.protect == PAGE_READONLY);
    CHECK(VirtualFree(pages, 0, MEM_RELEASE));
    ExitProcess(0);
}
