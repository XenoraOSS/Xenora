// SPDX-License-Identifier: GPL-3.0-or-later
#include <metal_stdlib>
using namespace metal;
uint geometry_word(const device uchar* p, uint at) {
    return uint(p[at]) | (uint(p[at+1]) << 8) | (uint(p[at+2]) << 16) | (uint(p[at+3]) << 24);
}
bool geometry_attribute(const device uchar* p, uint len, uint off, uint stride, uint index, thread float4& out) {
    // Logical bounds, including every component, precede any byte access.
    if (stride < 16 || len < 16 || off > len-16 || index > (len-16-off)/stride) return false;
    uint at = off + index*stride;
    uint4 bits = uint4(geometry_word(p,at), geometry_word(p,at+4), geometry_word(p,at+8), geometry_word(p,at+12));
    if (any((bits & uint4(0x7f800000u)) == uint4(0x7f800000u))) return false;
    out = as_type<float4>(bits);
    return true;
}
kernel void gather_geometry(const device uchar* positions [[buffer(0)]],
    const device uchar* colors [[buffer(1)]], const device uchar* indices [[buffer(2)]],
    device float4* packed_positions [[buffer(3)]], device float4* packed_colors [[buffer(4)]],
    device atomic_uint* status [[buffer(5)]], constant uint* p [[buffer(6)]], uint lane [[thread_position_in_grid]]) {
    if (lane >= p[10]) return;
    float4 position = float4(0,0,0,1), color = float4(0);
    bool valid = p[9] <= 0xffffffffu-lane;
    uint index = valid ? p[9]+lane : 0;
    if (valid && p[13] != 0) {
        uint len=p[6], off=p[7], size=p[8];
        valid = (size == 2 || size == 4) && len >= size && off <= len-size;
        if (valid) valid = index <= (len-size-off)/size;
        if (valid) {
            uint at=off+index*size;
            index = uint(indices[at]) | (uint(indices[at+1]) << 8);
            if (size == 4) index |= (uint(indices[at+2]) << 16) | (uint(indices[at+3]) << 24);
            // Unsigned index plus signed base without signed overflow or narrowing.
            uint base=p[11];
            if ((base & 0x80000000u) != 0) {
                uint magnitude=0u-base;
                valid=index >= magnitude;
                if (valid) index-=magnitude;
            } else {
                valid=index <= 0xffffffffu-base;
                if (valid) index+=base;
            }
        }
    }
    if (valid) valid=geometry_attribute(positions,p[0],p[1],p[2],index,position);
    if (valid && p[12] != 0) valid=geometry_attribute(colors,p[3],p[4],p[5],index,color);
    if (!valid) {
        position=float4(0,0,0,1); color=float4(0);
        atomic_fetch_or_explicit(status,1u,memory_order_relaxed);
    }
    packed_positions[lane]=position;
    packed_colors[lane]=color;
}
