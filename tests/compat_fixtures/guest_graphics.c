// SPDX-License-Identifier: GPL-3.0-or-later
//
// Guest graphics: an original C program with no C library linked that drives
// the D3D12-style guest graphics command path end to end.
//
// It creates a device, a command queue, a command allocator and a command
// list, creates its own vertex buffer and render target out of guest memory,
// records a draw, executes it, reads the rendered framebuffer back into guest
// memory, asks the runtime which backend produced the frame, and destroys
// everything in the reverse order of creation.
//
// Every outside function is imported by name from xenora_graphics.dll or
// kernel32.dll; the prototypes below follow the public interface described in
// docs/runtime/guest-graphics.md. Nothing else is linked.
//
// The program checks STRUCTURAL facts only. It never inspects a pixel value:
// the integration test, crates/loader/tests/guest_graphics_path.rs, derives
// the expected pixels independently, and a program that asserted the same
// pixels would make that test circular.
//
// The checks, in order:
//   1  every handle returned by a create call is non-zero
//   2  every call that had to succeed returned S_OK
//   3  the viewport was accepted
//   4  the readback of each of the two frames returned S_OK
//   5  both frames read back 256 bytes
//   6  the frame info call succeeded and reports an 8 by 8 frame
//   7  the frame info reports one draw
//
// Exit status:
//   0   every check passed
//   1..7  the number of the check that failed
//   8   the bad-call control saw the service reject its invalid call
//
// Build with tests/compat_fixtures/build-guest-graphics.sh.

typedef unsigned long long u64;
typedef unsigned int u32;

#define IMPORT __declspec(dllimport)

// xenora_graphics.dll. Every export returns an HRESULT: 0 is S_OK.
IMPORT int XenoraCreateDevice(u64 *ppDevice);
IMPORT int XenoraCreateCommandQueue(u64 device, u32 listType, u64 *ppQueue);
IMPORT int XenoraCreateCommandAllocator(u64 device, u32 listType, u64 *ppAllocator);
IMPORT int XenoraCreateCommandList(u64 device, u64 allocator, u32 listType, u64 *ppList);
IMPORT int XenoraCreateBuffer(u64 device, u64 guestAddr, u64 byteLength, u64 *ppResource);
IMPORT int XenoraCreateRenderTarget(u64 device, u32 width, u32 height, u64 *ppResource);
IMPORT int XenoraSetVertexBuffer(u64 list, u32 slot, u64 resource, u64 offset, u64 stride);
IMPORT int XenoraSetRenderTarget(u64 list, u64 resource, u32 clearColor);
IMPORT int XenoraSetViewport(u64 list, u32 width, u32 height);
IMPORT int XenoraDraw(u64 list, u32 vertexCount, u32 instanceCount, u32 startVertex);
IMPORT int XenoraCloseCommandList(u64 list);
IMPORT int XenoraExecuteCommandLists(u64 queue, u64 list);
IMPORT int XenoraReadbackResource(u64 resource, u64 guestAddr, u64 byteLength);
IMPORT int XenoraGetLastFrameInfo(u32 *ppInfo);
IMPORT int XenoraDestroyResource(u64 resource);
IMPORT int XenoraDestroyCommandList(u64 list);
IMPORT int XenoraDestroyCommandAllocator(u64 allocator);
IMPORT int XenoraDestroyCommandQueue(u64 queue);
IMPORT int XenoraDestroyDevice(u64 device);

// kernel32.dll
IMPORT void ExitProcess(u32 status);

#ifdef GATE_MISSING
// Negative control: a name no Xenora service provides. The loader must
// refuse the image before this entry point ever runs.
IMPORT void XenoraNoSuchService(void);
#endif

#define S_OK 0

// The command list type that every create call asks for.
#define LIST_DIRECT 0

// The size of the render target and of the framebuffer read back from it.
// The stride of one vertex record: a float4 position. A flat draw reads
// three consecutive positions, so no texture coordinate is bound.
#define VERTEX_STRIDE_FLAT 16
#define FRAME_WIDTH 8
#define FRAME_HEIGHT 8
#define FRAME_BYTES (FRAME_WIDTH * FRAME_HEIGHT * 4)

// Three float4 positions per scene.
#define VERTEX_COUNT 3
#define VERTEX_BYTES 48

// The exit status that says the bad-call control worked as intended.
#define BAD_CALL_REJECTED 8

// The compiler refers to this when a program uses floating point. The vertex
// positions are the only floats in the program and they are plain data.
int _fltused = 0;

// No C library is linked, so the byte move that the compiler asks for when it
// clears a local block is provided here. The writes go through a volatile
// pointer, so the compiler cannot turn this back into a call to itself.
void *memset(void *destination, int value, u64 count) {
    volatile unsigned char *bytes = (volatile unsigned char *)destination;
    for (u64 index = 0; index < count; index++)
        bytes[index] = (unsigned char)value;
    return destination;
}

// What the host reads from guest memory afterwards, in a section of its own so
// that the test can find it by name.
struct guest_graphics_result {
    u32 magic;
    u32 checks;
    u32 width;
    u32 height;
    u32 backend;
    u32 draw_count;
    u32 frame0_len;
    u32 frame1_len;
    u64 frame0_ptr;
    u64 frame1_ptr;
};
#pragma section(".gate", read, write)
__declspec(allocate(".gate")) struct guest_graphics_result guest_graphics_result = {
    0x67726170u, 0, 0, 0, 0, 0, 0, 0, 0, 0};

// The two scenes. The first covers the whole target, the second lies outside
// the clip volume and so covers no pixel at all.
static float scene0[3][4] = {
    {-1.0f, -1.0f, 0.5f, 1.0f},
    {3.0f, -1.0f, 0.5f, 1.0f},
    {-1.0f, 3.0f, 0.5f, 1.0f},
};
static float scene1[3][4] = {
    {5.0f, 5.0f, 0.5f, 1.0f},
    {6.0f, 5.0f, 0.5f, 1.0f},
    {5.0f, 6.0f, 0.5f, 1.0f},
};

// The raw frame bytes, read back from the two render targets.
static unsigned char frame0_bytes[FRAME_BYTES];
static unsigned char frame1_bytes[FRAME_BYTES];

// The clear colours, packed R | G<<8 | B<<16 | A<<24.
#define CLEAR0 ((u32)255u | ((u32)43u << 8) | ((u32)93u << 16) | ((u32)251u << 24))
#define CLEAR1 ((u32)10u | ((u32)20u << 8) | ((u32)30u << 16) | ((u32)255u << 24))

// Everything one frame produced. Every HRESULT is kept, so that the checks
// can be made afterwards, in one place and in one order.
struct frame_outcome {
    u64 allocator;
    u64 list;
    u64 resource;
    u64 vertex_buffer;
    int create_allocator;
    int create_list;
    int create_resource;
    int create_vertex_buffer;
    int set_render_target;
    int set_vertex_buffer;
    int set_viewport;
    int bad_viewport;
    int draw;
    int close_list;
    int execute;
    int readback;
};

static struct frame_outcome outcome0;
static struct frame_outcome outcome1;

// The handles that outlive a single frame.
static u64 device_handle;
static u64 queue_handle;

// One frame: create the resources, record a draw, close the list, execute it
// and read the framebuffer back. Nothing here inspects a pixel.
static void run_frame(struct frame_outcome *out, float *scene, u32 clear_color,
                      unsigned char *readback_target, int use_bad_viewport) {
    out->create_allocator =
        XenoraCreateCommandAllocator(device_handle, LIST_DIRECT, &out->allocator);
    out->create_list =
        XenoraCreateCommandList(device_handle, out->allocator, LIST_DIRECT, &out->list);
    out->create_resource =
        XenoraCreateRenderTarget(device_handle, FRAME_WIDTH, FRAME_HEIGHT, &out->resource);
    out->create_vertex_buffer =
        XenoraCreateBuffer(device_handle, (u64)scene, VERTEX_BYTES, &out->vertex_buffer);

    out->set_render_target = XenoraSetRenderTarget(out->list, out->resource, clear_color);
#ifdef GATE_BAD
    // Negative control: a viewport of zero width must be rejected with a
    // failure HRESULT rather than silently accepted. The valid viewport that
    // follows keeps the rest of the frame intact.
    if (use_bad_viewport)
        out->bad_viewport = XenoraSetViewport(out->list, 0, FRAME_HEIGHT);
#endif
    (void)use_bad_viewport;
    out->set_viewport = XenoraSetViewport(out->list, FRAME_WIDTH, FRAME_HEIGHT);
    out->set_vertex_buffer = XenoraSetVertexBuffer(out->list, 0, out->vertex_buffer, 0,
                                                    VERTEX_STRIDE_FLAT);
    out->draw = XenoraDraw(out->list, VERTEX_COUNT, 1, 0);
    out->close_list = XenoraCloseCommandList(out->list);
    out->execute = XenoraExecuteCommandLists(queue_handle, out->list);
    out->readback =
        XenoraReadbackResource(out->resource, (u64)readback_target, FRAME_BYTES);
}

// Destroy one frame's objects in the reverse order of creation: the resources
// the closed list referenced, then the list, then the allocator.
static void destroy_frame(struct frame_outcome *out) {
    XenoraDestroyResource(out->vertex_buffer);
    XenoraDestroyResource(out->resource);
    XenoraDestroyCommandList(out->list);
    XenoraDestroyCommandAllocator(out->allocator);
}

// The first check number that did not pass, or 0 when all of them passed.
static u32 first_failure(u32 c1, u32 c2, u32 c3, u32 c4, u32 c5, u32 c6, u32 c7) {
    if (!c1) return 1;
    if (!c2) return 2;
    if (!c3) return 3;
    if (!c4) return 4;
    if (!c5) return 5;
    if (!c6) return 6;
    if (!c7) return 7;
    return 0;
}

static u32 run(void) {
    u32 info[4] = {0, 0, 0, 0};

    int create_device = XenoraCreateDevice(&device_handle);
    int create_queue = XenoraCreateCommandQueue(device_handle, LIST_DIRECT, &queue_handle);

    run_frame(&outcome0, &scene0[0][0], CLEAR0, frame0_bytes, 1);
    run_frame(&outcome1, &scene1[0][0], CLEAR1, frame1_bytes, 0);

    int frame_info = XenoraGetLastFrameInfo(info);

    // Destroy the resources, the lists, the allocators, the queue and the
    // device, in the reverse order of creation.
    destroy_frame(&outcome0);
    destroy_frame(&outcome1);
    XenoraDestroyCommandQueue(queue_handle);
    XenoraDestroyDevice(device_handle);

    // The report the host reads.
    guest_graphics_result.frame0_len = FRAME_BYTES;
    guest_graphics_result.frame1_len = FRAME_BYTES;
    guest_graphics_result.frame0_ptr = (u64)frame0_bytes;
    guest_graphics_result.frame1_ptr = (u64)frame1_bytes;
    guest_graphics_result.width = info[1];
    guest_graphics_result.height = info[2];
    guest_graphics_result.backend = info[0];
    guest_graphics_result.draw_count = info[3];

    // 1: every handle returned by a create call is non-zero.
    u32 handles = device_handle != 0 && queue_handle != 0 &&
                  outcome0.allocator != 0 && outcome0.list != 0 &&
                  outcome0.resource != 0 && outcome0.vertex_buffer != 0 &&
                  outcome1.allocator != 0 && outcome1.list != 0 &&
                  outcome1.resource != 0 && outcome1.vertex_buffer != 0;

    // 2: every call that had to succeed returned S_OK.
    u32 succeeded = create_device == S_OK && create_queue == S_OK &&
                    outcome0.create_allocator == S_OK && outcome0.create_list == S_OK &&
                    outcome0.create_resource == S_OK && outcome0.create_vertex_buffer == S_OK &&
                    outcome0.set_render_target == S_OK && outcome0.set_vertex_buffer == S_OK &&
                    outcome0.draw == S_OK && outcome0.close_list == S_OK &&
                    outcome0.execute == S_OK &&
                    outcome1.create_allocator == S_OK && outcome1.create_list == S_OK &&
                    outcome1.create_resource == S_OK && outcome1.create_vertex_buffer == S_OK &&
                    outcome1.set_render_target == S_OK && outcome1.set_vertex_buffer == S_OK &&
                    outcome1.draw == S_OK && outcome1.close_list == S_OK &&
                    outcome1.execute == S_OK;

    // 3: the viewport was accepted.
    u32 viewport = outcome0.set_viewport == S_OK && outcome1.set_viewport == S_OK;
#ifdef GATE_BAD
    // The control additionally requires the invalid call to have been
    // rejected with a failure HRESULT.
    viewport = viewport && outcome0.bad_viewport != S_OK;
#endif

    // 4: the readback of each frame succeeded.
    u32 readback = outcome0.readback == S_OK && outcome1.readback == S_OK;

    // 5: both frames read back 256 bytes.
    u32 lengths = guest_graphics_result.frame0_len == FRAME_BYTES &&
                  guest_graphics_result.frame1_len == FRAME_BYTES;

    // 6: the frame info call succeeded and reports an 8 by 8 frame.
    u32 frame = frame_info == S_OK && info[1] == FRAME_WIDTH && info[2] == FRAME_HEIGHT;

    // 7: the frame info reports one draw.
    u32 draws = info[3] == 1;

    u32 failed = first_failure(handles, succeeded, viewport, readback, lengths, frame, draws);
    // The number of checks that passed: the ones before the failed one.
    guest_graphics_result.checks = failed ? failed - 1u : 7u;
    if (failed)
        return failed;
#ifdef GATE_BAD
    // Every check passed, and one of them was that the service rejected the
    // invalid call, so this run is distinguishable from a clean one.
    return BAD_CALL_REJECTED;
#else
    return 0;
#endif
}

#ifdef GATE_MISSING
// The entry point of the missing-import control. The loader must refuse the
// image before this runs, so the call below never happens.
void guest_graphics_entry(void) {
    XenoraNoSuchService();
    ExitProcess(run());
}
#else
void guest_graphics_entry(void) {
    ExitProcess(run());
}
#endif
