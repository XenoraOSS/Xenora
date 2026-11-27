#include <metal_stdlib>
using namespace metal;
kernel void xenora_compute(
    device const uint* input [[buffer(0)]],
    device uint* output [[buffer(1)]],
    uint index [[thread_position_in_grid]],
    uint grid [[threads_per_grid]]) {
    uint t0 = input[index];
    uint t1 = 0x7fc00000u;
    uint t2 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint pass = (false) ? 1u : 0u;
        t2 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t3 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = ((!(((l0 & 0x7fffffffu) > 0x7f800000u) || ((t1 & 0x7fffffffu) > 0x7f800000u)) && (((l0 << 1u) == 0u) ? 0x80000000u : (((l0 & 0x80000000u) != 0u) ? ~l0 : (l0 | 0x80000000u))) < (((t1 << 1u) == 0u) ? 0x80000000u : (((t1 & 0x80000000u) != 0u) ? ~t1 : (t1 | 0x80000000u))))) ? 1u : 0u;
        t3 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t4 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = ((!(((l0 & 0x7fffffffu) > 0x7f800000u) || ((t1 & 0x7fffffffu) > 0x7f800000u)) && (((l0 << 1u) == 0u) ? 0x80000000u : (((l0 & 0x80000000u) != 0u) ? ~l0 : (l0 | 0x80000000u))) == (((t1 << 1u) == 0u) ? 0x80000000u : (((t1 & 0x80000000u) != 0u) ? ~t1 : (t1 | 0x80000000u))))) ? 1u : 0u;
        t4 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t5 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = ((!(((l0 & 0x7fffffffu) > 0x7f800000u) || ((t1 & 0x7fffffffu) > 0x7f800000u)) && (((l0 << 1u) == 0u) ? 0x80000000u : (((l0 & 0x80000000u) != 0u) ? ~l0 : (l0 | 0x80000000u))) <= (((t1 << 1u) == 0u) ? 0x80000000u : (((t1 & 0x80000000u) != 0u) ? ~t1 : (t1 | 0x80000000u))))) ? 1u : 0u;
        t5 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t6 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = ((!(((l0 & 0x7fffffffu) > 0x7f800000u) || ((t1 & 0x7fffffffu) > 0x7f800000u)) && (((l0 << 1u) == 0u) ? 0x80000000u : (((l0 & 0x80000000u) != 0u) ? ~l0 : (l0 | 0x80000000u))) > (((t1 << 1u) == 0u) ? 0x80000000u : (((t1 & 0x80000000u) != 0u) ? ~t1 : (t1 | 0x80000000u))))) ? 1u : 0u;
        t6 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t7 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = ((!(((l0 & 0x7fffffffu) > 0x7f800000u) || ((t1 & 0x7fffffffu) > 0x7f800000u)) && (((l0 << 1u) == 0u) ? 0x80000000u : (((l0 & 0x80000000u) != 0u) ? ~l0 : (l0 | 0x80000000u))) != (((t1 << 1u) == 0u) ? 0x80000000u : (((t1 & 0x80000000u) != 0u) ? ~t1 : (t1 | 0x80000000u))))) ? 1u : 0u;
        t7 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t8 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = ((!(((l0 & 0x7fffffffu) > 0x7f800000u) || ((t1 & 0x7fffffffu) > 0x7f800000u)) && (((l0 << 1u) == 0u) ? 0x80000000u : (((l0 & 0x80000000u) != 0u) ? ~l0 : (l0 | 0x80000000u))) >= (((t1 << 1u) == 0u) ? 0x80000000u : (((t1 & 0x80000000u) != 0u) ? ~t1 : (t1 | 0x80000000u))))) ? 1u : 0u;
        t8 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t9 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = ((!(((l0 & 0x7fffffffu) > 0x7f800000u) || ((t1 & 0x7fffffffu) > 0x7f800000u)))) ? 1u : 0u;
        t9 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t10 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (!(!(((l0 & 0x7fffffffu) > 0x7f800000u) || ((t1 & 0x7fffffffu) > 0x7f800000u)))) ? 1u : 0u;
        t10 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t11 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (!(!(((l0 & 0x7fffffffu) > 0x7f800000u) || ((t1 & 0x7fffffffu) > 0x7f800000u)) && (((l0 << 1u) == 0u) ? 0x80000000u : (((l0 & 0x80000000u) != 0u) ? ~l0 : (l0 | 0x80000000u))) >= (((t1 << 1u) == 0u) ? 0x80000000u : (((t1 & 0x80000000u) != 0u) ? ~t1 : (t1 | 0x80000000u))))) ? 1u : 0u;
        t11 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t12 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (!(!(((l0 & 0x7fffffffu) > 0x7f800000u) || ((t1 & 0x7fffffffu) > 0x7f800000u)) && (((l0 << 1u) == 0u) ? 0x80000000u : (((l0 & 0x80000000u) != 0u) ? ~l0 : (l0 | 0x80000000u))) != (((t1 << 1u) == 0u) ? 0x80000000u : (((t1 & 0x80000000u) != 0u) ? ~t1 : (t1 | 0x80000000u))))) ? 1u : 0u;
        t12 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t13 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (!(!(((l0 & 0x7fffffffu) > 0x7f800000u) || ((t1 & 0x7fffffffu) > 0x7f800000u)) && (((l0 << 1u) == 0u) ? 0x80000000u : (((l0 & 0x80000000u) != 0u) ? ~l0 : (l0 | 0x80000000u))) > (((t1 << 1u) == 0u) ? 0x80000000u : (((t1 & 0x80000000u) != 0u) ? ~t1 : (t1 | 0x80000000u))))) ? 1u : 0u;
        t13 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t14 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (!(!(((l0 & 0x7fffffffu) > 0x7f800000u) || ((t1 & 0x7fffffffu) > 0x7f800000u)) && (((l0 << 1u) == 0u) ? 0x80000000u : (((l0 & 0x80000000u) != 0u) ? ~l0 : (l0 | 0x80000000u))) <= (((t1 << 1u) == 0u) ? 0x80000000u : (((t1 & 0x80000000u) != 0u) ? ~t1 : (t1 | 0x80000000u))))) ? 1u : 0u;
        t14 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t15 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (!(!(((l0 & 0x7fffffffu) > 0x7f800000u) || ((t1 & 0x7fffffffu) > 0x7f800000u)) && (((l0 << 1u) == 0u) ? 0x80000000u : (((l0 & 0x80000000u) != 0u) ? ~l0 : (l0 | 0x80000000u))) == (((t1 << 1u) == 0u) ? 0x80000000u : (((t1 & 0x80000000u) != 0u) ? ~t1 : (t1 | 0x80000000u))))) ? 1u : 0u;
        t15 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t16 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (!(!(((l0 & 0x7fffffffu) > 0x7f800000u) || ((t1 & 0x7fffffffu) > 0x7f800000u)) && (((l0 << 1u) == 0u) ? 0x80000000u : (((l0 & 0x80000000u) != 0u) ? ~l0 : (l0 | 0x80000000u))) < (((t1 << 1u) == 0u) ? 0x80000000u : (((t1 & 0x80000000u) != 0u) ? ~t1 : (t1 | 0x80000000u))))) ? 1u : 0u;
        t16 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t17 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint pass = (true) ? 1u : 0u;
        t17 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t18 = 0x80000000u;
    uint t19 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint pass = ((!(((t18 & 0x7fffffffu) > 0x7f800000u) || ((t1 & 0x7fffffffu) > 0x7f800000u)) && (((t18 << 1u) == 0u) ? 0x80000000u : (((t18 & 0x80000000u) != 0u) ? ~t18 : (t18 | 0x80000000u))) < (((t1 << 1u) == 0u) ? 0x80000000u : (((t1 & 0x80000000u) != 0u) ? ~t1 : (t1 | 0x80000000u))))) ? 1u : 0u;
        t19 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t20 = 0x3f800000u;
    uint t21 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint pass = (!(!(((t19 & 0x7fffffffu) > 0x7f800000u) || ((t20 & 0x7fffffffu) > 0x7f800000u)))) ? 1u : 0u;
        t21 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t22 = 0x80000000u;
    uint t23 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint pass = (!(!(((t22 & 0x7fffffffu) > 0x7f800000u) || ((t1 & 0x7fffffffu) > 0x7f800000u)) && (((t22 << 1u) == 0u) ? 0x80000000u : (((t22 & 0x80000000u) != 0u) ? ~t22 : (t22 | 0x80000000u))) == (((t1 << 1u) == 0u) ? 0x80000000u : (((t1 & 0x80000000u) != 0u) ? ~t1 : (t1 | 0x80000000u))))) ? 1u : 0u;
        t23 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    output[index] = t23;
}
