// SPDX-License-Identifier: GPL-3.0-or-later

typedef unsigned int u32;
typedef unsigned long long Handle;
typedef int HResult;
typedef struct {
    u32 first;
    unsigned short second;
    unsigned short third;
    unsigned char last[8];
} Guid;

typedef struct Unknown Unknown;
typedef struct {
    HResult (*query)(Unknown *, const Guid *, void **);
    u32 (*add_ref)(Unknown *);
    u32 (*release)(Unknown *);
} UnknownTable;
struct Unknown {
    const UnknownTable *table;
};

__declspec(dllimport) HResult CoInitializeEx(void *, u32);
__declspec(dllimport) void CoUninitialize(void);
__declspec(dllimport) HResult CoCreateInstance(const Guid *, Unknown *, u32, const Guid *, void **);
__declspec(dllimport) HResult DllCanUnloadNow(void);
__declspec(dllimport) Handle CreateEventW(void *, int, int, const unsigned short *);
__declspec(dllimport) Handle CreateThread(void *, Handle, u32 (*)(void *), void *, u32, u32 *);
__declspec(dllimport) int SetEvent(Handle);
__declspec(dllimport) u32 WaitForSingleObject(Handle, u32);
__declspec(dllimport) int GetExitCodeThread(Handle, u32 *);
__declspec(dllimport) int CloseHandle(Handle);
__declspec(dllimport) __declspec(noreturn) void ExitProcess(u32);

enum { S_OK = 0, S_FALSE = 1, COINIT_MTA = 0, COINIT_STA = 2, CLSCTX_INPROC_SERVER = 1 };
#define CO_E_NOTINITIALIZED ((HResult)0x800401f0u)
#define RPC_E_CHANGED_MODE ((HResult)0x80010106u)
#define WAIT_INFINITE (~0u)
#define CHECK(code, condition) do { if (!(condition)) ExitProcess(code); } while (0)

// The class exists in the opt-in Xenora server. IUnknown uses its public IID.
static const Guid class_id = {0x58713751, 0x7490, 0x414e, {0xa0, 0x58, 0x63, 0x6f, 0x6d, 0, 0, 1}};
static const Guid unknown_id = {0, 0, 0, {0xc0, 0, 0, 0, 0, 0, 0, 0x46}};

typedef struct {
    Handle holding_object;
    Handle release_object;
} Handoff;

static HResult activate(void **object) {
    return CoCreateInstance(&class_id, 0, CLSCTX_INPROC_SERVER, &unknown_id, object);
}

static u32 release(void *pointer) {
    Unknown *object = pointer;
    return object->table->release(object);
}

static u32 worker(void *argument) {
    const Handoff *handoff = argument;
    void *object = argument;
    CHECK(21, activate(&object) == CO_E_NOTINITIALIZED && !object);
    CHECK(22, CoInitializeEx(0, COINIT_MTA) == S_OK);
    CHECK(23, CoInitializeEx(0, COINIT_MTA) == S_FALSE);
    CHECK(24, CoInitializeEx(0, COINIT_STA) == RPC_E_CHANGED_MODE);
    CoUninitialize();
    CHECK(25, activate(&object) == S_OK && object);

    CHECK(26, SetEvent(handoff->holding_object));
    CHECK(27, WaitForSingleObject(handoff->release_object, WAIT_INFINITE) == 0);
    CHECK(28, release(object) == 0);
    CHECK(30, DllCanUnloadNow() == S_OK);

    CoUninitialize();
    object = argument;
    CHECK(29, activate(&object) == CO_E_NOTINITIALIZED && !object);
    return 0;
}

void entry(void) {
    void *object = 0;
    CHECK(1, DllCanUnloadNow() == S_OK);
    CHECK(2, CoInitializeEx(0, COINIT_STA) == S_OK);
    CHECK(3, activate(&object) == S_OK && object);
    Handoff handoff;
    handoff.holding_object = CreateEventW(0, 0, 0, 0);
    handoff.release_object = CreateEventW(0, 0, 0, 0);
    CHECK(4, handoff.holding_object && handoff.release_object);
    Handle thread = CreateThread(0, 0, worker, &handoff, 0, 0);
    CHECK(5, thread);
    CHECK(6, WaitForSingleObject(handoff.holding_object, WAIT_INFINITE) == 0);
    CHECK(7, DllCanUnloadNow() == S_FALSE);
    CHECK(8, release(object) == 0);
    CHECK(9, DllCanUnloadNow() == S_FALSE);
    CHECK(10, SetEvent(handoff.release_object));
    CHECK(11, WaitForSingleObject(thread, WAIT_INFINITE) == 0);
    u32 status = ~0u;
    CHECK(12, GetExitCodeThread(thread, &status) && status == 0);
    CHECK(13, DllCanUnloadNow() == S_OK);
    CHECK(14, CloseHandle(thread) && CloseHandle(handoff.holding_object)
        && CloseHandle(handoff.release_object));

    // Finishing the worker must leave the entry thread's original STA live.
    object = 0;
    CHECK(15, activate(&object) == S_OK && object);
    CHECK(16, release(object) == 0);
    CoUninitialize();
    object = &handoff;
    CHECK(17, activate(&object) == CO_E_NOTINITIALIZED && !object);
    CHECK(18, DllCanUnloadNow() == S_OK);
    ExitProcess(0);
}
