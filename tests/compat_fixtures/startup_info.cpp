// SPDX-License-Identifier: GPL-3.0-or-later
//
// An original hosted C++ program: the mingw-w64 C runtime's startup code runs
// first, then a constructor reads the process's startup information and command
// line before main, and main compares them with what it receives.
//
// The structure and the functions are the public Windows headers', so the
// layout assertions below are independent of startup_info.c.
//
// Build with tests/compat_fixtures/build-startup-info.sh. The test that runs
// it is crates/loader/tests/startup_info.rs.

#include <cstddef>
#include <cstdio>
#include <cstring>
#include <windows.h>

static_assert(sizeof(STARTUPINFOW) == 104 && sizeof(STARTUPINFOA) == 104);
static_assert(offsetof(STARTUPINFOW, lpDesktop) == 16 && offsetof(STARTUPINFOW, dwX) == 32);
static_assert(offsetof(STARTUPINFOW, dwFlags) == 60 && offsetof(STARTUPINFOW, wShowWindow) == 64);
static_assert(offsetof(STARTUPINFOW, hStdInput) == 80);

namespace {

struct Early {
    STARTUPINFOW info;
    STARTUPINFOA narrow;
    const char *line;
    const wchar_t *wide;

    Early() {
        std::memset(&info, 0xCC, sizeof info);
        std::memset(&narrow, 0xCC, sizeof narrow);
        GetStartupInfoW(&info);
        GetStartupInfoA(&narrow);
        line = GetCommandLineA();
        wide = GetCommandLineW();
    }
} early;

// The same text in both encodings; the program is given ASCII only.
bool matches(const char *text, const wchar_t *wide) {
    for (; *text; text++, wide++)
        if (static_cast<wchar_t>(*text) != *wide) return false;
    return *wide == 0;
}

}  // namespace

int main(int argc, char **argv) {
    if (early.line != GetCommandLineA() || early.wide != GetCommandLineW()) return 1;
    if (!matches(early.line, early.wide)) return 2;
    if (early.info.cb != sizeof early.info || early.info.dwFlags != early.narrow.dwFlags) return 3;
    std::printf("line %s\n", early.line);
    for (int i = 0; i < argc; i++) std::printf("argv %d %s\n", i, argv[i]);
    std::printf("title %s\n", early.narrow.lpTitle ? early.narrow.lpTitle : "(none)");
    // What a Windows program's startup code passes on as nCmdShow.
    int show = early.info.dwFlags & STARTF_USESHOWWINDOW ? early.info.wShowWindow : SW_SHOWDEFAULT;
    std::printf("show %d\n", show);
    return 0;
}
