// SPDX-License-Identifier: GPL-3.0-or-later
#include "records.h"

extern "C" __declspec(dllimport) Handle GetProcessHeap();
extern "C" __declspec(dllimport) void *HeapAlloc(Handle, U32, decltype(sizeof(0)));
extern "C" __declspec(dllimport) int HeapFree(Handle, U32, void *);

void *operator new(decltype(sizeof(0)), void *storage) noexcept { return storage; }

class Record {
public:
    Trace *const trace;
    const U32 value;

    __declspec(noinline) Record(Trace *out, U32 tag) : trace(out), value(tag) { trace->constructed = tag; }
    __declspec(noinline) ~Record() { trace->destroyed = value; }
};

extern "C" __declspec(dllexport) void *MakeRecord(Trace *trace, U32 tag) {
    void *storage = HeapAlloc(GetProcessHeap(), 0, sizeof(Record));
    return storage ? new (storage) Record(trace, tag) : nullptr;
}

extern "C" __declspec(dllexport) void FinishRecord(void *payload, U32 callback_id) {
    auto *record = static_cast<Record *>(payload);
    Trace *trace = record->trace;
    trace->callback_id = callback_id;
    trace->callbacks = trace->callbacks + 1;
    record->~Record();
    trace->freed = HeapFree(GetProcessHeap(), 0, record) != 0;
}
