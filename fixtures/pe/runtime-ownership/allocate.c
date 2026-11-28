// SPDX-License-Identifier: GPL-3.0-or-later

__declspec(dllimport) void *malloc(unsigned long long size);
__declspec(dllimport) int *_errno(void);

__declspec(dllexport) unsigned int XenoraOwnershipMarker = 0x1254;

__declspec(dllexport) unsigned char *XenoraAllocate(void) {
    unsigned char *bytes = malloc(4);
    if (bytes) {
        bytes[0] = 9;
        bytes[1] = 37;
        bytes[2] = 81;
        bytes[3] = 143;
        *_errno() = 31;
    }
    return bytes;
}
