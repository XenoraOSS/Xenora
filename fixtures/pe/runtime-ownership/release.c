// SPDX-License-Identifier: GPL-3.0-or-later

__declspec(dllimport) void free(void *block);
__declspec(dllimport) int *_errno(void);
__declspec(dllimport) unsigned int XenoraOwnershipMarker;

__declspec(dllexport) int XenoraValidateAndRelease(unsigned char *bytes) {
    int *error = _errno();
    int status = 0;
    if (*error != 41 || XenoraOwnershipMarker != 0x6ace) {
        status = 22;
    } else if (bytes[0] != 9 || bytes[1] != 37 || bytes[2] != 81 || bytes[3] != 143) {
        status = 23;
    }
    free(bytes);
    *error = 51;
    return status;
}
