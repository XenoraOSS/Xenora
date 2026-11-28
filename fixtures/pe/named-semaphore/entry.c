// SPDX-License-Identifier: GPL-3.0-or-later

// Original PE32+ guest. Each failed expectation returns its source line.
typedef unsigned int u32;
typedef unsigned short u16;
typedef void *handle;

#define IMPORT __declspec(dllimport)
IMPORT handle CreateSemaphoreA(void *, int, int, const char *);
IMPORT handle CreateSemaphoreW(void *, int, int, const u16 *);
IMPORT handle CreateSemaphoreExW(void *, int, int, const u16 *, u32, u32);
IMPORT handle OpenSemaphoreA(u32, int, const char *);
IMPORT handle OpenSemaphoreW(u32, int, const u16 *);
IMPORT int ReleaseSemaphore(handle, int, int *);
IMPORT u32 WaitForSingleObject(handle, u32);
IMPORT int DuplicateHandle(handle, handle, handle, handle *, u32, int, u32);
IMPORT int CloseHandle(handle);
IMPORT u32 GetLastError(void);
IMPORT void ExitProcess(u32);

#define CHECK(condition) do { if (!(condition)) ExitProcess(__LINE__); } while (0)
#define MODIFY_STATE 2u
#define SYNCHRONIZE 0x00100000u
#define BOTH (MODIFY_STATE | SYNCHRONIZE)
#define WAIT_OBJECT_0 0u
#define WAIT_TIMEOUT 258u
#define WAIT_FAILED 0xffffffffu
#define ERROR_FILE_NOT_FOUND 2u
#define ERROR_ACCESS_DENIED 5u
#define ERROR_NOT_SUPPORTED 50u
#define ERROR_ALREADY_EXISTS 183u
#define ERROR_TOO_MANY_POSTS 298u
#define CURRENT_PROCESS ((handle)-1)
#define DUPLICATE_SAME_ACCESS 2u

static const char name_a[] = "XenoraSemaphore1347";
static const u16 name_w[] = L"XenoraSemaphore1347";
static const char other_case[] = "xenoraSemaphore1347";
static const u16 unicode_w[] = L"XenoraSema\u00e9";
static const char unicode_a[] = "XenoraSema\xc3\xa9";
static const u16 scoped_name[] = L"Global\\XenoraSemaphore1347";

void entry(void) {
    CHECK(!OpenSemaphoreW(SYNCHRONIZE, 0, name_w));
    CHECK(GetLastError() == ERROR_FILE_NOT_FOUND);

    handle first = CreateSemaphoreW(0, 1, 2, name_w);
    CHECK(first && GetLastError() == 0);
    handle ansi = CreateSemaphoreA(0, -1, 0, name_a);
    CHECK(ansi && GetLastError() == ERROR_ALREADY_EXISTS);
    handle ex_wait = CreateSemaphoreExW(0, 3, 4, name_w, 0, SYNCHRONIZE);
    CHECK(ex_wait && GetLastError() == ERROR_ALREADY_EXISTS);
    // These cases exercise the bounded Xenora namespace and UTF-8 A mapping.
    handle unicode = CreateSemaphoreW(0, 1, 1, unicode_w);
    CHECK(unicode && GetLastError() == 0);
    handle unicode_wait = OpenSemaphoreA(SYNCHRONIZE, 0, unicode_a);
    CHECK(unicode_wait);
    CHECK(WaitForSingleObject(unicode_wait, 0) == WAIT_OBJECT_0);
    CHECK(WaitForSingleObject(unicode, 0) == WAIT_TIMEOUT);
    CHECK(CloseHandle(unicode_wait));
    CHECK(CloseHandle(unicode));

    CHECK(!OpenSemaphoreW(SYNCHRONIZE, 1, name_w));
    CHECK(GetLastError() == ERROR_NOT_SUPPORTED);
    CHECK(!OpenSemaphoreW(SYNCHRONIZE, 0, scoped_name));
    CHECK(GetLastError() == ERROR_NOT_SUPPORTED);
    CHECK(!CreateSemaphoreW(0, 0, 1, scoped_name));
    CHECK(GetLastError() == ERROR_NOT_SUPPORTED);

    handle open_modify = OpenSemaphoreA(MODIFY_STATE, 0, name_a);
    CHECK(open_modify);
    handle open_wait = OpenSemaphoreW(SYNCHRONIZE, 0, name_w);
    CHECK(open_wait);

    CHECK(!OpenSemaphoreA(SYNCHRONIZE, 0, other_case));
    CHECK(GetLastError() == ERROR_FILE_NOT_FOUND);
    handle other = CreateSemaphoreA(0, 1, 1, other_case);
    CHECK(other);
    CHECK(WaitForSingleObject(ex_wait, 0) == WAIT_OBJECT_0);
    CHECK(WaitForSingleObject(open_wait, 0) == WAIT_TIMEOUT);
    CHECK(WaitForSingleObject(other, 0) == WAIT_OBJECT_0);
    CHECK(CloseHandle(other));

    int previous = -1;
    CHECK(!ReleaseSemaphore(ex_wait, 1, &previous));
    CHECK(GetLastError() == ERROR_ACCESS_DENIED && previous == -1);
    CHECK(WaitForSingleObject(open_modify, 0) == WAIT_FAILED);
    CHECK(GetLastError() == ERROR_ACCESS_DENIED);
    CHECK(ReleaseSemaphore(open_modify, 2, &previous) && previous == 0);
    CHECK(!ReleaseSemaphore(ansi, 1, &previous));
    CHECK(GetLastError() == ERROR_TOO_MANY_POSTS && previous == 0);
    CHECK(WaitForSingleObject(ansi, 0) == WAIT_OBJECT_0);
    CHECK(WaitForSingleObject(first, 0) == WAIT_OBJECT_0);
    CHECK(WaitForSingleObject(open_wait, 0) == WAIT_TIMEOUT);

    handle duplicate = 0;
    CHECK(DuplicateHandle(CURRENT_PROCESS, open_modify, CURRENT_PROCESS,
                          &duplicate, 0, 0, DUPLICATE_SAME_ACCESS));
    CHECK(CloseHandle(first));
    CHECK(CloseHandle(ansi));
    CHECK(CloseHandle(ex_wait));
    CHECK(CloseHandle(open_modify));
    CHECK(CloseHandle(open_wait));

    CHECK(ReleaseSemaphore(duplicate, 1, &previous) && previous == 0);
    handle survivor = OpenSemaphoreW(BOTH, 0, name_w);
    CHECK(survivor);
    CHECK(WaitForSingleObject(survivor, 0) == WAIT_OBJECT_0);
    CHECK(WaitForSingleObject(survivor, 0) == WAIT_TIMEOUT);
    CHECK(CloseHandle(duplicate));
    CHECK(CloseHandle(survivor));
    CHECK(!OpenSemaphoreA(BOTH, 0, name_a));
    CHECK(GetLastError() == ERROR_FILE_NOT_FOUND);

    handle fresh = CreateSemaphoreExW(0, 3, 3, name_w, 0, BOTH);
    CHECK(fresh);
    CHECK(WaitForSingleObject(fresh, 0) == WAIT_OBJECT_0);
    CHECK(WaitForSingleObject(fresh, 0) == WAIT_OBJECT_0);
    CHECK(WaitForSingleObject(fresh, 0) == WAIT_OBJECT_0);
    CHECK(WaitForSingleObject(fresh, 0) == WAIT_TIMEOUT);
    CHECK(CloseHandle(fresh));
    ExitProcess(0);
}
