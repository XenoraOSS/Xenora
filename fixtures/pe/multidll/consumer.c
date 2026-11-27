// SPDX-License-Identifier: GPL-3.0-or-later
//
// Original Xenora fixture. A no-CRT consumer EXE that imports a function and
// a data variable from two package DLLs plus ExitProcess from kernel32. The
// exit status is a checksum of every binding and of the provider
// initialization order, so a wrong base, an unbound slot, a skipped DllMain or
// an inverted order changes the observed status.

__declspec(dllimport) unsigned int XenoraAdd(unsigned int a, unsigned int b);
__declspec(dllimport) unsigned int XenoraScale;
__declspec(dllimport) unsigned int XenoraQuad(unsigned int value);
__declspec(dllimport) unsigned int XenoraMathInit;
__declspec(dllimport) unsigned int XenoraChainInit;
__declspec(dllimport) unsigned int XenoraMathTls;
__declspec(dllimport) void ExitProcess(unsigned int status);

void entry(void) {
    unsigned int checksum = 0;
    checksum += XenoraAdd(20, 22); /* 42 */
    checksum += XenoraQuad(2);     /* 8, through chain -> math */
    checksum += XenoraScale * 2;   /* 10 */
    checksum += XenoraMathInit;    /* 1, leaf DllMain */
    checksum += XenoraChainInit;   /* 2, middle DllMain after the leaf */
    checksum += XenoraMathTls;     /* 7, leaf TLS callback before DllMain */
    ExitProcess(checksum);
}
