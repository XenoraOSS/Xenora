// SPDX-License-Identifier: GPL-3.0-or-later

// Original freestanding AMD64 guest. A failed check exits with its source line.
typedef unsigned int u32;
typedef unsigned short wchar_t;
typedef void *handle;

#define IMPORT __declspec(dllimport)
#define CHECK(condition) do { if (!(condition)) ExitProcess(__LINE__); } while (0)
#define ADD 0x20000000u
#define REPLACE 0x80000000u
#define ADD_IF_NEW 0x10000000u
#define COALESCE_COMMA 0x40000000u
#define HEADER_NOT_FOUND 12150u
#define NOT_SUPPORTED 50u

IMPORT __declspec(noreturn) void ExitProcess(u32);
IMPORT const char *GetCommandLineA(void);
IMPORT u32 GetLastError(void);
IMPORT handle WinHttpOpen(const wchar_t *, u32, const wchar_t *, const wchar_t *, u32);
IMPORT handle WinHttpConnect(handle, const wchar_t *, unsigned short, u32);
IMPORT handle WinHttpOpenRequest(handle, const wchar_t *, const wchar_t *, const wchar_t *,
                                 const wchar_t *, const wchar_t *const *, u32);
IMPORT int WinHttpAddRequestHeaders(handle, const wchar_t *, u32, u32);
IMPORT int WinHttpSendRequest(handle, const wchar_t *, u32, void *, u32, u32, unsigned long long);
IMPORT int WinHttpReceiveResponse(handle, void *);
IMPORT int WinHttpCloseHandle(handle);

static unsigned short port_argument(void) {
    const char *cursor = GetCommandLineA(), *digits = cursor;
    while (*cursor) {
        if (*cursor == ' ') digits = cursor + 1;
        ++cursor;
    }
    u32 port = 0;
    while (*digits >= '0' && *digits <= '9') {
        port = port * 10 + (u32)(*digits++ - '0');
        CHECK(port <= 65535);
    }
    CHECK(port && !*digits);
    return (unsigned short)port;
}

void entry(void) {
    handle session = WinHttpOpen(0, 1, 0, 0, 0);
    CHECK(session);
    handle connection = WinHttpConnect(session, L"127.0.0.1", port_argument(), 0);
    CHECK(connection);
    handle request = WinHttpOpenRequest(connection, L"GET", L"/indexed", 0, 0, 0, 0);
    CHECK(request);
    CHECK(WinHttpAddRequestHeaders(request, L"X-List: first\r\nX-Unrelated: stays\r\n"
                                           L"x-list: second\r\nX-List: third\r\n"
                                           L"X-List: fourth", ~0u, ADD));
    CHECK(WinHttpAddRequestHeaders(request, L"x-LIsT: replacement", ~0u, REPLACE | 1));
    CHECK(WinHttpAddRequestHeaders(request, L"X-LIST:", ~0u, REPLACE | 2));
    CHECK(!WinHttpAddRequestHeaders(request, L"x-list: should-not-append", ~0u,
                                    REPLACE | ADD | 9));
    CHECK(GetLastError() == HEADER_NOT_FOUND);
    CHECK(!WinHttpAddRequestHeaders(request, L"X-Missing:", ~0u, REPLACE | 1));
    CHECK(GetLastError() == HEADER_NOT_FOUND);
    CHECK(!WinHttpAddRequestHeaders(request, L"x-list: reserved", ~0u, REPLACE | 0x100));
    CHECK(GetLastError() == NOT_SUPPORTED);
    CHECK(!WinHttpAddRequestHeaders(request, L"x-list: unrelated-index", ~0u, COALESCE_COMMA | 1));
    CHECK(GetLastError() == NOT_SUPPORTED);
    CHECK(WinHttpAddRequestHeaders(request, L"X-List: default", ~0u, REPLACE));
    CHECK(WinHttpAddRequestHeaders(request, L"X-Added: fallback", ~0u, REPLACE | ADD));
    CHECK(WinHttpAddRequestHeaders(request, L"X-Control: one", ~0u, ADD_IF_NEW));
    CHECK(WinHttpAddRequestHeaders(request, L"X-Control: two", ~0u, COALESCE_COMMA));
    CHECK(WinHttpSendRequest(request, 0, 0, 0, 0, 0, 0));
    CHECK(WinHttpReceiveResponse(request, 0));
    CHECK(WinHttpCloseHandle(request) && WinHttpCloseHandle(connection) && WinHttpCloseHandle(session));
    ExitProcess(0);
}
