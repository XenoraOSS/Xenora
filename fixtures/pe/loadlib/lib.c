// SPDX-License-Identifier: GPL-3.0-or-later
//
// Original Xenora fixture. A no-CRT provider DLL for the dynamic-load slice:
// one exported function, exported initialization evidence written by DllMain,
// and an exported TLS marker written by one callback. The loader must prepare
// and initialize this image before the application's indirect call can see
// consistent evidence. Nothing here is derived from a commercial binary.

typedef unsigned int u32;
typedef unsigned long long u64;

__declspec(dllexport) u32 XenoraLibInit;
__declspec(dllexport) u32 XenoraLibTls;
__declspec(dllexport) u32 XenoraLibDetach;

#ifndef XENORA_NO_TRIPLE
__declspec(dllexport) u32 XenoraTriple(u32 value) {
    return value * 3u;
}
#endif

/* One thread's TLS template. The loader copies it into the thread block. */
__declspec(allocate(".data")) static unsigned char xenora_tls_template[4] = {
    7, 0, 0, 0,
};
static u32 xenora_tls_index;

static void xenora_tls_callback(void *module, u32 reason, void *reserved) {
    (void)module;
    (void)reserved;
    if (reason == 1) {
        XenoraLibTls = 7;
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
#ifndef XENORA_FAIL_INIT
        XenoraLibInit = 0xA5A5u;
#endif
    } else if (reason == 0) {
        XenoraLibDetach = 1;
    }
#ifdef XENORA_FAIL_INIT
    /* Negative control: the initializer refuses PROCESS_ATTACH. */
    return 0;
#else
    return 1;
#endif
}
