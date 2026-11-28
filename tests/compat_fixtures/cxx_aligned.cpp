// SPDX-License-Identifier: GPL-3.0-or-later
//
// Original no-CRT C++ fixture for aligned allocation operators (issue #941).
// The compiler emits the aligned scalar and array new/delete calls for an
// over-aligned type. Direct asm-named calls cover zero-size blocks, null
// deletion and the invalid-interior control without a C or C++ runtime.

typedef unsigned int u32;
typedef unsigned long long u64;
typedef __UINTPTR_TYPE__ uintptr;

extern "C" __declspec(dllimport) void ExitProcess(u32 status);
extern "C" __declspec(dllimport) void *aligned_new(u64 size, u64 alignment)
    asm("_ZnwySt11align_val_t");
extern "C" __declspec(dllimport) void aligned_delete(void *address, u64 alignment)
    asm("_ZdlPvSt11align_val_t");

struct alignas(64) Wide {
    u64 values[8];
};

static bool aligned(void *address) {
    return (reinterpret_cast<uintptr>(address) & 63u) == 0;
}

extern "C" void guest_cxx_aligned_entry() {
    Wide *first = new Wide;
    Wide *second = new Wide;
    if (first == 0 || second == 0)
        ExitProcess(1);
    if (!aligned(first) || !aligned(second))
        ExitProcess(2);
    if (first == second)
        ExitProcess(3);
    first->values[0] = 11;
    second->values[0] = 22;
    if (first->values[0] != 11 || second->values[0] != 22)
        ExitProcess(4);

    Wide *items = new Wide[2];
    if (items == 0 || !aligned(items) || items == first || items == second)
        ExitProcess(5);
    items[0].values[0] = 33;
    items[1].values[0] = 44;
    if (items[0].values[0] != 33 || items[1].values[0] != 44)
        ExitProcess(6);
    delete[] items;

    delete first;
    delete second;

    void *empty_first = aligned_new(0, 64);
    void *empty_second = aligned_new(0, 64);
    if (empty_first == 0 || empty_second == 0 || empty_first == empty_second)
        ExitProcess(7);
    if (!aligned(empty_first) || !aligned(empty_second))
        ExitProcess(8);
    aligned_delete(empty_first, 64);
    aligned_delete(empty_second, 64);
    aligned_delete(0, 64);

#ifdef INVALID_INTERIOR_CONTROL
    void *block = aligned_new(64, 64);
    if (block == 0)
        ExitProcess(40);
    aligned_delete(reinterpret_cast<unsigned char *>(block) + 8, 64);
    ExitProcess(41);
#endif

    ExitProcess(0);
}
