// SPDX-License-Identifier: GPL-3.0-or-later
#include "hierarchy.hpp"

extern "C" __declspec(dllimport) bool ProduceViews(Views *);
extern "C" __declspec(dllimport) unsigned EventCount();
extern "C" __declspec(dllimport) int ConsumeViews(Left *, Right *);
extern "C" __declspec(dllimport) void *GetProcessHeap();
extern "C" __declspec(dllimport) decltype(sizeof(0)) HeapSize(void *, unsigned, const void *);

extern "C" int entry() {
    if (EventCount() != 0)
        return 1;

    Views views{};
    if (!ProduceViews(&views) || EventCount() != 4)
        return 2;

    void *heap = GetProcessHeap();
    const auto address = reinterpret_cast<decltype(sizeof(0))>(views.allocation);
    constexpr auto unavailable = static_cast<decltype(sizeof(0))>(-1);
    const auto live_size = HeapSize(heap, 0, views.allocation);
    if (live_size == unavailable)
        return 3;

#ifdef REJECT_HANDOFF
    if (ConsumeViews(views.left, nullptr) != -1 || EventCount() != 4 ||
        HeapSize(heap, 0, views.allocation) != live_size)
        return 4;
#endif

    if (ConsumeViews(views.left, views.right) != 253423 || EventCount() != 13)
        return 5;
    if (HeapSize(heap, 0, reinterpret_cast<void *>(address)) != unavailable)
        return 6;

#ifdef REJECT_HANDOFF
    return 24;
#else
    return 70;
#endif
}
