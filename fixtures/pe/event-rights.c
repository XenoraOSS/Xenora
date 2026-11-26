// SPDX-License-Identifier: GPL-3.0-or-later

// Original freestanding PE32+ workload for scoped event rights and scheduled
// handoff. The ANSI import uses a documented synchronization API-set contract.

typedef unsigned int u32;
typedef unsigned long long u64;
typedef u32 (*Worker)(void *);

#define IMPORT __declspec(dllimport)
#define SYNCHRONIZE 0x00100000u
#define EVENT_MODIFY_STATE 0x0002u
#define EVENT_ALL_ACCESS 0x001f0003u
#define MANUAL_RESET 1u
#define INITIAL_SET 2u
#define WAIT_OBJECT_0 0u
#define WAIT_TIMEOUT 258u
#define WAIT_FAILED 0xffffffffu
#define ERROR_ACCESS_DENIED 5u
#define ERROR_NOT_SUPPORTED 50u
#define ERROR_INVALID_PARAMETER 87u
#define CURRENT_PROCESS (~0ull)
#define ROUNDS 4u
#define WORKER_STATUS 0xa1b2c3d4u

IMPORT u64 CreateEventExA(void *attributes, const char *name, u32 flags, u32 access);
IMPORT u64 CreateEventExW(void *attributes, const unsigned short *name, u32 flags, u32 access);
IMPORT int SetEvent(u64 event);
IMPORT int ResetEvent(u64 event);
IMPORT u32 WaitForSingleObject(u64 handle, u32 timeout);
IMPORT int DuplicateHandle(u64 source_process, u64 source, u64 target_process,
                           u64 *target, u32 access, int inherit, u32 options);
IMPORT int CloseHandle(u64 handle);
IMPORT u32 GetLastError(void);
IMPORT u64 CreateThread(void *attributes, u64 stack_size, Worker worker, void *argument,
                        u32 flags, u32 *thread_id);
IMPORT int GetExitCodeThread(u64 thread, u32 *status);
IMPORT __declspec(noreturn) void ExitProcess(u32 status);

#define CHECK(condition) do { if (!(condition)) ExitProcess(__LINE__); } while (0)

struct Handoff {
    u64 request_wait;
    u64 reply_modify;
    volatile u32 pending;
    volatile u32 reply;
};

static u64 duplicate(u64 source, u32 rights) {
    u64 target = 0;
    CHECK(DuplicateHandle(CURRENT_PROCESS, source, CURRENT_PROCESS, &target, rights, 0, 0));
    CHECK(target != 0 && target != source);
    return target;
}

static u32 worker(void *argument) {
    struct Handoff *handoff = argument;
    for (u32 round = 1; round <= ROUNDS; round++) {
        CHECK(WaitForSingleObject(handoff->request_wait, 1000) == WAIT_OBJECT_0);
        const u32 request = handoff->pending;
        CHECK(request == round * 17u);
        handoff->reply = request * request + round;
        CHECK(SetEvent(handoff->reply_modify));
    }
    return WORKER_STATUS;
}

void entry(void) {
    CHECK(CreateEventExA(0, 0, 0x80000000u, EVENT_ALL_ACCESS) == 0);
    CHECK(GetLastError() == ERROR_INVALID_PARAMETER);
    CHECK(CreateEventExW(0, 0, 0, 0x80000000u) == 0);
    CHECK(GetLastError() == ERROR_NOT_SUPPORTED);
    CHECK(CreateEventExA((void *)1, 0, 0, EVENT_ALL_ACCESS) == 0);
    CHECK(GetLastError() == ERROR_NOT_SUPPORTED);
    CHECK(CreateEventExW(0, (const unsigned short *)1, 0, EVENT_ALL_ACCESS) == 0);
    CHECK(GetLastError() == ERROR_NOT_SUPPORTED);

    u64 no_rights = CreateEventExA(0, 0, INITIAL_SET, 0);
    CHECK(no_rights != 0);
    CHECK(!SetEvent(no_rights) && GetLastError() == ERROR_ACCESS_DENIED);
    CHECK(!ResetEvent(no_rights) && GetLastError() == ERROR_ACCESS_DENIED);
    CHECK(WaitForSingleObject(no_rights, 0) == WAIT_FAILED);
    CHECK(GetLastError() == ERROR_ACCESS_DENIED);
    CHECK(CloseHandle(no_rights));

    u64 auto_event = CreateEventExA(0, 0, INITIAL_SET, SYNCHRONIZE);
    CHECK(auto_event != 0);
    CHECK(!SetEvent(auto_event) && GetLastError() == ERROR_ACCESS_DENIED);
    CHECK(!ResetEvent(auto_event) && GetLastError() == ERROR_ACCESS_DENIED);
    CHECK(WaitForSingleObject(auto_event, 0) == WAIT_OBJECT_0);
    CHECK(WaitForSingleObject(auto_event, 0) == WAIT_TIMEOUT);
    CHECK(CloseHandle(auto_event));

    u64 modify_event = CreateEventExW(0, 0, 0, EVENT_MODIFY_STATE);
    CHECK(modify_event != 0);
    CHECK(SetEvent(modify_event));
    CHECK(WaitForSingleObject(modify_event, 0) == WAIT_FAILED);
    CHECK(GetLastError() == ERROR_ACCESS_DENIED);
    CHECK(ResetEvent(modify_event));
    CHECK(CloseHandle(modify_event));

    u64 manual_event = CreateEventExW(0, 0, MANUAL_RESET | INITIAL_SET, EVENT_ALL_ACCESS);
    CHECK(manual_event != 0);
    CHECK(WaitForSingleObject(manual_event, 0) == WAIT_OBJECT_0);
    CHECK(WaitForSingleObject(manual_event, 0) == WAIT_OBJECT_0);
    CHECK(ResetEvent(manual_event));
    CHECK(WaitForSingleObject(manual_event, 0) == WAIT_TIMEOUT);
    CHECK(CloseHandle(manual_event));

    u64 request = CreateEventExA(0, 0, 0, EVENT_ALL_ACCESS);
    u64 reply = CreateEventExW(0, 0, 0, EVENT_ALL_ACCESS);
    CHECK(request != 0 && reply != 0);
    struct Handoff handoff;
    handoff.request_wait = duplicate(request, SYNCHRONIZE);
    handoff.reply_modify = duplicate(reply, EVENT_MODIFY_STATE);
    handoff.pending = 0;
    handoff.reply = 0;
    const u64 request_modify = duplicate(request, EVENT_MODIFY_STATE);
    const u64 reply_wait = duplicate(reply, SYNCHRONIZE);
    CHECK(CloseHandle(request));
    CHECK(CloseHandle(reply));

    // Each object survives through its restricted duplicate handles. Check
    // the four assignments before using them for the running worker.
    CHECK(WaitForSingleObject(handoff.request_wait, 0) == WAIT_TIMEOUT);
    CHECK(WaitForSingleObject(reply_wait, 0) == WAIT_TIMEOUT);
    CHECK(!SetEvent(handoff.request_wait) && GetLastError() == ERROR_ACCESS_DENIED);
    CHECK(!ResetEvent(reply_wait) && GetLastError() == ERROR_ACCESS_DENIED);
    CHECK(WaitForSingleObject(request_modify, 0) == WAIT_FAILED);
    CHECK(GetLastError() == ERROR_ACCESS_DENIED);
    CHECK(WaitForSingleObject(handoff.reply_modify, 0) == WAIT_FAILED);
    CHECK(GetLastError() == ERROR_ACCESS_DENIED);
    CHECK(ResetEvent(request_modify));
    CHECK(ResetEvent(handoff.reply_modify));

    const u64 thread = CreateThread(0, 0, worker, &handoff, 0, 0);
    CHECK(thread != 0);
    for (u32 round = 1; round <= ROUNDS; round++) {
        const u32 value = round * 17u;
        handoff.pending = value;
        CHECK(SetEvent(request_modify));
        CHECK(WaitForSingleObject(reply_wait, 1000) == WAIT_OBJECT_0);
        CHECK(handoff.reply == value * value + round);
        CHECK(WaitForSingleObject(reply_wait, 0) == WAIT_TIMEOUT);
    }
    CHECK(WaitForSingleObject(thread, 1000) == WAIT_OBJECT_0);
    u32 status = 0;
    CHECK(GetExitCodeThread(thread, &status));
    CHECK(status == WORKER_STATUS);
    CHECK(CloseHandle(thread));
    CHECK(CloseHandle(reply_wait));
    CHECK(CloseHandle(handoff.reply_modify));
    CHECK(CloseHandle(request_modify));
    CHECK(CloseHandle(handoff.request_wait));
    ExitProcess(0);
}
