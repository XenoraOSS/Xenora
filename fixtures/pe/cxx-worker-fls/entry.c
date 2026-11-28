// SPDX-License-Identifier: GPL-3.0-or-later
#include "records.h"

__declspec(dllimport) U32 FlsAlloc(void (*callback)(void *));
__declspec(dllimport) int FlsFree(U32);
__declspec(dllimport) void *FlsGetValue(U32);
__declspec(dllimport) int FlsSetValue(U32, void *);
__declspec(dllimport) U32 GetCurrentThreadId(void);
__declspec(dllimport) Handle CreateEventW(void *, int, int, const unsigned short *);
__declspec(dllimport) int SetEvent(Handle);
__declspec(dllimport) Handle CreateThread(void *, unsigned long long, U32 (*)(void *),
                                          void *, U32, U32 *);
__declspec(dllimport) U32 WaitForSingleObject(Handle, U32);
__declspec(dllimport) int GetExitCodeThread(Handle, U32 *);
__declspec(dllimport) int CloseHandle(Handle);
__declspec(dllimport) __declspec(noreturn) void ExitThread(U32);
__declspec(dllimport) __declspec(noreturn) void ExitProcess(U32);
__declspec(dllimport) void *MakeRecord(Trace *, U32);
__declspec(dllimport) void WorkerCleanup(void *);

#define INFINITE 0xffffffffu
#define WAIT_TIMEOUT 258u
#define REQUIRE(code, condition) do { if (!(condition)) ExitProcess(code); } while (0)

typedef struct Worker {
    Trace trace;
    Handle ready;
    Handle release;
    U32 index;
    U32 tag;
    U32 exit_code;
    int explicit_exit;
} Worker;

static U32 worker_main(void *argument) {
    Worker *worker = argument;
    REQUIRE(31, FlsGetValue(worker->index) == 0);
    worker->trace.owner_id = GetCurrentThreadId();
    void *record = MakeRecord(&worker->trace, worker->tag);
    REQUIRE(32, record && FlsSetValue(worker->index, record));
    worker->trace.record = record;
    REQUIRE(33, SetEvent(worker->ready));
    REQUIRE(34, WaitForSingleObject(worker->release, INFINITE) == 0);
    REQUIRE(35, FlsGetValue(worker->index) == record);
    if (worker->explicit_exit)
        ExitThread(worker->exit_code);
    return worker->exit_code;
}

void entry(void) {
    const U32 index = FlsAlloc(WorkerCleanup);
    REQUIRE(1, index != 0xffffffffu && FlsGetValue(index) == 0);
    const Handle release = CreateEventW(0, 1, 0, 0);
    REQUIRE(2, release);
    Worker workers[2];
    Handle threads[2];
    U32 ids[2];
    for (U32 i = 0; i < 2; ++i) {
        Worker *worker = &workers[i];
        worker->trace.record = 0;
        worker->trace.owner_id = 0;
        worker->trace.callback_id = 0;
        worker->trace.constructed = 0;
        worker->trace.destroyed = 0;
        worker->trace.callbacks = 0;
        worker->trace.freed = 0;
        worker->ready = CreateEventW(0, 0, 0, 0);
        worker->release = release;
        worker->index = index;
        worker->tag = 11 + i;
        worker->exit_code = 41 + i;
        worker->explicit_exit = i == 1;
        REQUIRE(3, worker->ready);
        threads[i] = CreateThread(0, 0, worker_main, worker, 0, &ids[i]);
        REQUIRE(4, threads[i]);
    }
    for (U32 i = 0; i < 2; ++i) {
        REQUIRE(5, WaitForSingleObject(workers[i].ready, INFINITE) == 0);
        REQUIRE(6, WaitForSingleObject(threads[i], 0) == WAIT_TIMEOUT);
        REQUIRE(7, workers[i].trace.owner_id == ids[i] && ids[i] != GetCurrentThreadId());
        REQUIRE(8, workers[i].trace.constructed == workers[i].tag);
        REQUIRE(9, workers[i].trace.callbacks == 0 && workers[i].trace.destroyed == 0);
    }
    REQUIRE(10, ids[0] != ids[1] && workers[0].trace.record && workers[1].trace.record &&
        workers[0].trace.record != workers[1].trace.record && FlsGetValue(index) == 0);
    REQUIRE(11, SetEvent(release));
    for (U32 i = 0; i < 2; ++i) {
        REQUIRE(12, WaitForSingleObject(threads[i], INFINITE) == 0);
        U32 status = 0;
        REQUIRE(13, GetExitCodeThread(threads[i], &status) && status == workers[i].exit_code);
        const Trace *trace = &workers[i].trace;
        REQUIRE(14, trace->callbacks == 1 && trace->callback_id == ids[i]);
        REQUIRE(15, trace->destroyed == workers[i].tag && trace->freed == 1);
        REQUIRE(16, CloseHandle(threads[i]) && CloseHandle(workers[i].ready));
    }
    REQUIRE(17, FlsGetValue(index) == 0 && FlsFree(index) && CloseHandle(release));
    ExitProcess(0);
}
