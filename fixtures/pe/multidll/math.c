// SPDX-License-Identifier: GPL-3.0-or-later
//
// Original Xenora fixture. A leaf provider DLL with no CRT dependency: one
// exported function, exported variables with nonzero initial values, a TLS
// template with one callback, and an exported initialization marker written
// by DllMain. The TLS directory is declared explicitly because this fixture
// intentionally links without a CRT. Nothing here is derived from any
// commercial binary.

typedef unsigned int u32;
typedef unsigned long long u64;

__declspec(dllexport) u32 XenoraMathInit;
__declspec(dllexport) u32 XenoraMathTls;
__declspec(dllexport) u32 XenoraMathDetach;
__declspec(dllexport) u32 XenoraScale = 5;

__declspec(dllexport) u32 XenoraAdd(u32 a, u32 b) {
    return a + b;
}

/* One thread's TLS template. The loader copies it into the thread block. */
__declspec(allocate(".data")) static unsigned char xenora_tls_template[8] = {
    1, 2, 3, 4, 5, 6, 7, 8,
};
static u32 xenora_tls_index;

static void xenora_tls_callback(void *module, u32 reason, void *reserved) {
    (void)module;
    (void)reserved;
    if (reason == 1) {
        XenoraMathTls = 7;
    }
}

__declspec(allocate(".data")) static void *xenora_tls_callbacks[2] = {
    (void *)xenora_tls_callback,
    0,
};

typedef struct {
    u64 start;
    u64 end;
    u64 index;
    u64 callbacks;
    u32 zero_fill;
    u32 characteristics;
} TlsDirectory;

/* The linker recognises `_tls_used` and emits data directory 9 from it. */
__declspec(allocate(".rdata$T")) const TlsDirectory _tls_used = {
    (u64)xenora_tls_template,
    (u64)xenora_tls_template + sizeof(xenora_tls_template),
    (u64)&xenora_tls_index,
    (u64)xenora_tls_callbacks,
    0,
    0,
};

int DllMain(void *module, u32 reason, void *reserved) {
    (void)module;
    (void)reserved;
    if (reason == 1) {
        XenoraMathInit = 1;
    } else if (reason == 0) {
        XenoraMathDetach = 1;
    }
    return 1;
}
