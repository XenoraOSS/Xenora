// SPDX-License-Identifier: GPL-3.0-or-later

#include "xenora-session-v1.h"

typedef unsigned int u32;
typedef unsigned short u16;
typedef unsigned long long u64;

#define IMPORT __declspec(dllimport)
#define CHECK(condition, code) do { if (!(condition)) ExitProcess(code); } while (0)

IMPORT u64 CreateFileW(const u16 *path, u32 access, u32 share, void *security,
                       u32 disposition, u32 flags, u64 template);
IMPORT int WriteFile(u64 file, const void *bytes, u32 size, u32 *written, void *overlapped);
IMPORT int ReadFile(u64 file, void *bytes, u32 size, u32 *read, void *overlapped);
IMPORT int SetFilePointerEx(u64 file, long long distance, u64 *position, u32 origin);
IMPORT int CloseHandle(u64 handle);
IMPORT void Sleep(u32 milliseconds);
IMPORT u64 GetTickCount64(void);
IMPORT void GetSystemTimeAsFileTime(u64 *filetime);
IMPORT __declspec(noreturn) void ExitProcess(u32 status);
#ifdef SESSION_COMPANION
IMPORT u32 CompanionColor(u32 input);
#endif

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
IMPORT int XenoraDestroyResource(u64 resource);
IMPORT int XenoraDestroyCommandList(u64 list);
IMPORT int XenoraDestroyCommandAllocator(u64 allocator);
IMPORT int XenoraDestroyCommandQueue(u64 queue);
IMPORT int XenoraDestroyDevice(u64 device);

enum { WIDTH = 4, HEIGHT = 3, MAX_FRAMES = 64, LARGE_SIZE = 64 };
enum session_mode { NORMAL, MISSING_PRESENT, EXTRA_PRESENT, LEAK_FILE, STALL_FRAME, DETERMINISTIC_CLOCK, LARGE_FRAME, READONLY_NEXT_INPUT };

struct journal_entry {
    u32 sequence;
    u32 input;
};

static const u16 journal_path[] = {
    'C', ':', '\\', 'a', 'u', 't', 'h', 'o', 'r', 'e', 'd', '-',
    's', 'e', 's', 's', 'i', 'o', 'n', '.', 'b', 'i', 'n', 0,
};
// Each triangle covers every row of one outer column of the 4-by-3 target.
static const float vertices[2][3][4] = {
    {{-1.0f, -8.0f, 0.5f, 1.0f}, {-0.375f, 0.0f, 0.5f, 1.0f}, {-1.0f, 8.0f, 0.5f, 1.0f}},
    {{0.5f, -8.0f, 0.5f, 1.0f}, {1.125f, 0.0f, 0.5f, 1.0f}, {0.5f, 8.0f, 0.5f, 1.0f}},
};
int _fltused;

static struct journal_entry expected[MAX_FRAMES];
static struct journal_entry journal[MAX_FRAMES + 1];
static u32 pixels[WIDTH * HEIGHT];
static const u32 readonly_next_input = 0xa5a5a5a5u;

struct graphics {
    u64 device;
    u64 queue;
    u64 allocator;
    u64 buffer;
    u64 target;
    u32 width;
    u32 height;
};

static void set_up(struct graphics *graphics) {
    CHECK(XenoraCreateDevice(&graphics->device) == 0, 10);
    CHECK(XenoraCreateCommandQueue(graphics->device, 0, &graphics->queue) == 0, 11);
    CHECK(XenoraCreateCommandAllocator(graphics->device, 0, &graphics->allocator) == 0, 12);
    CHECK(XenoraCreateBuffer(graphics->device, (u64)vertices, sizeof(vertices), &graphics->buffer) == 0, 13);
    CHECK(XenoraCreateRenderTarget(graphics->device, graphics->width, graphics->height, &graphics->target) == 0, 14);
}

static void draw_frame(const struct graphics *graphics, u32 input, u32 sequence, u32 blue) {
    const u32 side = input & 1;
    const u32 clear = 0xff000000u | ((blue & 255) << 16) | (sequence << 8) | (input & 255);
    u64 list;
    CHECK(XenoraCreateCommandList(graphics->device, graphics->allocator, 0, &list) == 0, 20);
    CHECK(XenoraSetVertexBuffer(list, 0, graphics->buffer, side * sizeof(vertices[0]), sizeof(vertices[0][0])) == 0, 21);
    CHECK(XenoraSetRenderTarget(list, graphics->target, clear) == 0, 22);
    CHECK(XenoraSetViewport(list, graphics->width, graphics->height) == 0, 23);
    CHECK(XenoraDraw(list, 3, 1, 0) == 0, 24);
    CHECK(XenoraCloseCommandList(list) == 0 && XenoraExecuteCommandLists(graphics->queue, list) == 0, 25);
    if (graphics->width == WIDTH && graphics->height == HEIGHT) {
        CHECK(XenoraReadbackResource(graphics->target, (u64)pixels, sizeof(pixels)) == 0, 26);
        const u32 red_column = side ? WIDTH - 1 : 0;
        for (u32 pixel = 0; pixel < WIDTH * HEIGHT; ++pixel)
            CHECK(pixels[pixel] == (pixel % WIDTH == red_column ? 0xff0000ffu : clear), 27);
    }
    CHECK(XenoraDestroyCommandList(list) == 0, 28);
}

static void tear_down(const struct graphics *graphics) {
    CHECK(XenoraDestroyResource(graphics->target) == 0, 40);
    CHECK(XenoraDestroyResource(graphics->buffer) == 0, 41);
    CHECK(XenoraDestroyCommandAllocator(graphics->allocator) == 0, 42);
    CHECK(XenoraDestroyCommandQueue(graphics->queue) == 0, 43);
    CHECK(XenoraDestroyDevice(graphics->device) == 0, 44);
}

static u32 clock_milliseconds(void) {
    u64 filetime;
    GetSystemTimeAsFileTime(&filetime);
    const u64 milliseconds = GetTickCount64();
    CHECK(filetime == 116444736000000000ull + milliseconds * 10000, 47);
    return (u32)milliseconds;
}

static void run_session(enum session_mode mode) {
    const u64 file = CreateFileW(journal_path, 0xc0000000u, 0, 0, 2, 0, 0);
    CHECK(file != 0 && file != ~0ull, 1);
    struct graphics graphics;
    graphics.width = mode == LARGE_FRAME ? LARGE_SIZE : WIDTH;
    graphics.height = mode == LARGE_FRAME ? LARGE_SIZE : HEIGHT;
    set_up(&graphics);
    if (mode == DETERMINISTIC_CLOCK)
        CHECK(clock_milliseconds() == 0, 48);

    u32 frames = 0;
    for (;;) {
        u32 input;
        const int available = XenoraSessionNextInput(&input);
        if (available == 0)
            break;
        if (mode == STALL_FRAME)
            Sleep(5000);
        CHECK(available == 1 && frames < MAX_FRAMES, 2);
        struct journal_entry *record = &expected[frames];
        record->sequence = ++frames;
        record->input = input;
        u32 count;
        CHECK(WriteFile(file, record, sizeof(*record), &count, 0) && count == sizeof(*record), 3);
#ifdef SESSION_COMPANION
        const u32 blue = CompanionColor(input);
#else
        const u32 blue = mode == DETERMINISTIC_CLOCK ? clock_milliseconds() : 0x3c;
#endif
        draw_frame(&graphics, input, frames, blue);

        if (mode == MISSING_PRESENT) {
            (void)XenoraSessionNextInput(&input);
            ExitProcess(71);
        }
        CHECK(XenoraSessionPresent() == 1, 4);
        if (mode == READONLY_NEXT_INPUT) {
            (void)XenoraSessionNextInput((u32 *)&readonly_next_input);
            ExitProcess(74);
        }
        if (mode == EXTRA_PRESENT) {
            (void)XenoraSessionPresent();
            ExitProcess(72);
        }
        if (mode == DETERMINISTIC_CLOCK)
            Sleep(5);
    }
    if (mode == DETERMINISTIC_CLOCK)
        CHECK(clock_milliseconds() == frames * 21, 49);

    CHECK(SetFilePointerEx(file, 0, 0, 0), 30);
    u32 count;
    CHECK(ReadFile(file, journal, sizeof(journal), &count, 0) && count == frames * sizeof(journal[0]), 31);
    for (u32 index = 0; index < frames; ++index)
        CHECK(journal[index].sequence == index + 1 && journal[index].input == expected[index].input, 32);

    tear_down(&graphics);
    if (mode == LEAK_FILE) {
        (void)XenoraSessionFinish();
        ExitProcess(73);
    }
    CHECK(CloseHandle(file), 45);
    CHECK(XenoraSessionFinish() == 1, 46);
    ExitProcess(0);
}

void entry(void) { run_session(NORMAL); }
void missing_present_entry(void) { run_session(MISSING_PRESENT); }
void extra_present_entry(void) { run_session(EXTRA_PRESENT); }
void leak_entry(void) { run_session(LEAK_FILE); }
void stall_frame_entry(void) { run_session(STALL_FRAME); }
void deterministic_clock_entry(void) { run_session(DETERMINISTIC_CLOCK); }
void large_frame_entry(void) { run_session(LARGE_FRAME); }

void null_next_input_entry(void) {
    (void)XenoraSessionNextInput(0);
    ExitProcess(75);
}
void readonly_next_input_entry(void) { run_session(READONLY_NEXT_INPUT); }

#ifdef SESSION_CROSS_PAGE_NEXT_INPUT
IMPORT void *VirtualAlloc(void *address, u64 size, u32 allocation, u32 protection);
IMPORT int VirtualProtect(void *address, u64 size, u32 protection, u32 *old_protection);

void cross_page_next_input_entry(void) {
    enum { PAGE_SIZE = 4096, MEM_RESERVE_COMMIT = 0x3000, PAGE_READWRITE = 4, PAGE_READONLY = 2 };
    unsigned char *pages = VirtualAlloc(0, 2 * PAGE_SIZE, MEM_RESERVE_COMMIT, PAGE_READWRITE);
    CHECK(pages != 0, 76);
    u32 old_protection = 0;
    CHECK(VirtualProtect(pages + PAGE_SIZE, PAGE_SIZE, PAGE_READONLY, &old_protection), 77);
    CHECK(old_protection == PAGE_READWRITE, 78);
    (void)XenoraSessionNextInput((u32 *)(pages + PAGE_SIZE - 2));
    ExitProcess(79);
}
#endif
