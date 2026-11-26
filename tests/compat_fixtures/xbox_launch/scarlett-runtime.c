// SPDX-License-Identifier: GPL-3.0-or-later

__declspec(dllimport) long XGameRuntimeInitialize(void);
__declspec(dllimport) void XGameRuntimeUninitialize(void);

int launch_entry(void) {
    long result = XGameRuntimeInitialize();
    if (result < 0) {
        return 1;
    }
    if (XGameRuntimeInitialize() != 0) {
        return 2;
    }
    XGameRuntimeUninitialize();
    if (XGameRuntimeInitialize() != 0) {
        return 3;
    }
    XGameRuntimeUninitialize();
    XGameRuntimeUninitialize();
    return 0;
}
