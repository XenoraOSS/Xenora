// SPDX-License-Identifier: GPL-3.0-or-later
#include <stdint.h>
#include <stdio.h>

extern uint32_t xcheck_arm64(uint32_t lane, uint32_t x);

static const uint32_t inputs[] = {
    0x00000000u, 0x00000001u, 0x00000011u, 0x00abcdefu, 0x00ffffffu,
    0x01000000u, 0x80000001u, 0xffffffffu, 0x0042f6e9u, 0x00d15ea5u,
};

int main(void) {
    for (uint32_t index = 0; index < sizeof(inputs) / sizeof(inputs[0]); ++index) {
        printf("%08x %08x\n", inputs[index], xcheck_arm64(index, inputs[index]));
    }
    return 0;
}
