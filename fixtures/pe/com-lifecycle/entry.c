// SPDX-License-Identifier: GPL-3.0-or-later
#include "interfaces.h"

__declspec(dllimport) HResult CoInitializeEx(void *, U32);
__declspec(dllimport) void CoUninitialize(void);
__declspec(dllimport) HResult DllCanUnloadNow(void);
__declspec(dllimport) HResult AcquireFactory(void **);
__declspec(dllimport) HResult ActivateObject(void **);
__declspec(dllimport) U32 ConsumeObject(Unknown *);
__declspec(dllimport) Handle CreateEventW(void *, int, int, const unsigned short *);
__declspec(dllimport) int SetEvent(Handle);
__declspec(dllimport) Handle CreateThread(void *, unsigned long long, U32 (*)(void *), void *, U32, U32 *);
__declspec(dllimport) U32 WaitForSingleObject(Handle, U32);
__declspec(dllimport) int GetExitCodeThread(Handle, U32 *);
__declspec(dllimport) int CloseHandle(Handle);
__declspec(dllimport) __declspec(noreturn) void ExitProcess(U32);

enum { COINIT_MTA = 0, COINIT_STA = 2 };
#define WAIT_INFINITE 0xffffffffu
#define REQUIRE(status, condition) do { if (!(condition)) ExitProcess(status); } while (0)

typedef struct Gates {
    Handle active;
    Handle release;
} Gates;

static U32 worker(void *argument) {
    const Gates *gates = argument;
    void *result = argument;
    REQUIRE(31, ActivateObject(&result) == CO_E_NOTINITIALIZED && !result);
    REQUIRE(32, CoInitializeEx(0, COINIT_MTA) == S_OK);
    REQUIRE(33, CoInitializeEx(0, COINIT_MTA) == S_FALSE);
    REQUIRE(34, CoInitializeEx(0, COINIT_STA) == RPC_E_CHANGED_MODE);
    CoUninitialize();
    REQUIRE(35, ActivateObject(&result) == S_OK && result);
    Unknown *object = result;

    REQUIRE(36, SetEvent(gates->active));
    REQUIRE(37, WaitForSingleObject(gates->release, WAIT_INFINITE) == 0);
    REQUIRE(38, object->vtable->release(object) == 0);
    REQUIRE(39, DllCanUnloadNow() == S_OK);

    CoUninitialize();
    result = argument;
    REQUIRE(40, ActivateObject(&result) == CO_E_NOTINITIALIZED && !result);
    return 0;
}

void entry(void) {
    REQUIRE(1, DllCanUnloadNow() == S_OK);
    void *result = &result;
    REQUIRE(2, AcquireFactory(&result) == CO_E_NOTINITIALIZED && !result);
    REQUIRE(3, CoInitializeEx(0, COINIT_STA) == S_OK);
    REQUIRE(4, AcquireFactory(&result) == S_OK && result);
    Factory *factory = result;
    REQUIRE(5, factory->vtable->lock(factory, 1) == S_OK);
    REQUIRE(6, factory->vtable->create(factory, 0, &iid_unknown, &result) == S_OK && result);
    Unknown *object = result;

    Gates gates;
    gates.active = CreateEventW(0, 0, 0, 0);
    gates.release = CreateEventW(0, 0, 0, 0);
    REQUIRE(7, gates.active && gates.release);
    Handle thread = CreateThread(0, 0, worker, &gates, 0, 0);
    REQUIRE(8, thread);
    REQUIRE(9, WaitForSingleObject(gates.active, WAIT_INFINITE) == 0);
    REQUIRE(10, DllCanUnloadNow() == S_FALSE);
    U32 consumed = ConsumeObject(object);
    if (consumed)
        ExitProcess(50 + consumed);

    REQUIRE(11, factory->vtable->lock(factory, 0) == S_OK);
    REQUIRE(12, factory->vtable->release(factory) == 0);
    REQUIRE(13, DllCanUnloadNow() == S_FALSE);
    REQUIRE(14, SetEvent(gates.release));
    REQUIRE(15, WaitForSingleObject(thread, WAIT_INFINITE) == 0);
    U32 status = ~0u;
    REQUIRE(16, GetExitCodeThread(thread, &status) && status == 0);
    REQUIRE(17, CloseHandle(thread) && CloseHandle(gates.active) && CloseHandle(gates.release));

    // Worker teardown cannot affect the main thread's original STA.
    REQUIRE(18, ActivateObject(&result) == S_OK && result);
    object = result;
    REQUIRE(19, object->vtable->release(object) == 0);

    // The synthetic server retains its lock after all references retire.
    // Its stable class ID allows this fixture to reacquire the factory.
    REQUIRE(22, AcquireFactory(&result) == S_OK && result);
    factory = result;
    REQUIRE(23, factory->vtable->lock(factory, 1) == S_OK);
    REQUIRE(24, factory->vtable->release(factory) == 0);
    REQUIRE(25, DllCanUnloadNow() == S_FALSE);
    REQUIRE(26, AcquireFactory(&result) == S_OK && result);
    factory = result;
    REQUIRE(27, factory->vtable->lock(factory, 0) == S_OK);
    REQUIRE(28, factory->vtable->release(factory) == 0);
    CoUninitialize();
    result = &gates;
    REQUIRE(20, AcquireFactory(&result) == CO_E_NOTINITIALIZED && !result);
    REQUIRE(21, DllCanUnloadNow() == S_OK);
    ExitProcess(0);
}
