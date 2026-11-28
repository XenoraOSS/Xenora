// SPDX-License-Identifier: GPL-3.0-or-later

typedef unsigned int u32;
typedef void *Handle;

__declspec(dllimport) Handle AcquireFixtureEvent(void);
__declspec(dllimport) u32 WaitForSingleObject(Handle event, u32 milliseconds);
__declspec(dllimport) int SetEvent(Handle event);
__declspec(dllimport) int CloseHandle(Handle handle);
__declspec(dllimport) u32 GetLastError(void);
__declspec(dllimport) __declspec(noreturn) void ExitProcess(u32 status);

enum {
    WAIT_OBJECT_0 = 0,
    WAIT_TIMEOUT = 258,
    ERROR_INVALID_HANDLE = 6,
};

void entry(void) {
    Handle event = AcquireFixtureEvent();
    if (event == 0 || AcquireFixtureEvent() != 0)
        ExitProcess(11);
    if (WaitForSingleObject(event, 0) != WAIT_TIMEOUT)
        ExitProcess(12);
    if (!SetEvent(event) || WaitForSingleObject(event, 0) != WAIT_OBJECT_0)
        ExitProcess(13);

#if LEAVE_EVENT_OPEN
    ExitProcess(37);
#else
    if (!CloseHandle(event))
        ExitProcess(14);
    if (WaitForSingleObject(event, 0) != 0xffffffffU ||
        GetLastError() != ERROR_INVALID_HANDLE)
        ExitProcess(15);
    ExitProcess(0);
#endif
}
