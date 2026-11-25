// SPDX-License-Identifier: GPL-3.0-or-later
//
// An original C program with no C library linked that uses the process and
// thread lifecycle services: the exit code of its own process, the bounds of
// its threads' stacks, thread group affinity, the exit code of a thread it
// created, and TerminateProcess on itself.
//
// Each check has a number. A failed check ends the process with that number;
// a run in which every check passed ends with STATUS through TerminateProcess.
// The stack bounds of both threads are saved on the synthetic C: drive for the
// test to compare with the layout it gave the process.
//
// The expected values come from Microsoft Learn's pages for each function:
// STILL_ACTIVE (259) while a process or thread runs, ERROR_INVALID_HANDLE (6)
// for a handle that names no process, ERROR_INVALID_PARAMETER (87) for an
// affinity naming a processor or group that does not exist.
//
// Build with tests/compat_fixtures/build-process-lifecycle.sh. The test that
// runs it is crates/loader/tests/process_lifecycle.rs.

typedef unsigned long long u64;
typedef unsigned int u32;
typedef unsigned short u16;

typedef struct {
    u64 mask;
    u16 group;
    u16 reserved[3];
} GROUP_AFFINITY;

#define IMPORT __declspec(dllimport)
IMPORT void *GetCurrentProcess(void);
IMPORT void *GetCurrentThread(void);
IMPORT int TerminateProcess(void *process, u32 status);
IMPORT int GetExitCodeProcess(void *process, u32 *code);
IMPORT int GetExitCodeThread(void *thread, u32 *code);
IMPORT void GetCurrentThreadStackLimits(u64 *low, u64 *high);
IMPORT int GetThreadGroupAffinity(void *thread, GROUP_AFFINITY *affinity);
IMPORT int SetThreadGroupAffinity(void *thread, const GROUP_AFFINITY *affinity,
                                  GROUP_AFFINITY *previous);
IMPORT u32 GetLastError(void);
IMPORT void SetLastError(u32 code);
IMPORT void *CreateThread(void *security, u64 stack, u32 (*start)(void *), void *parameter,
                          u32 flags, u32 *id);
IMPORT u32 ResumeThread(void *thread);
IMPORT u32 WaitForSingleObject(void *object, u32 milliseconds);
IMPORT int CloseHandle(void *object);
IMPORT void *CreateFileW(const u16 *name, u32 access, u32 share, void *security,
                         u32 disposition, u32 flags, void *template_file);
IMPORT int WriteFile(void *file, const void *data, u32 size, u32 *written, void *overlapped);
IMPORT void ExitProcess(u32 status);

#define STATUS 0x7E57u
#define STILL_ACTIVE 259u
#define WORKER_CODE 0x51u
#define ERROR_INVALID_HANDLE 6u
#define ERROR_INVALID_PARAMETER 87u
#ifdef LIFECYCLE_BREAK
// Control: a wrong expectation, so the first check must fail.
#define EXPECTED_ACTIVE (STILL_ACTIVE + 1)
#else
#define EXPECTED_ACTIVE STILL_ACTIVE
#endif

static u32 checks;

static void check(int holds) {
    checks++;
    if (!holds) ExitProcess(checks);
}

// Stack bounds: main low, main high, worker low, worker high.
static u64 bounds[4];

// A worker's own local lies inside the bounds it reports.
static u32 worker(void *parameter) {
    volatile u64 local = (u64)parameter;
    GetCurrentThreadStackLimits(&bounds[2], &bounds[3]);
    if (!(bounds[2] < (u64)&local && (u64)&local < bounds[3])) return 1;
    GROUP_AFFINITY affinity = {0};
    if (!GetThreadGroupAffinity(GetCurrentThread(), &affinity)) return 2;
    if (affinity.mask != 1 || affinity.group != 0) return 3;
    return (u32)local;
}

void start(void) {
    u32 code = 0;

    // 1-2: the running process reports STILL_ACTIVE for itself.
    check(GetExitCodeProcess(GetCurrentProcess(), &code));
    check(code == EXPECTED_ACTIVE);

    // 3-6: no handle other than the pseudo-handle names a process.
    SetLastError(0);
    check(!GetExitCodeProcess((void *)0x1234, &code));
    check(GetLastError() == ERROR_INVALID_HANDLE);
    SetLastError(0);
    check(!TerminateProcess((void *)0, 9));
    check(GetLastError() == ERROR_INVALID_HANDLE);

    // 7-9: the main thread's stack holds this local, and is nonempty.
    volatile u32 local = 0;
    GetCurrentThreadStackLimits(&bounds[0], &bounds[1]);
    check(bounds[0] < bounds[1]);
    check(bounds[0] < (u64)&local);
    check((u64)&local < bounds[1]);

    // 10-12: one processor in group 0.
    GROUP_AFFINITY affinity = {.mask = ~0ull, .group = 7};
    check(GetThreadGroupAffinity(GetCurrentThread(), &affinity));
    check(affinity.mask == 1);
    check(affinity.group == 0 && !affinity.reserved[0]);

    // 13-15: setting the same affinity succeeds and returns the previous one.
    GROUP_AFFINITY previous = {.mask = 0, .group = 3};
    check(SetThreadGroupAffinity(GetCurrentThread(), &affinity, &previous));
    check(previous.mask == 1 && previous.group == 0);
    check(SetThreadGroupAffinity(GetCurrentThread(), &affinity, 0));

    // 16-21: a processor or group that does not exist is refused.
    GROUP_AFFINITY wrong = {.mask = 2};
    SetLastError(0);
    check(!SetThreadGroupAffinity(GetCurrentThread(), &wrong, 0));
    check(GetLastError() == ERROR_INVALID_PARAMETER);
    wrong.mask = 1;
    wrong.group = 1;
    SetLastError(0);
    check(!SetThreadGroupAffinity(GetCurrentThread(), &wrong, 0));
    check(GetLastError() == ERROR_INVALID_PARAMETER);
    wrong.group = 0;
    wrong.mask = 0;
    SetLastError(0);
    check(!SetThreadGroupAffinity(GetCurrentThread(), &wrong, 0));
    check(GetLastError() == ERROR_INVALID_PARAMETER);

    // 22-27: a worker thread, created suspended (4), its exit code before and after it ends, and its
    // affinity through its handle.
    void *thread = CreateThread(0, 0, worker, (void *)(u64)WORKER_CODE, 4, 0);
    check(thread != 0);
    check(GetExitCodeThread(thread, &code) && code == STILL_ACTIVE);
    check(GetThreadGroupAffinity(thread, &affinity) && affinity.mask == 1);
    check(ResumeThread(thread) == 1);
    check(WaitForSingleObject(thread, 0xFFFFFFFFu) == 0);
    check(GetExitCodeThread(thread, &code) && code == WORKER_CODE);

    // 28-30: the two stacks are distinct.
    check(bounds[2] < bounds[3]);
    check(bounds[3] <= bounds[0] || bounds[1] <= bounds[2]);
    check(CloseHandle(thread));

    // 31-32: save the bounds.
    static const u16 name[] = {'C', ':', '\\', 's', 't', 'a', 'c', 'k', 's', '.', 'b', 'i', 'n', 0};
    void *file = CreateFileW(name, 0x40000000u, 0, 0, 2, 0, 0);
    check(file != (void *)-1);
    u32 written = 0;
    check(WriteFile(file, bounds, sizeof bounds, &written, 0) && written == sizeof bounds);
    CloseHandle(file);

    // TerminateProcess on itself does not return.
    TerminateProcess(GetCurrentProcess(), STATUS);
    ExitProcess(0xDEAD);
}
