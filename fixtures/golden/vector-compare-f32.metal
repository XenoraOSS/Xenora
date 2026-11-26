#include <metal_stdlib>
using namespace metal;
kernel void xenora_compute(
    device const uint* input [[buffer(0)]],
    device uint* output [[buffer(1)]],
    uint index [[thread_position_in_grid]],
    uint grid [[threads_per_grid]]) {
    uint t0 = input[index];
    uint t1 = 0x00000000u;
    uint t2 = 0x00000001u;
    uint t3 = 0x3fc00000u;
    uint t4 = 0x3fc00000u;
    uint t5 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (as_type<float>(t4) < as_type<float>(l0)) ? 1u : 0u;
        t5 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t6 = t1 ^ t5;
    uint t7 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (as_type<float>(l0) == as_type<float>(l0)) ? 1u : 0u;
        t7 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t8 = t6 << (t2 & 31u);
    uint t9 = t8 ^ t7;
    uint t10 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (isnan(as_type<float>(l0)) || isnan(as_type<float>(l0))) ? 1u : 0u;
        t10 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t11 = t9 << (t2 & 31u);
    uint t12 = t11 ^ t10;
    uint t13 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (!isnan(as_type<float>(l0)) && !isnan(as_type<float>(l0))) ? 1u : 0u;
        t13 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t14 = t12 << (t2 & 31u);
    uint t15 = t14 ^ t13;
    uint t16 = 0x3f400000u;
    uint t17 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = ((as_type<float>(t16) < as_type<float>(l0)) || (as_type<float>(t16) > as_type<float>(l0))) ? 1u : 0u;
        t17 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t18 = t15 << (t2 & 31u);
    uint t19 = t18 ^ t17;
    uint t20 = 0x3fc00000u;
    uint t21 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (!(as_type<float>(t20) > as_type<float>(l0))) ? 1u : 0u;
        t21 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t22 = t19 << (t2 & 31u);
    uint t23 = t22 ^ t21;
    uint t24 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (as_type<float>(l0) <= as_type<float>(l0)) ? 1u : 0u;
        t24 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t25 = t23 << (t2 & 31u);
    uint t26 = t25 ^ t24;
    uint t27 = 0x3fc00000u;
    uint t28 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (as_type<float>(t27) > as_type<float>(l0)) ? 1u : 0u;
        t28 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t29 = t26 << (t2 & 31u);
    uint t30 = t29 ^ t28;
    uint t31 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (as_type<float>(l0) >= as_type<float>(t3)) ? 1u : 0u;
        t31 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t32 = t30 << (t2 & 31u);
    uint t33 = t32 ^ t31;
    uint t34 = 0x3fc00000u;
    uint t35 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (!(as_type<float>(t34) >= as_type<float>(l0))) ? 1u : 0u;
        t35 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t36 = t33 << (t2 & 31u);
    uint t37 = t36 ^ t35;
    uint t38 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (!((as_type<float>(l0) < as_type<float>(l0)) || (as_type<float>(l0) > as_type<float>(l0)))) ? 1u : 0u;
        t38 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t39 = t37 << (t2 & 31u);
    uint t40 = t39 ^ t38;
    uint t41 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (!(as_type<float>(l0) <= as_type<float>(t3))) ? 1u : 0u;
        t41 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t42 = t40 << (t2 & 31u);
    uint t43 = t42 ^ t41;
    uint t44 = 0x3fc00000u;
    uint t45 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (!(as_type<float>(t44) == as_type<float>(l0))) ? 1u : 0u;
        t45 |= ((0x0f0f0f0fu >> lane) & pass) << lane;
    }
    uint t46 = t43 << (t2 & 31u);
    uint t47 = t46 ^ t45;
    uint t48 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint l0 = input[lane_index];
        uint pass = (!(as_type<float>(t45) < as_type<float>(l0))) ? 1u : 0u;
        t48 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t49 = t47 << (t2 & 31u);
    uint t50 = t49 ^ t48;
    uint t51 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint pass = (true) ? 1u : 0u;
        t51 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t53 = t50 ^ t0;
    uint t54 = t51 != 0u ? t53 : t50;
    uint t55 = 0u;
    for (uint lane = 0u; lane < 32u; ++lane) {
        uint lane_index = (index & ~31u) + lane;
        if (lane_index >= grid) { break; }
        uint pass = (false) ? 1u : 0u;
        t55 |= ((0xffffffffu >> lane) & pass) << lane;
    }
    uint t57 = 0x00000000u;
    uint t58 = t55 != 0u ? t57 : t54;
    output[index] = t58;
}
