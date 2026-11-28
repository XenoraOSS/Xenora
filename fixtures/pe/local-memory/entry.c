// SPDX-License-Identifier: GPL-3.0-or-later

// Original AMD64 Windows guest. A failed check exits with its source line.
typedef unsigned int u32;
typedef unsigned long long usize;
typedef unsigned char byte;
typedef void *handle;

#define IMPORT __declspec(dllimport)
IMPORT handle LocalAlloc(u32, usize);
IMPORT handle LocalFree(handle);
IMPORT void *LocalLock(handle);
IMPORT int LocalUnlock(handle);
IMPORT usize LocalSize(handle);
IMPORT u32 LocalFlags(handle);
IMPORT handle LocalReAlloc(handle, usize, u32);
IMPORT handle LocalHandle(const void *);
IMPORT u32 GetLastError(void);
IMPORT void SetLastError(u32);
IMPORT __declspec(noreturn) void ExitProcess(u32);

#define CHECK(condition) do { if (!(condition)) ExitProcess(__LINE__); } while (0)
#define LMEM_MOVEABLE 0x0002u
#define LMEM_ZEROINIT 0x0040u
#define LMEM_DISCARDED 0x4000u
#define LMEM_LOCKCOUNT 0x00ffu
#define ERROR_INVALID_HANDLE 6u
#define ERROR_NOT_LOCKED 158u
#define ERROR_DISCARDED 157u

static void require_final_unlock(handle memory) {
    SetLastError(0x1234u);
    CHECK(!LocalUnlock(memory));
    CHECK(GetLastError() == 0);
    CHECK((LocalFlags(memory) & LMEM_LOCKCOUNT) == 0);
}

static void require_discarded(handle memory) {
    CHECK(LocalSize(memory) == 0);
    CHECK(LocalFlags(memory) & LMEM_DISCARDED);
    CHECK(!LocalLock(memory));
    CHECK(GetLastError() == ERROR_DISCARDED);
}

void entry(void) {
    handle fixed = LocalAlloc(LMEM_ZEROINIT, 8);
    CHECK(fixed && LocalLock(fixed) == fixed);
    CHECK(!LocalUnlock(fixed) && GetLastError() == ERROR_NOT_LOCKED);
    CHECK(!LocalFree(fixed));

    handle movable = LocalAlloc(LMEM_MOVEABLE, 0);
    handle another = LocalAlloc(LMEM_MOVEABLE, 0);
    CHECK(movable && another && movable != another);
    CHECK(!LocalFree(another));
    require_discarded(movable);

    CHECK(LocalReAlloc(movable, 8, LMEM_MOVEABLE | LMEM_ZEROINIT) == movable);
    CHECK(!(LocalFlags(movable) & LMEM_DISCARDED));
    CHECK(LocalSize(movable) >= 8);
    byte *bytes = LocalLock(movable);
    CHECK(bytes && bytes != movable && LocalHandle(bytes) == movable);
    CHECK(LocalLock(movable) == bytes);
    CHECK((LocalFlags(movable) & LMEM_LOCKCOUNT) == 2);
    CHECK(LocalUnlock(movable));
    CHECK((LocalFlags(movable) & LMEM_LOCKCOUNT) == 1);
    for (u32 i = 0; i < 8; ++i) {
        CHECK(bytes[i] == 0);
        bytes[i] = (byte)(0x91 + i);
    }
    require_final_unlock(movable);

    CHECK(!LocalReAlloc(movable, ~(usize)0, LMEM_MOVEABLE | LMEM_ZEROINIT));
    CHECK(LocalSize(movable) >= 8);
    bytes = LocalLock(movable);
    CHECK(bytes && LocalHandle(bytes) == movable);
    for (u32 i = 0; i < 8; ++i)
        CHECK(bytes[i] == (byte)(0x91 + i));
    require_final_unlock(movable);

    bytes = LocalLock(movable);
    CHECK(bytes && LocalHandle(bytes) == movable);
    CHECK(!LocalReAlloc(movable, 0, LMEM_MOVEABLE));
    CHECK(LocalSize(movable) >= 8 && !(LocalFlags(movable) & LMEM_DISCARDED));
    CHECK((LocalFlags(movable) & LMEM_LOCKCOUNT) == 1);

    // Xenora's deterministic guest allocator places this live blocker in the
    // next 64 KiB slot. Growing past its start cannot succeed in place.
    handle blocker = LocalAlloc(LMEM_MOVEABLE, 8);
    CHECK(blocker);
    byte *blocker_bytes = LocalLock(blocker);
    usize old_address = (usize)bytes;
    CHECK(blocker_bytes && (usize)blocker_bytes - old_address == 65536);
    usize growth = (usize)blocker_bytes - old_address + 1;
    blocker_bytes[0] = 0x5a;
    CHECK(!LocalReAlloc(movable, growth, 0));
    CHECK(LocalSize(movable) >= 8 && LocalSize(movable) < growth);
    CHECK(LocalHandle(bytes) == movable && (LocalFlags(movable) & LMEM_LOCKCOUNT) == 1);
    for (u32 i = 0; i < 8; ++i)
        CHECK(bytes[i] == (byte)(0x91 + i));
    require_final_unlock(movable);

    CHECK(LocalReAlloc(movable, growth, LMEM_MOVEABLE | LMEM_ZEROINIT) == movable);
    CHECK(LocalSize(movable) >= growth);
    bytes = LocalLock(movable);
    CHECK(bytes && (usize)bytes != old_address && LocalHandle(bytes) == movable);
    CHECK(!LocalHandle((const void *)old_address));
    for (u32 i = 0; i < 8; ++i)
        CHECK(bytes[i] == (byte)(0x91 + i));
    CHECK(bytes[8] == 0 && bytes[growth - 1] == 0 && blocker_bytes[0] == 0x5a);
    require_final_unlock(movable);
    CHECK(!LocalFree(blocker));

    CHECK(LocalReAlloc(movable, 0, LMEM_MOVEABLE) == movable);
    require_discarded(movable);
    CHECK(!LocalFree(movable));
    CHECK(LocalFree(movable) == movable && GetLastError() == ERROR_INVALID_HANDLE);

    handle locked = LocalAlloc(LMEM_MOVEABLE, 8);
    CHECK(locked && LocalLock(locked));
    CHECK(!LocalFree(locked));
    CHECK(LocalFree(locked) == locked && GetLastError() == ERROR_INVALID_HANDLE);
    ExitProcess(0);
}
