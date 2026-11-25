// SPDX-License-Identifier: GPL-3.0-or-later
//
// Guest textured scene: an original C program with no C library linked that
// drives the D3D12-style guest graphics command path with a texture bound.
//
// It creates a device, a command queue, a command allocator and a command
// list, uploads a 2 by 2 R8G8B8A8 texture and a vertex buffer out of guest
// memory, creates a shader-resource-view heap, a sampler heap, a shader
// resource view, a sampler and a root signature, records a textured draw,
// executes it, reads the rendered framebuffer back into guest memory, asks
// the runtime which backend produced the frame, and destroys everything in
// the reverse order of creation.
//
// Every outside function is imported by name from xenora_graphics.dll or
// kernel32.dll; the prototypes below follow the public interface described in
// docs/runtime/guest-graphics.md. Nothing else is linked.
//
// The program checks STRUCTURAL facts only. It never inspects a pixel value:
// the integration test, crates/loader/tests/guest_textured_path.rs, derives
// the expected pixels independently, and a program that asserted the same
// pixels would make that test circular.
//
// The checks, in order:
//   1  every handle returned by a create call is non-zero
//   2  every call that had to succeed returned S_OK
//   3  the readback returned S_OK
//   4  the frame read back 256 bytes
//   5  the frame info call succeeded and reports an 8 by 8 frame
//   6  the frame info reports one draw
//
// Exit status:
//   0   every check passed
//   1..6  the number of the check that failed
//
// Build with tests/compat_fixtures/build-guest-textured.sh.

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
IMPORT int XenoraCreateTexture2D(u64 device, u32 width, u32 height, u32 format, u64 guestAddr,
                                u64 *ppResource);
IMPORT int XenoraCreateDescriptorHeap(u64 device, u32 heapType, u32 numDescriptors, u64 *ppHeap);
IMPORT int XenoraCreateShaderResourceView(u64 device, u64 heap, u32 slot, u64 resource, u32 format);
IMPORT int XenoraCreateSampler(u64 device, u64 heap, u32 slot, u32 filter, u32 addressU,
                               u32 addressV, u32 addressW);
IMPORT int XenoraCreateRootSignature(u64 device, u32 srvParameter, u32 samplerParameter,
                                     u64 *ppRootSignature);
IMPORT int XenoraSetDescriptorHeaps(u64 list, u64 srvHeap, u64 samplerHeap);
IMPORT int XenoraSetGraphicsRootSignature(u64 list, u64 rootSignature);
IMPORT int XenoraSetGraphicsRootDescriptorTable(u64 list, u32 rootParameterIndex, u64 heap,
                                                u32 slot);
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
#define FRAME_WIDTH 8
#define FRAME_HEIGHT 8
#define FRAME_BYTES (FRAME_WIDTH * FRAME_HEIGHT * 4)

// The texture: 2 by 2 R8G8B8A8, row major.
// The stride of one vertex record: a float4 position followed by a float4
// texture coordinate.
#define VERTEX_STRIDE_TEXTURED 32
#define TEXTURE_WIDTH 2
#define TEXTURE_HEIGHT 2
#define TEXTURE_BYTES (TEXTURE_WIDTH * TEXTURE_HEIGHT * 4)

// The texture format the service calls R8G8B8A8_UNORM.
#define TEXTURE_FORMAT 28

// The descriptor heap types: one heap for shader resource views, one for
// samplers. Each holds a single descriptor.
#define HEAP_CBV_SRV_UAV 0
#define HEAP_SAMPLER 1

// The sampler filter and address modes. Point sampling keeps the sampled
// colour exactly one of the two texture columns. The filter is
// D3D12_FILTER_MIN_MAG_MIP_POINT and the address mode is
// D3D12_TEXTURE_ADDRESS_MODE_CLAMP, the only pair the runtime accepts.
#define SAMPLER_FILTER_POINT 0
#define SAMPLER_ADDRESS_CLAMP 3

// The root parameter indices of the two descriptor tables.
#define ROOT_SRV 0
#define ROOT_SAMPLER 1

// Three vertices of a float4 position and a float4 texture coordinate, so
// the vertex stride is 32 bytes.
#define VERTEX_COUNT 3
#define VERTEX_FLOATS 8
#define VERTEX_BYTES (VERTEX_COUNT * VERTEX_FLOATS * 4)

// The compiler refers to this when a program uses floating point. The vertex
// and texture data are the only floats in the program and they are plain data.
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
struct guest_textured_result {
    u32 magic;
    u32 checks;
    u32 width;
    u32 height;
    u32 backend;
    u32 draw_count;
    u32 frame_len;
    u64 frame_ptr;
};
#pragma section(".gate", read, write)
__declspec(allocate(".gate")) struct guest_textured_result guest_textured_result = {
    0x67727478u, 0, 0, 0, 0, 0, 0, 0};

// The texture pixels: column 0 is red, column 1 is blue, and both rows of a
// column hold the same colour so that the sampled result does not depend on
// which row the sampler picks.
static unsigned char texture_pixels[TEXTURE_BYTES] = {
    255, 0, 0, 255,   /* row 0, column 0: red */
    0, 0, 255, 255,   /* row 0, column 1: blue */
    255, 0, 0, 255,   /* row 1, column 0: red */
    0, 0, 255, 255,   /* row 1, column 1: blue */
};

// The full-screen triangle. Every vertex carries the same texture coordinate,
// so every pixel samples column 0 of the texture, which is red.
static float scene[3][VERTEX_FLOATS] = {
    {-1.0f, -1.0f, 0.5f, 1.0f, 0.25f, 0.5f, 0.0f, 0.0f},
    {3.0f, -1.0f, 0.5f, 1.0f, 0.25f, 0.5f, 0.0f, 0.0f},
    {-1.0f, 3.0f, 0.5f, 1.0f, 0.25f, 0.5f, 0.0f, 0.0f},
};

// The raw frame bytes, read back from the render target.
static unsigned char frame_bytes[FRAME_BYTES];

// The clear colour, packed R | G<<8 | B<<16 | A<<24.
#define CLEAR ((u32)0u | ((u32)0u << 8) | ((u32)0u << 16) | ((u32)255u << 24))

// Every HRESULT is kept, so that the checks can be made afterwards, in one
// place and in one order.
struct textured_outcome {
    u64 allocator;
    u64 list;
    u64 queue;
    u64 render_target;
    u64 texture;
    u64 vertex_buffer;
    u64 srv_heap;
    u64 sampler_heap;
    u64 root_signature;
    int create_queue;
    int create_allocator;
    int create_list;
    int create_render_target;
    int create_texture;
    int create_buffer;
    int create_srv_heap;
    int create_sampler_heap;
    int create_srv;
    int create_sampler;
    int create_root_signature;
    int set_descriptor_heaps;
    int set_root_signature;
    int set_srv_table;
    int set_sampler_table;
    int set_render_target;
    int set_viewport;
    int set_vertex_buffer;
    int draw;
    int close_list;
    int execute;
    int readback;
};

static struct textured_outcome outcome;

// The device handle, which outlives every other handle.
static u64 device_handle;

// The first check number that did not pass, or 0 when all of them passed.
static u32 first_failure(u32 c1, u32 c2, u32 c3, u32 c4, u32 c5, u32 c6) {
    if (!c1) return 1;
    if (!c2) return 2;
    if (!c3) return 3;
    if (!c4) return 4;
    if (!c5) return 5;
    if (!c6) return 6;
    return 0;
}

static u32 run(void) {
    u32 info[4] = {0, 0, 0, 0};

    int create_device = XenoraCreateDevice(&device_handle);
    outcome.create_queue =
        XenoraCreateCommandQueue(device_handle, LIST_DIRECT, &outcome.queue);
    outcome.create_allocator =
        XenoraCreateCommandAllocator(device_handle, LIST_DIRECT, &outcome.allocator);
    outcome.create_list =
        XenoraCreateCommandList(device_handle, outcome.allocator, LIST_DIRECT, &outcome.list);

    // The texture and the vertex buffer both live in guest memory.
    outcome.create_texture =
        XenoraCreateTexture2D(device_handle, TEXTURE_WIDTH, TEXTURE_HEIGHT, TEXTURE_FORMAT,
                              (u64)texture_pixels, &outcome.texture);
    outcome.create_buffer =
        XenoraCreateBuffer(device_handle, (u64)&scene[0][0], VERTEX_BYTES, &outcome.vertex_buffer);
    outcome.create_render_target =
        XenoraCreateRenderTarget(device_handle, FRAME_WIDTH, FRAME_HEIGHT, &outcome.render_target);

    // One descriptor in each heap: the shader resource view in slot 0 of the
    // shader-resource-view heap, the sampler in slot 0 of the sampler heap.
    outcome.create_srv_heap =
        XenoraCreateDescriptorHeap(device_handle, HEAP_CBV_SRV_UAV, 1, &outcome.srv_heap);
    outcome.create_sampler_heap =
        XenoraCreateDescriptorHeap(device_handle, HEAP_SAMPLER, 1, &outcome.sampler_heap);
    outcome.create_srv =
        XenoraCreateShaderResourceView(device_handle, outcome.srv_heap, 0, outcome.texture,
                                       TEXTURE_FORMAT);
    outcome.create_sampler =
        XenoraCreateSampler(device_handle, outcome.sampler_heap, 0, SAMPLER_FILTER_POINT,
                            SAMPLER_ADDRESS_CLAMP, SAMPLER_ADDRESS_CLAMP, SAMPLER_ADDRESS_CLAMP);
    outcome.create_root_signature =
        XenoraCreateRootSignature(device_handle, ROOT_SRV, ROOT_SAMPLER, &outcome.root_signature);

    // The recorded frame.
    outcome.set_descriptor_heaps =
        XenoraSetDescriptorHeaps(outcome.list, outcome.srv_heap, outcome.sampler_heap);
    outcome.set_root_signature =
        XenoraSetGraphicsRootSignature(outcome.list, outcome.root_signature);
    outcome.set_srv_table =
        XenoraSetGraphicsRootDescriptorTable(outcome.list, ROOT_SRV, outcome.srv_heap, 0);
    outcome.set_sampler_table =
        XenoraSetGraphicsRootDescriptorTable(outcome.list, ROOT_SAMPLER, outcome.sampler_heap, 0);
    outcome.set_render_target =
        XenoraSetRenderTarget(outcome.list, outcome.render_target, CLEAR);
    outcome.set_viewport = XenoraSetViewport(outcome.list, FRAME_WIDTH, FRAME_HEIGHT);
    outcome.set_vertex_buffer = XenoraSetVertexBuffer(outcome.list, 0, outcome.vertex_buffer, 0,
                                                      VERTEX_STRIDE_TEXTURED);
    outcome.draw = XenoraDraw(outcome.list, VERTEX_COUNT, 1, 0);
    outcome.close_list = XenoraCloseCommandList(outcome.list);
    outcome.execute = XenoraExecuteCommandLists(outcome.queue, outcome.list);
    outcome.readback =
        XenoraReadbackResource(outcome.render_target, (u64)frame_bytes, FRAME_BYTES);

    int frame_info = XenoraGetLastFrameInfo(info);

    // Destroy everything in the reverse order of creation: the resources the
    // closed list referenced, then the list, the allocator, the queue and the
    // device.
    XenoraDestroyResource(outcome.render_target);
    XenoraDestroyResource(outcome.texture);
    XenoraDestroyResource(outcome.vertex_buffer);
    XenoraDestroyCommandList(outcome.list);
    XenoraDestroyCommandAllocator(outcome.allocator);
    XenoraDestroyCommandQueue(outcome.queue);
    XenoraDestroyDevice(device_handle);

    // The report the host reads.
    guest_textured_result.width = info[1];
    guest_textured_result.height = info[2];
    guest_textured_result.backend = info[0];
    guest_textured_result.draw_count = info[3];
    guest_textured_result.frame_len = FRAME_BYTES;
    guest_textured_result.frame_ptr = (u64)frame_bytes;

    // 1: every handle returned by a create call is non-zero.
    u32 handles = device_handle != 0 && outcome.queue != 0 && outcome.allocator != 0 &&
                  outcome.list != 0 && outcome.render_target != 0 && outcome.texture != 0 &&
                  outcome.vertex_buffer != 0 && outcome.srv_heap != 0 &&
                  outcome.sampler_heap != 0 && outcome.root_signature != 0;

    // 2: every call that had to succeed returned S_OK.
    u32 succeeded = create_device == S_OK && outcome.create_queue == S_OK &&
                    outcome.create_allocator == S_OK && outcome.create_list == S_OK &&
                    outcome.create_texture == S_OK && outcome.create_buffer == S_OK &&
                    outcome.create_render_target == S_OK && outcome.create_srv_heap == S_OK &&
                    outcome.create_sampler_heap == S_OK && outcome.create_srv == S_OK &&
                    outcome.create_sampler == S_OK && outcome.create_root_signature == S_OK &&
                    outcome.set_descriptor_heaps == S_OK && outcome.set_root_signature == S_OK &&
                    outcome.set_srv_table == S_OK && outcome.set_sampler_table == S_OK &&
                    outcome.set_render_target == S_OK && outcome.set_viewport == S_OK &&
                    outcome.set_vertex_buffer == S_OK && outcome.draw == S_OK &&
                    outcome.close_list == S_OK && outcome.execute == S_OK;

    // 3: the readback succeeded.
    u32 readback = outcome.readback == S_OK;

    // 4: the frame read back 256 bytes.
    u32 lengths = guest_textured_result.frame_len == FRAME_BYTES;

    // 5: the frame info call succeeded and reports an 8 by 8 frame.
    u32 frame = frame_info == S_OK && info[1] == FRAME_WIDTH && info[2] == FRAME_HEIGHT;

    // 6: the frame info reports one draw.
    u32 draws = info[3] == 1;

    u32 failed = first_failure(handles, succeeded, readback, lengths, frame, draws);
    // The number of checks that passed: the ones before the failed one.
    guest_textured_result.checks = failed ? failed - 1u : 6u;
    if (failed)
        return failed;
    return 0;
}

#ifdef GATE_MISSING
// The entry point of the missing-import control. The loader must refuse the
// image before this runs, so the call below never happens.
void guest_textured_entry(void) {
    XenoraNoSuchService();
    ExitProcess(run());
}
#else
void guest_textured_entry(void) {
    ExitProcess(run());
}
#endif
