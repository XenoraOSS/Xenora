// SPDX-License-Identifier: GPL-3.0-or-later

typedef unsigned int u32;
typedef unsigned long long u64;
__declspec(dllimport) void PhaseProbe(u32 marker);

static u32 attached;
static unsigned char tls_template[1] = {1};
static u32 tls_index;

static void tls_callback(void *instance, u32 reason, void *reserved) {
    (void)instance;
    (void)reserved;
    if (reason == 1)
        PhaseProbe(4);
}

static void *tls_callbacks[2] = {(void *)tls_callback, 0};
typedef struct {
    u64 start, end, index, callbacks;
    u32 zero_fill, characteristics;
} TlsDirectory;
__declspec(allocate(".rdata$T")) const TlsDirectory _tls_used = {
    (u64)tls_template, (u64)tls_template + sizeof(tls_template),
    (u64)&tls_index, (u64)tls_callbacks, 0, 0,
};

int DllMain(void *instance, u32 reason, void *reserved) {
    (void)instance;
    (void)reserved;
    if (reason == 1) {
        PhaseProbe(1);
#ifdef REJECT_ATTACH
        return 0;
#else
        attached = 7;
#endif
    } else if (reason == 0) {
        PhaseProbe(3);
        attached = 0;
    }
    return 1;
}

__declspec(dllexport) u32 ProviderValue(void) {
    return attached;
}
