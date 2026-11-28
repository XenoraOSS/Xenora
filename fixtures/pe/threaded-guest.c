// SPDX-License-Identifier: GPL-3.0-or-later

typedef unsigned int u32;
typedef unsigned short u16;
typedef unsigned long long u64;

#define IMPORT __declspec(dllimport)

IMPORT u32 XenoraNextSceneCommand(void);
IMPORT u32 XenoraReportSceneFrame(u32 sequence, u32 worker_id, u32 column, u64 job_address);
IMPORT u32 XenoraReportSceneCleanup(u32 worker_id, u32 worker_status, u64 released_job_address, u32 journal_length);

IMPORT u64 CreateFileW(const u16 *path, u32 access, u32 share, void *security,
                       u32 disposition, u32 flags, u64 template);
IMPORT int WriteFile(u64 file, const void *bytes, u32 size, u32 *written, void *overlapped);
IMPORT int ReadFile(u64 file, void *bytes, u32 size, u32 *read, void *overlapped);
IMPORT int SetFilePointerEx(u64 file, long long distance, u64 *position, u32 origin);
IMPORT u64 CreateEventW(void *security, int manual_reset, int initial, const u16 *name);
IMPORT int SetEvent(u64 event);
IMPORT u32 WaitForSingleObject(u64 handle, u32 milliseconds);
IMPORT u32 SignalObjectAndWait(u64 signal, u64 wait, u32 milliseconds, int alertable);
IMPORT int CloseHandle(u64 handle);
IMPORT u64 CreateThread(void *security, u64 stack_size, u32 (*entry)(void *),
                        void *parameter, u32 flags, u32 *thread_id);
IMPORT u32 GetCurrentThreadId(void);
IMPORT u32 GetExitCodeThread(u64 thread, u32 *status);
IMPORT u32 GetLastError(void);
IMPORT void *VirtualAlloc(void *address, u64 size, u32 kind, u32 protection);
IMPORT int VirtualFree(void *address, u64 size, u32 kind);
IMPORT __declspec(noreturn) void ExitProcess(u32 status);

IMPORT int XenoraCreateDevice(u64 *out);
IMPORT int XenoraCreateCommandQueue(u64 device, u32 type, u64 *out);
IMPORT int XenoraCreateCommandAllocator(u64 device, u32 type, u64 *out);
IMPORT int XenoraCreateCommandList(u64 device, u64 allocator, u32 type, u64 *out);
IMPORT int XenoraCreateBuffer(u64 device, u64 address, u64 size, u64 *out);
IMPORT int XenoraCreateRenderTarget(u64 device, u32 width, u32 height, u64 *out);
IMPORT int XenoraSetVertexBuffer(u64 list, u32 slot, u64 buffer, u64 offset, u64 stride);
IMPORT int XenoraSetRenderTarget(u64 list, u64 resource, u32 clear);
IMPORT int XenoraSetViewport(u64 list, u32 width, u32 height);
IMPORT int XenoraDraw(u64 list, u32 vertices, u32 instances, u32 first_vertex);
IMPORT int XenoraCloseCommandList(u64 list);
IMPORT int XenoraExecuteCommandLists(u64 queue, u64 list);
IMPORT int XenoraReadbackResource(u64 resource, u64 address, u64 size);
IMPORT int XenoraGetLastFrameInfo(u32 *out);
IMPORT int XenoraDestroyResource(u64 resource);
IMPORT int XenoraDestroyCommandList(u64 list);
IMPORT int XenoraDestroyCommandAllocator(u64 allocator);
IMPORT int XenoraDestroyCommandQueue(u64 queue);
IMPORT int XenoraDestroyDevice(u64 device);

#define S_OK 0
#define WIDTH 4
#define HEIGHT 3
#define MAX_EVENTS 32
#define CLEAR 0xFB5D2B13u
#define RED 0xFF0000FFu
#define GENERIC_READ_WRITE 0xC0000000u
#define CREATE_ALWAYS 2
#define FILE_BEGIN 0
#define WAIT_OBJECT_0 0
#define WAIT_MS 500
#define INVALID_HANDLE (~0ull)
#define MEM_RESERVE_COMMIT 0x3000
#define PAGE_READWRITE 0x04
#define MEM_RELEASE 0x8000
#define ERROR_NOT_SUPPORTED 50
#define UNSCHEDULED_EXIT 0x20000001u
#define WORKER_EXIT 0x4C0DE12Au
#define APPLICATION_EXIT 0x73D0CA11u

int _fltused;

// Each triangle covers one column of sample centers in the target.
static const float vertices[WIDTH][3][4] = {
    {{-1.0f, -10.0f, 0.5f, 1.0f}, {-0.3f, 0.0f, 0.5f, 1.0f}, {-1.0f, 10.0f, 0.5f, 1.0f}},
    {{-0.5f, -10.0f, 0.5f, 1.0f}, {0.2f, 0.0f, 0.5f, 1.0f}, {-0.5f, 10.0f, 0.5f, 1.0f}},
    {{0.0f, -10.0f, 0.5f, 1.0f}, {0.7f, 0.0f, 0.5f, 1.0f}, {0.0f, 10.0f, 0.5f, 1.0f}},
    {{0.5f, -10.0f, 0.5f, 1.0f}, {1.2f, 0.0f, 0.5f, 1.0f}, {0.5f, 10.0f, 0.5f, 1.0f}},
};
static const u16 log_path[] = {
    'C', ':', '\\', 't', 'h', 'r', 'e', 'a', 'd', 'e', 'd', '-', 's', 'c', 'e', 'n', 'e', '.', 'l', 'o', 'g', 0,
};
static u32 frame[WIDTH * HEIGHT];
static unsigned char expected_journal[MAX_EVENTS];
static unsigned char observed_journal[MAX_EVENTS + 1];

struct scene_job {
    u64 work_ready;
    u64 work_done;
    u64 file;
    volatile u32 command;
    volatile u32 column;
    volatile u32 sequence;
    volatile u32 worker_id;
};

static void require(int condition, u32 status) {
    if (!condition)
        ExitProcess(status);
}

static u32 scene_worker(void *parameter) {
    struct scene_job *job = parameter;
    job->worker_id = GetCurrentThreadId();
    u32 column = 0;
    u32 sequence = 0;
    for (;;) {
        if (WaitForSingleObject(job->work_ready, WAIT_MS) != WAIT_OBJECT_0)
            return 0x10;
        u32 command = job->command;
        if (command == 0)
            return WORKER_EXIT;
        if ((command != 1 && command != 2) || sequence == MAX_EVENTS)
            return 0x11;
        column = (column + (command == 1 ? 1 : WIDTH - 1)) % WIDTH;
        unsigned char result = (unsigned char)('0' + column);
        u32 written;
        if (!WriteFile(job->file, &result, 1, &written, 0) || written != 1)
            return 0x12;
        job->column = column;
        job->sequence = ++sequence;
        if (!SetEvent(job->work_done))
            return 0x13;
    }
}

static u64 record(u64 device, u64 allocator, u64 buffer, u64 target, u32 column) {
    u64 list;
    require(XenoraCreateCommandList(device, allocator, 0, &list) == S_OK, 4);
    require(XenoraSetVertexBuffer(list, 0, buffer, column * sizeof(vertices[0]), sizeof(vertices[0][0])) == S_OK, 4);
    require(XenoraSetRenderTarget(list, target, CLEAR) == S_OK, 4);
    require(XenoraSetViewport(list, WIDTH, HEIGHT) == S_OK, 4);
    require(XenoraDraw(list, 3, 1, 0) == S_OK, 4);
    require(XenoraCloseCommandList(list) == S_OK, 4);
    return list;
}

void entry(void) {
    struct scene_job *job = VirtualAlloc(0, sizeof(*job), MEM_RESERVE_COMMIT, PAGE_READWRITE);
    require(job != 0, 1);
    u64 work_ready = CreateEventW(0, 0, 0, 0);
    u64 work_done = CreateEventW(0, 0, 0, 0);
    u64 file = CreateFileW(log_path, GENERIC_READ_WRITE, 0, 0, CREATE_ALWAYS, 0, 0);
    require(work_ready != 0 && work_done != 0 && file != INVALID_HANDLE && file != 0, 2);
    job->work_ready = work_ready;
    job->work_done = work_done;
    job->file = file;

    u32 main_id = GetCurrentThreadId();
    u32 worker_id = 0;
    u64 thread = CreateThread(0, 0, scene_worker, job, 0, &worker_id);
    if (thread == 0) {
        require(GetLastError() == ERROR_NOT_SUPPORTED, 0x20);
        require(CloseHandle(file) && CloseHandle(work_done) && CloseHandle(work_ready), 0x21);
        require(VirtualFree(job, 0, MEM_RELEASE), 0x22);
        ExitProcess(UNSCHEDULED_EXIT);
    }
    require(worker_id != 0 && worker_id != main_id, 0x23);

    u64 device, queue, allocator, buffer, target;
    require(XenoraCreateDevice(&device) == S_OK, 3);
    require(XenoraCreateCommandQueue(device, 0, &queue) == S_OK, 3);
    require(XenoraCreateCommandAllocator(device, 0, &allocator) == S_OK, 3);
    require(XenoraCreateBuffer(device, (u64)vertices, sizeof(vertices), &buffer) == S_OK, 3);
    require(XenoraCreateRenderTarget(device, WIDTH, HEIGHT, &target) == S_OK, 3);
    u64 lists[WIDTH];
    for (u32 column = 0; column < WIDTH; column++)
        lists[column] = record(device, allocator, buffer, target, column);

    u32 frames = 0;
    u32 expected_column = 0;
    for (;;) {
        u32 command = XenoraNextSceneCommand();
        if (command == 0)
            break;
        require((command == 1 || command == 2) && frames < MAX_EVENTS, 5);
        expected_column = command == 1
            ? (expected_column + 1 == WIDTH ? 0 : expected_column + 1)
            : (expected_column == 0 ? WIDTH - 1 : expected_column - 1);
        expected_journal[frames] = (unsigned char)('0' + expected_column);

        job->command = command;
        require(SignalObjectAndWait(work_ready, work_done, WAIT_MS, 0) == WAIT_OBJECT_0, 6);
        u32 column = job->column;
        require(job->worker_id == worker_id && job->sequence == frames + 1 && column == expected_column, 6);

        require(XenoraExecuteCommandLists(queue, lists[column]) == S_OK, 7);
        require(XenoraReadbackResource(target, (u64)frame, sizeof(frame)) == S_OK, 7);
        u32 info[4];
        require(XenoraGetLastFrameInfo(info) == S_OK && info[1] == WIDTH && info[2] == HEIGHT && info[3] == 1, 7);
        for (u32 i = 0; i < WIDTH * HEIGHT; i++)
            require(frame[i] == (i % WIDTH == column ? RED : CLEAR), 8);

        frames++;
        require(XenoraReportSceneFrame(frames, worker_id, column, (u64)job) == 1, 9);
    }

    job->command = 0;
    require(SetEvent(work_ready) && WaitForSingleObject(thread, WAIT_MS) == WAIT_OBJECT_0, 10);
    u32 worker_status;
    require(GetExitCodeThread(thread, &worker_status) && worker_status == WORKER_EXIT, 10);

    u32 count;
    require(SetFilePointerEx(file, 0, 0, FILE_BEGIN), 11);
    require(ReadFile(file, observed_journal, sizeof(observed_journal), &count, 0) && count == frames, 11);
    for (u32 i = 0; i < count; i++)
        require(observed_journal[i] == expected_journal[i], 11);
    require(CloseHandle(thread) && CloseHandle(file) && CloseHandle(work_done) && CloseHandle(work_ready), 12);
    u64 address = (u64)job;
    require(VirtualFree(job, 0, MEM_RELEASE), 13);

    for (u32 index = WIDTH; index > 0; index--)
        require(XenoraDestroyCommandList(lists[index - 1]) == S_OK, 14);
    require(XenoraDestroyResource(target) == S_OK, 14);
    require(XenoraDestroyResource(buffer) == S_OK, 14);
    require(XenoraDestroyCommandAllocator(allocator) == S_OK, 14);
    require(XenoraDestroyCommandQueue(queue) == S_OK, 14);
    require(XenoraDestroyDevice(device) == S_OK, 14);

    require(XenoraReportSceneCleanup(worker_id, worker_status, address, count) == 1, 15);
    ExitProcess(APPLICATION_EXIT);
}
