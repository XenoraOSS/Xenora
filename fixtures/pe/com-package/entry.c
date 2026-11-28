// SPDX-License-Identifier: GPL-3.0-or-later
#include "interfaces.h"

__declspec(dllimport) HResult CoInitializeEx(void *, U32);
__declspec(dllimport) void CoUninitialize(void);
__declspec(dllimport) HResult DllCanUnloadNow(void);
__declspec(dllimport) HResult ProduceObject(void **);
__declspec(dllimport) U32 InspectObject(Unknown *);
__declspec(dllimport) U32 ReleaseObject(Unknown *);
__declspec(dllimport) __declspec(noreturn) void ExitProcess(U32);

void entry(void) {
    if (CoInitializeEx(0, 0) != S_OK)
        ExitProcess(1);
    if (DllCanUnloadNow() != S_OK)
        ExitProcess(2);
    void *object = 0;
    if (ProduceObject(&object) != S_OK || !object)
        ExitProcess(3);
    if (DllCanUnloadNow() != S_FALSE)
        ExitProcess(4);
    U32 inspection = InspectObject(object);
    if (inspection != 0)
        ExitProcess(10 + inspection);
    if (DllCanUnloadNow() != S_FALSE)
        ExitProcess(5);
    if (ReleaseObject(object) != 0)
        ExitProcess(6);
    if (DllCanUnloadNow() != S_OK)
        ExitProcess(7);
    CoUninitialize();
    ExitProcess(0);
}
