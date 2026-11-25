// SPDX-License-Identifier: GPL-3.0-or-later
#include <metal_stdlib>
using namespace metal;

// Each invocation owns one complete destination word. Input buffers are immutable;
// destination already contains the entire prior version, including padded tails.
kernel void patch_buffer_region(device const uint *source [[buffer(0)]],
                                device uint *destination [[buffer(1)]],
                                constant uint4 &parameters [[buffer(2)]],
                                uint index [[thread_position_in_grid]]) {
    if (index >= parameters.w) return;
    uint word = parameters.y / 4u + index;
    uint end = parameters.y + parameters.z;
    uint value = destination[word];
    for (uint byte = 0u; byte < 4u; ++byte) {
        uint position = word * 4u + byte;
        if (position >= parameters.y && position < end) {
            uint source_position = parameters.x + (position - parameters.y);
            uint source_byte = (source[source_position / 4u] >> ((source_position % 4u) * 8u)) & 0xffu;
            uint shift = byte * 8u;
            value = (value & ~(0xffu << shift)) | (source_byte << shift);
        }
    }
    destination[word] = value;
}
