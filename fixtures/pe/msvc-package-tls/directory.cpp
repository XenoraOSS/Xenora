// SPDX-License-Identifier: GPL-3.0-or-later

// Clang emits TLS data and offsets between the linker section markers.
#pragma section(".tls", read, write)
#pragma section(".tls$ZZZ", read, write)

extern "C" {
__declspec(allocate(".tls")) unsigned char start_tls = 0;
__declspec(allocate(".tls$ZZZ")) unsigned char end_tls = 0;
unsigned int _tls_index = 0;

struct TlsDirectory {
    const void *template_start;
    const void *template_end;
    unsigned int *index;
    const void *callbacks;
    unsigned int zero_fill;
    unsigned int characteristics;
};
static_assert(sizeof(TlsDirectory) == 40);
static const void *const no_callbacks[] = {nullptr};
extern const TlsDirectory _tls_used = {&start_tls, &end_tls, &_tls_index, no_callbacks, 0, 0};
}
