// SPDX-License-Identifier: GPL-3.0-or-later

typedef unsigned int u32;
typedef unsigned short u16;
typedef unsigned long long u64;

#define IMPORT __declspec(dllimport)

IMPORT u32 XenoraNextEvent(void);
IMPORT u32 XenoraPresent(u32 sequence);
IMPORT void XenoraFinish(u32 frames);

IMPORT u64 CreateFileW(const u16 *path, u32 access, u32 share, void *security,
                       u32 disposition, u32 flags, u64 template);
IMPORT int WriteFile(u64 file, const void *bytes, u32 size, u32 *written, void *overlapped);
IMPORT int ReadFile(u64 file, void *bytes, u32 size, u32 *read, void *overlapped);
IMPORT int SetFilePointerEx(u64 file, long long distance, u64 *position, u32 origin);
IMPORT u64 CreateEventW(void *security, int manual_reset, int initial, const u16 *name);
IMPORT int SetEvent(u64 event);
IMPORT u32 WaitForSingleObject(u64 handle, u32 milliseconds);
IMPORT int CloseHandle(u64 handle);
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
#define WAIT_TIMEOUT 258
#define INVALID_HANDLE (~0ull)

int _fltused;

// Both triangles cover exactly one column of sample centers in the target.
static const float vertices[2][3][4] = {
    {{-1.0f, -10.0f, 0.5f, 1.0f}, {-0.3f, 0.0f, 0.5f, 1.0f}, {-1.0f, 10.0f, 0.5f, 1.0f}},
    {{0.5f, -10.0f, 0.5f, 1.0f}, {1.2f, 0.0f, 0.5f, 1.0f}, {0.5f, 10.0f, 0.5f, 1.0f}},
};
static const u16 log_path[] = {
    'C', ':', '\\', 'x', 'e', 'n', 'o', 'r', 'a', '-', 's', 'e', 's', 's', 'i', 'o', 'n', '.', 'l', 'o', 'g', 0,
};
static u32 frame[WIDTH * HEIGHT];
static unsigned char events[MAX_EVENTS];
static unsigned char logged[MAX_EVENTS + 1];

static void require(int condition, u32 status) {
    if (!condition)
        ExitProcess(status);
}

static u64 record(u64 device, u64 allocator, u64 buffer, u64 target, u32 direction) {
    u64 list;
    require(XenoraCreateCommandList(device, allocator, 0, &list) == S_OK, 4);
    require(XenoraSetVertexBuffer(list, 0, buffer, direction * sizeof(vertices[0]), sizeof(vertices[0][0])) == S_OK, 4);
    require(XenoraSetRenderTarget(list, target, CLEAR) == S_OK, 4);
    require(XenoraSetViewport(list, WIDTH, HEIGHT) == S_OK, 4);
    require(XenoraDraw(list, 3, 1, 0) == S_OK, 4);
    require(XenoraCloseCommandList(list) == S_OK, 4);
    return list;
}

void entry(void) {
    u64 file = CreateFileW(log_path, GENERIC_READ_WRITE, 0, 0, CREATE_ALWAYS, 0, 0);
    require(file != INVALID_HANDLE && file != 0, 1);
    u64 latch = CreateEventW(0, 1, 0, 0);
    require(latch != 0 && WaitForSingleObject(latch, 0) == WAIT_TIMEOUT, 2);

    u64 device, queue, allocator, buffer, target;
    require(XenoraCreateDevice(&device) == S_OK, 3);
    require(XenoraCreateCommandQueue(device, 0, &queue) == S_OK, 3);
    require(XenoraCreateCommandAllocator(device, 0, &allocator) == S_OK, 3);
    require(XenoraCreateBuffer(device, (u64)vertices, sizeof(vertices), &buffer) == S_OK, 3);
    require(XenoraCreateRenderTarget(device, WIDTH, HEIGHT, &target) == S_OK, 3);
    u64 left = record(device, allocator, buffer, target, 0);
    u64 right = record(device, allocator, buffer, target, 1);

    u32 frames = 0;
    for (;;) {
        u32 event = XenoraNextEvent();
        if (event == 0)
            break;
        require((event == 1 || event == 2) && frames < MAX_EVENTS, 5);
        if (frames == 0)
            require(SetEvent(latch), 2);
        require(WaitForSingleObject(latch, 0) == WAIT_OBJECT_0, 2);

        events[frames] = event == 1 ? 'L' : 'R';
        u32 count;
        require(WriteFile(file, &events[frames], 1, &count, 0) && count == 1, 6);
        require(XenoraExecuteCommandLists(queue, event == 1 ? left : right) == S_OK, 7);
        require(XenoraReadbackResource(target, (u64)frame, sizeof(frame)) == S_OK, 7);
        u32 info[4];
        require(XenoraGetLastFrameInfo(info) == S_OK && info[1] == WIDTH && info[2] == HEIGHT && info[3] == 1, 7);

        u32 red_column = event == 1 ? 0 : WIDTH - 1;
        for (u32 i = 0; i < WIDTH * HEIGHT; i++)
            require(frame[i] == (i % WIDTH == red_column ? RED : CLEAR), 8);
        frames++;
        require(XenoraPresent(frames) == 1, 9);
    }

    u32 count;
    require(SetFilePointerEx(file, 0, 0, FILE_BEGIN), 10);
    require(ReadFile(file, logged, sizeof(logged), &count, 0) && count == frames, 10);
    for (u32 i = 0; i < frames; i++)
        require(logged[i] == events[i], 10);

    require(XenoraDestroyCommandList(right) == S_OK, 11);
    require(XenoraDestroyCommandList(left) == S_OK, 11);
    require(XenoraDestroyResource(target) == S_OK, 11);
    require(XenoraDestroyResource(buffer) == S_OK, 11);
    require(XenoraDestroyCommandAllocator(allocator) == S_OK, 11);
    require(XenoraDestroyCommandQueue(queue) == S_OK, 11);
    require(XenoraDestroyDevice(device) == S_OK, 11);
    require(CloseHandle(latch) && CloseHandle(file), 12);
    XenoraFinish(frames);
    ExitProcess(0);
}
