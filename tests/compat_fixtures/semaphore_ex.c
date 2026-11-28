// SPDX-License-Identifier: GPL-3.0-or-later
//
// The Win32 semaphore and access-rights pages define count consumption,
// SEMAPHORE_MODIFY_STATE for release and SYNCHRONIZE for wait. Each failed
// assertion exits with its source line. The named case uses the guest's
// process namespace and default security attributes.

typedef unsigned int u32;
typedef unsigned short u16;
typedef void *handle;

#define IMPORT __declspec(dllimport)
IMPORT handle CreateSemaphoreExA(void *, int, int, const char *, u32, u32);
IMPORT handle CreateSemaphoreExW(void *, int, int, const u16 *, u32, u32);
IMPORT int ReleaseSemaphore(handle, int, int *);
IMPORT u32 WaitForSingleObject(handle, u32);
IMPORT int CloseHandle(handle);
IMPORT u32 GetLastError(void);
IMPORT void ExitProcess(u32);

#define SYNCHRONIZE 0x00100000u
#define SEMAPHORE_MODIFY_STATE 2u
#define WAIT_OBJECT_0 0u
#define WAIT_TIMEOUT 258u
#define WAIT_FAILED 0xffffffffu
#define ERROR_ACCESS_DENIED 5u
#define ERROR_INVALID_HANDLE 6u
#define ERROR_NOT_SUPPORTED 50u
#define ERROR_INVALID_PARAMETER 87u
#define ERROR_TOO_MANY_POSTS 298u

#define CHECK(condition) do { if (!(condition)) ExitProcess(__LINE__); } while (0)

void start(void) {
    handle wait_only = CreateSemaphoreExA(0, 1, 2, 0, 0, SYNCHRONIZE);
    CHECK(wait_only);
    int previous = -1;
    CHECK(!ReleaseSemaphore(wait_only, 1, &previous));
    CHECK(GetLastError() == ERROR_ACCESS_DENIED && previous == -1);
    CHECK(WaitForSingleObject(wait_only, 0) == WAIT_OBJECT_0);
    CHECK(WaitForSingleObject(wait_only, 0) == WAIT_TIMEOUT);
    CHECK(CloseHandle(wait_only));
    CHECK(WaitForSingleObject(wait_only, 0) == WAIT_FAILED);
    CHECK(GetLastError() == ERROR_INVALID_HANDLE);

    handle release_only = CreateSemaphoreExW(0, 0, 2, 0, 0, SEMAPHORE_MODIFY_STATE);
    CHECK(release_only);
    CHECK(WaitForSingleObject(release_only, 0) == WAIT_FAILED);
    CHECK(GetLastError() == ERROR_ACCESS_DENIED);
    CHECK(ReleaseSemaphore(release_only, 2, &previous) && previous == 0);
    CHECK(WaitForSingleObject(release_only, 0) == WAIT_FAILED);
    CHECK(GetLastError() == ERROR_ACCESS_DENIED);
    CHECK(CloseHandle(release_only));

    handle both = CreateSemaphoreExW(0, 0, 2, 0, 0, SYNCHRONIZE | SEMAPHORE_MODIFY_STATE);
    CHECK(both);
    CHECK(WaitForSingleObject(both, 0) == WAIT_TIMEOUT);
    CHECK(ReleaseSemaphore(both, 2, &previous) && previous == 0);
    CHECK(!ReleaseSemaphore(both, 1, &previous));
    CHECK(GetLastError() == ERROR_TOO_MANY_POSTS && previous == 0);
    CHECK(WaitForSingleObject(both, 0) == WAIT_OBJECT_0);
    CHECK(WaitForSingleObject(both, 0) == WAIT_OBJECT_0);
    CHECK(WaitForSingleObject(both, 0) == WAIT_TIMEOUT);
    CHECK(CloseHandle(both));

    CHECK(!CreateSemaphoreExA(0, -1, 2, 0, 0, SYNCHRONIZE));
    CHECK(GetLastError() == ERROR_INVALID_PARAMETER);
    CHECK(!CreateSemaphoreExW(0, 0, 0, 0, 0, SYNCHRONIZE));
    CHECK(GetLastError() == ERROR_INVALID_PARAMETER);
    CHECK(!CreateSemaphoreExA(0, 3, 2, 0, 0, SYNCHRONIZE));
    CHECK(GetLastError() == ERROR_INVALID_PARAMETER);
    CHECK(!CreateSemaphoreExW(0, 0, 2, 0, 1, SYNCHRONIZE));
    CHECK(GetLastError() == ERROR_INVALID_PARAMETER);
    handle named = CreateSemaphoreExA(0, 1, 2, "name", 0, SYNCHRONIZE);
    CHECK(named);
    CHECK(WaitForSingleObject(named, 0) == WAIT_OBJECT_0);
    CHECK(WaitForSingleObject(named, 0) == WAIT_TIMEOUT);
    CHECK(CloseHandle(named));
    CHECK(!CreateSemaphoreExW(&previous, 0, 2, 0, 0, SYNCHRONIZE));
    CHECK(GetLastError() == ERROR_NOT_SUPPORTED);
    CHECK(!CreateSemaphoreExA(0, 0, 2, 0, 0, 0x80000000u));
    CHECK(GetLastError() == ERROR_NOT_SUPPORTED);

    ExitProcess(0);
}
