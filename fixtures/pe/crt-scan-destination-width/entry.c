/* SPDX-License-Identifier: GPL-3.0-or-later */
typedef unsigned long long u64;
typedef unsigned short wide;
__declspec(dllimport) int __stdio_common_vsscanf(
    u64, const char *, u64, const char *, void *, __builtin_ms_va_list);
__declspec(dllimport) __declspec(noreturn) void ExitProcess(unsigned);

static int scan(const char *input, const char *format, ...) {
    __builtin_ms_va_list args;
    __builtin_ms_va_start(args, format);
    const u64 options = TEST_CASE == 1 || TEST_CASE == 3 ? 1 : 2;
    int result = __stdio_common_vsscanf(options, input, ~(u64)0, format, 0, args);
    __builtin_ms_va_end(args);
    return result;
}

void entry(void) {
    wide w[4] = {0x5151, 0x5252, 0x5353, 0x5454};
#if TEST_CASE == 2
    scan("\x80", "%lc", w);
    ExitProcess(40);
#elif TEST_CASE == 3
    scan(" abZ", "%2ls", w, 2u);
    ExitProcess(41);
#else
    char n[4] = {'!', '!', '!', '!'};
    int used = -1;
#if TEST_CASE == 1
    if (scan(" XY", "%2C%n", w, 2u, &used) != 1 ||
        w[0] != ' ' || w[1] != 'X' || w[2] != 0x5353 || used != 2)
        ExitProcess(21);
    if (scan(" abZ", "%2ls%n", w, 3u, &used) != 1 ||
        w[0] != 'a' || w[1] != 'b' || w[2] != 0 || w[3] != 0x5454 || used != 3)
        ExitProcess(22);
    w[2] = 0x5353;
    if (scan("AZ", "%1l[A-Z]%n", w, 2u, &used) != 1 ||
        w[0] != 'A' || w[1] != 0 || w[2] != 0x5353 || w[3] != 0x5454 || used != 1)
        ExitProcess(23);
    if (scan("a b", "%hC%hS", n, 1u, n + 1, 2u) != 2 ||
        n[0] != 'a' || n[1] != 'b' || n[2] != 0 || n[3] != '!')
        ExitProcess(24);
#else
    if (scan(" x", "%2lc%n", w, &used) != 1 ||
        w[0] != ' ' || w[1] != 'x' || w[2] != 0x5353 || used != 2)
        ExitProcess(11);
    if (scan(" hi!", "%2S%n", w, &used) != 1 ||
        w[0] != 'h' || w[1] != 'i' || w[2] != 0 || w[3] != 0x5454 || used != 3)
        ExitProcess(12);
    if (scan("Q Z", "%lC%lS", w, w + 1) != 2 ||
        w[0] != 'Q' || w[1] != 'Z' || w[2] != 0 || w[3] != 0x5454)
        ExitProcess(13);
    if (scan("ab!", "%*2l[a-z]%hC%n", n, &used) != 1 ||
        n[0] != '!' || n[1] != '!' || used != 3)
        ExitProcess(14);
    if (scan("xy!", "%2hS%n", n, &used) != 1 ||
        n[0] != 'x' || n[1] != 'y' || n[2] != 0 || n[3] != '!' || used != 2)
        ExitProcess(15);
    if (scan("!", "%l[a-z]", w) != 0 || w[0] != 'Q' || w[1] != 'Z')
        ExitProcess(16);
    if (scan("\x80", "%hC", n) != 1 || (unsigned char)n[0] != 0x80 || n[1] != 'y')
        ExitProcess(17);
#endif
    ExitProcess(0);
#endif
}
