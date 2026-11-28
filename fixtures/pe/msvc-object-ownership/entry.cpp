// SPDX-License-Identifier: GPL-3.0-or-later
#include "object.hpp"

extern "C" __declspec(dllimport) Object *XenoraConstruct(unsigned int seed);
extern "C" __declspec(dllimport) unsigned int XenoraObjectTrace();
extern "C" __declspec(dllimport) int XenoraConsume(Object *object, unsigned int permit);
extern "C" __declspec(dllimport) void *GetProcessHeap();
extern "C" __declspec(dllimport) unsigned long long HeapSize(void *heap, unsigned int flags,
                                                            const void *address);

extern "C" int entry() {
    if (XenoraObjectTrace() != 0) {
        return 11;
    }
    Object *object = XenoraConstruct(12);
    if (!object || XenoraObjectTrace() != 1) {
        return 12;
    }
    // HeapSize observes the synthetic UCRT allocator's live block record.
    void *heap = GetProcessHeap();
    if (HeapSize(heap, 0, object) == ~0ULL) {
        return 13;
    }
    unsigned long long address = reinterpret_cast<unsigned long long>(object);
#ifdef XENORA_REJECT_CONSUME
    if (XenoraConsume(object, 0) != -1 || XenoraObjectTrace() != 1 ||
        HeapSize(heap, 0, object) == ~0ULL) {
        return 14;
    }
#endif
    if (XenoraConsume(object, 17) != 43 || XenoraObjectTrace() != 12345 ||
        HeapSize(heap, 0, reinterpret_cast<void *>(address)) != ~0ULL) {
        return 15;
    }
#ifdef XENORA_REJECT_CONSUME
    return 24;
#else
    return 70;
#endif
}
