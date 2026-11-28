// SPDX-License-Identifier: GPL-3.0-or-later

__declspec(dllimport) int *_errno(void);
__declspec(dllimport) void *GetProcessHeap(void);
__declspec(dllimport) unsigned long long HeapSize(void *heap, unsigned int flags, const void *block);
__declspec(dllimport) unsigned int XenoraOwnershipMarker;
__declspec(dllimport) unsigned char *XenoraAllocate(void);
__declspec(dllimport) int XenoraValidateAndRelease(unsigned char *bytes);

int entry(void) {
    int *error = _errno();
    if (*error != 0 || XenoraOwnershipMarker != 0x1254) {
        return 11;
    }
    unsigned char *bytes = XenoraAllocate();
    if (!bytes || *error != 31) {
        return 12;
    }

    // These heap queries check Xenora's synthetic allocation bookkeeping.
    void *heap = GetProcessHeap();
    if (HeapSize(heap, 0, bytes) != 4) {
        return 13;
    }
    unsigned long long address = (unsigned long long)bytes;
    *error = 41;
    XenoraOwnershipMarker = 0x6ace;
#ifdef XENORA_CORRUPT_PAYLOAD
    bytes[2] = 0;
#endif
    int status = XenoraValidateAndRelease(bytes);
    if (*error != 51 || HeapSize(heap, 0, (void *)address) != ~0ULL) {
        return 14;
    }
    return status ? status : 70;
}
