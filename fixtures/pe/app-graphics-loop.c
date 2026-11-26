// SPDX-License-Identifier: GPL-3.0-or-later

// Original PE32+ guest for the macOS fixture viewer's frame loop.
// The renderer is host-backed, but the guest owns every graphics command.

typedef unsigned int u32;
typedef unsigned short u16;
typedef unsigned long long u64;

#define IMPORT __declspec(dllimport)
#define S_OK 0
#define WIDTH 4u
#define HEIGHT 3u
#define CLEAR 0xfb5d2b13u
#define MAX_FRAMES 100000u
#define GENERIC_READ_WRITE 0xc0000000u
#define CREATE_ALWAYS 2u
#define FILE_BEGIN 0u
#define INVALID_HANDLE (~0ull)

IMPORT u32 frame_begin(u32 sequence);
IMPORT u32 input_poll(void);
IMPORT u32 frame_present(u32 sequence, u32 column, u64 guest_readback_address);
IMPORT void frame_finish(u32 completed);

IMPORT u64 CreateFileW(const u16 *path, u32 access, u32 share, void *security,
                       u32 disposition, u32 flags, u64 template);
IMPORT int WriteFile(u64 file, const void *buffer, u32 size, u32 *written, void *overlapped);
IMPORT int ReadFile(u64 file, void *buffer, u32 size, u32 *read, void *overlapped);
IMPORT int SetFilePointerEx(u64 file, long long distance, u64 *position, u32 origin);
IMPORT int CloseHandle(u64 handle);

IMPORT int XenoraCreateDevice(u64 *device);
IMPORT int XenoraCreateCommandQueue(u64 device, u32 list_type, u64 *queue);
IMPORT int XenoraCreateCommandAllocator(u64 device, u32 list_type, u64 *allocator);
IMPORT int XenoraCreateCommandList(u64 device, u64 allocator, u32 list_type, u64 *list);
IMPORT int XenoraCreateBuffer(u64 device, u64 guest_addr, u64 size, u64 *buffer);
IMPORT int XenoraCreateRenderTarget(u64 device, u32 width, u32 height, u64 *target);
IMPORT int XenoraSetVertexBuffer(u64 list, u32 slot, u64 buffer, u64 offset, u64 stride);
IMPORT int XenoraSetRenderTarget(u64 list, u64 target, u32 clear);
IMPORT int XenoraSetViewport(u64 list, u32 width, u32 height);
IMPORT int XenoraDraw(u64 list, u32 vertices, u32 instances, u32 start);
IMPORT int XenoraCloseCommandList(u64 list);
IMPORT int XenoraExecuteCommandLists(u64 queue, u64 list);
IMPORT int XenoraReadbackResource(u64 resource, u64 guest_addr, u64 size);
IMPORT int XenoraDestroyResource(u64 resource);
IMPORT int XenoraDestroyCommandList(u64 list);
IMPORT int XenoraDestroyCommandAllocator(u64 allocator);
IMPORT int XenoraDestroyCommandQueue(u64 queue);
IMPORT int XenoraDestroyDevice(u64 device);
IMPORT __declspec(noreturn) void ExitProcess(u32 status);

// The triangle for column x starts at NDC x = -1 + x/2 and covers only
// that column's pixel centres in the three framebuffer rows.
static const float vertices[WIDTH][3][4] = {
    {{-1.0f, -10.0f, 0.5f, 1.0f}, {-0.3f, 0.0f, 0.5f, 1.0f}, {-1.0f, 10.0f, 0.5f, 1.0f}},
    {{-0.5f, -10.0f, 0.5f, 1.0f}, { 0.2f, 0.0f, 0.5f, 1.0f}, {-0.5f, 10.0f, 0.5f, 1.0f}},
    {{ 0.0f, -10.0f, 0.5f, 1.0f}, { 0.7f, 0.0f, 0.5f, 1.0f}, { 0.0f, 10.0f, 0.5f, 1.0f}},
    {{ 0.5f, -10.0f, 0.5f, 1.0f}, { 1.2f, 0.0f, 0.5f, 1.0f}, { 0.5f, 10.0f, 0.5f, 1.0f}},
};

static unsigned char frame[WIDTH * HEIGHT * 4];
static unsigned char records[MAX_FRAMES];
static unsigned char readback[256];
static const u16 journal_path[] = {
    'C', ':', '\\', 'x', 'e', 'n', 'o', 'r', 'a', '-', 'a', 'p', 'p', '-',
    'f', 'r', 'a', 'm', 'e', 's', '.', 'b', 'i', 'n', 0,
};

static void require(int condition, u32 failure) {
    if (!condition)
        ExitProcess(failure);
}

void entry(void) {
    u64 journal = CreateFileW(journal_path, GENERIC_READ_WRITE, 0, 0, CREATE_ALWAYS, 0, 0);
    require(journal != 0 && journal != INVALID_HANDLE, 7);

    u64 device, queue, allocator, buffer, target, lists[WIDTH];
    require(XenoraCreateDevice(&device) == S_OK, 1);
    require(XenoraCreateCommandQueue(device, 0, &queue) == S_OK, 1);
    require(XenoraCreateCommandAllocator(device, 0, &allocator) == S_OK, 1);
    require(XenoraCreateBuffer(device, (u64)vertices, sizeof(vertices), &buffer) == S_OK, 1);
    require(XenoraCreateRenderTarget(device, WIDTH, HEIGHT, &target) == S_OK, 1);

    for (u32 column = 0; column < WIDTH; column++) {
        u64 list;
        require(XenoraCreateCommandList(device, allocator, 0, &list) == S_OK, 2);
        require(XenoraSetVertexBuffer(list, 0, buffer, column * sizeof(vertices[0]),
                                      sizeof(vertices[0][0])) == S_OK, 2);
        require(XenoraSetRenderTarget(list, target, CLEAR) == S_OK, 2);
        require(XenoraSetViewport(list, WIDTH, HEIGHT) == S_OK, 2);
        require(XenoraDraw(list, 3, 1, 0) == S_OK, 2);
        require(XenoraCloseCommandList(list) == S_OK, 2);
        lists[column] = list;
    }

    u32 interactive_column = 0;
    u32 sequence = 0;
    for (;; sequence++) {
        u32 mode = frame_begin(sequence);
        if (mode == 0)
            break;
        require((mode == 1 || mode == 2) && sequence < MAX_FRAMES, 3);
        u32 column = sequence % WIDTH;
        u32 stamp = 0x10u;
        if (mode == 2) {
            u32 input = input_poll();
            require(input <= 2, 3);
            if (input == 1)
                interactive_column = (interactive_column + 1) % WIDTH;
            else if (input == 2)
                interactive_column = (interactive_column + WIDTH - 1) % WIDTH;
            column = interactive_column;
            stamp = 0x20u | (input << 2);
        }

        require(XenoraExecuteCommandLists(queue, lists[column]) == S_OK, 4);
        require(XenoraReadbackResource(target, (u64)frame, sizeof(frame)) == S_OK, 4);
        records[sequence] = (unsigned char)(stamp | column);
        u32 written;
        require(WriteFile(journal, &records[sequence], 1, &written, 0) && written == 1, 7);
        require(frame_present(sequence, column, (u64)frame) == 1, 5);
    }

    require(SetFilePointerEx(journal, 0, 0, FILE_BEGIN), 8);
    for (u32 offset = 0; offset < sequence;) {
        u32 size = sequence - offset;
        if (size > sizeof(readback))
            size = sizeof(readback);
        u32 received;
        require(ReadFile(journal, readback, size, &received, 0) && received == size, 8);
        for (u32 i = 0; i < received; i++)
            require(readback[i] == records[offset + i], 8);
        offset += received;
    }
    require(CloseHandle(journal), 8);

    for (u32 column = 0; column < WIDTH; column++)
        require(XenoraDestroyCommandList(lists[column]) == S_OK, 6);
    require(XenoraDestroyResource(target) == S_OK, 6);
    require(XenoraDestroyResource(buffer) == S_OK, 6);
    require(XenoraDestroyCommandAllocator(allocator) == S_OK, 6);
    require(XenoraDestroyCommandQueue(queue) == S_OK, 6);
    require(XenoraDestroyDevice(device) == S_OK, 6);
    frame_finish(sequence);
    ExitProcess(0);
}
