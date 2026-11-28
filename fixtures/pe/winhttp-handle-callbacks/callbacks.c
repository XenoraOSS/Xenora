// SPDX-License-Identifier: GPL-3.0-or-later

typedef unsigned int u32;
typedef unsigned long long u64;
typedef void (*status_callback)(u64 handle, u64 context, u32 status, void *information, u32 length);

#define IMPORT __declspec(dllimport)
#define CHECK(condition, code) do { if (!(condition)) ExitProcess(code); } while (0)
#define INVALID_CALLBACK ((status_callback)~0ull)

IMPORT u64 WinHttpOpen(const unsigned short *agent, u32 access, const void *proxy,
                       const void *bypass, u32 flags);
IMPORT u64 WinHttpConnect(u64 session, const unsigned short *server, unsigned short port, u32 reserved);
IMPORT u64 WinHttpOpenRequest(u64 connection, const void *verb, const void *path, const void *version,
                              const void *referer, const void *accept, u32 flags);
IMPORT status_callback WinHttpSetStatusCallback(u64 handle, status_callback callback, u32 mask, u64 reserved);
IMPORT int WinHttpSetOption(u64 handle, u32 option, const void *value, u32 length);
IMPORT int WinHttpQueryOption(u64 handle, u32 option, void *value, u32 *length);
IMPORT int WinHttpCloseHandle(u64 handle);
IMPORT u32 GetLastError(void);
IMPORT __declspec(noreturn) void ExitProcess(u32 status);

enum {
    CONTEXT_VALUE = 78,
    CREATED = 0x400,
    CLOSING = 0x800,
    HANDLE_NOTIFICATIONS = CREATED | CLOSING,
    UNSUPPORTED_NOTIFICATION = 0x100,
    INVALID_PARAMETER = 87,
};

static const u64 session_context = 0x147f0258a1b2c3d4ull;
static const u64 connection_context = 0x8e8d1623f0e1d2c3ull;
static const u64 request_context = 0x7777666655554444ull;
static const unsigned short server[] = {'e', 'x', 'a', 'm', 'p', 'l', 'e', '.', 't', 'e', 's', 't', 0};
static u64 live_session;

static struct {
    u32 callback;
    u32 status;
    u64 handle;
    u64 context;
    u64 observed_payload;
} expected;

static void expect_event(u32 callback, u32 status, u64 handle, u64 context) {
    CHECK(expected.callback == 0, 10);
    expected.callback = callback;
    expected.status = status;
    expected.handle = handle;
    expected.context = context;
    expected.observed_payload = 0;
}

static void check_event(u64 payload) {
    CHECK(expected.callback == 0, 11);
    CHECK(payload != 0 && expected.observed_payload == payload, 12);
}

static u64 query_context(u64 handle) {
    u32 size = sizeof(u64);
    u64 value = 0;
    CHECK(WinHttpQueryOption(handle, CONTEXT_VALUE, &value, &size), 20);
    CHECK(size == sizeof(value), 21);
    return value;
}

static void observe(u32 callback, u64 handle, u64 context, u32 status, void *information, u32 length) {
    CHECK(callback == expected.callback, 13);
    CHECK(handle == expected.handle, 14);
    CHECK(context == expected.context, 15);
    CHECK(status == expected.status, 16);
    CHECK(information != 0 && length == sizeof(u64), 17);
    const u64 payload = *(const u64 *)information;
    CHECK(payload != 0, 18);
    // This acquires the WinHTTP table again while the callback is active.
    CHECK(query_context(live_session) == session_context, 19);
    CHECK(*(const u64 *)information == payload, 22);
    expected.observed_payload = payload;
    expected.callback = 0;
}

static void first(u64 handle, u64 context, u32 status, void *information, u32 length) {
    observe(1, handle, context, status, information, length);
    // A void Win64 callback need not preserve RAX. The calling API must
    // still return its own saved HINTERNET or BOOL after this callback.
    __asm__ volatile("movabsq $0x123456789abcdef0, %%rax" : : : "rax");
}

static void second(u64 handle, u64 context, u32 status, void *information, u32 length) {
    observe(2, handle, context, status, information, length);
    __asm__ volatile("movabsq $0xaabbccddeeff8877, %%rax" : : : "rax");
}

void entry(void) {
    live_session = WinHttpOpen(0, 1, 0, 0, 0);
    CHECK(live_session != 0, 30);
    CHECK(query_context(live_session) == 0, 31);
    CHECK(WinHttpSetOption(live_session, CONTEXT_VALUE, &session_context, sizeof(session_context)), 32);
    CHECK(WinHttpSetStatusCallback(live_session, first, HANDLE_NOTIFICATIONS, 0) == 0, 33);

    // Failed setter requests must leave the first callback installed.
    CHECK(WinHttpSetStatusCallback(live_session, second, CREATED, 1) == INVALID_CALLBACK, 34);
    CHECK(GetLastError() == INVALID_PARAMETER, 35);
    CHECK(WinHttpSetStatusCallback(live_session, second, CREATED | UNSUPPORTED_NOTIFICATION, 0) ==
          INVALID_CALLBACK, 36);
    CHECK(GetLastError() == INVALID_PARAMETER, 37);
    CHECK(WinHttpSetStatusCallback(live_session, (status_callback)&expected, CREATED, 0) ==
          INVALID_CALLBACK, 54);
    CHECK(GetLastError() == INVALID_PARAMETER, 55);

    expect_event(1, CREATED, live_session, session_context);
    const u64 first_connection = WinHttpConnect(live_session, server, 80, 0);
    check_event(first_connection);
    CHECK(query_context(first_connection) == 0, 38);
    CHECK(WinHttpSetOption(first_connection, CONTEXT_VALUE, &connection_context, sizeof(connection_context)), 39);

    CHECK(WinHttpSetStatusCallback(live_session, second, CREATED, 0) == first, 40);
    expect_event(2, CREATED, live_session, session_context);
    const u64 second_connection = WinHttpConnect(live_session, server, 80, 0);
    check_event(second_connection);
    CHECK(query_context(second_connection) == 0, 41);
    CHECK(WinHttpSetOption(second_connection, CONTEXT_VALUE, &connection_context, sizeof(connection_context)), 42);
    // This child has a nonzero context but inherited a creation-only mask.
    CHECK(WinHttpCloseHandle(second_connection) == 1, 43);

    CHECK(WinHttpSetStatusCallback(live_session, 0, ~0u, 0) == second, 44);
    const u64 silent_connection = WinHttpConnect(live_session, server, 80, 0);
    CHECK(silent_connection != 0, 45);
    CHECK(WinHttpCloseHandle(silent_connection) == 1, 46);

    // Replacement and removal on the session leave its existing child alone.
    expect_event(1, CREATED, first_connection, connection_context);
    const u64 zero_context_request = WinHttpOpenRequest(first_connection, 0, 0, 0, 0, 0, 0);
    check_event(zero_context_request);
    CHECK(query_context(zero_context_request) == 0, 47);
    CHECK(WinHttpCloseHandle(zero_context_request) == 1, 48);

    expect_event(1, CREATED, first_connection, connection_context);
    const u64 request = WinHttpOpenRequest(first_connection, 0, 0, 0, 0, 0, 0);
    check_event(request);
    CHECK(WinHttpSetOption(request, CONTEXT_VALUE, &request_context, sizeof(request_context)), 49);

    expect_event(1, CLOSING, first_connection, connection_context);
    CHECK(WinHttpCloseHandle(first_connection) == 1, 52);
    check_event(first_connection);

    // Closing the ancestor invalidates request operations. Explicit close
    // still delivers the one notification with the request's saved context.
    u64 context = 0;
    u32 length = sizeof(context);
    CHECK(!WinHttpQueryOption(request, CONTEXT_VALUE, &context, &length) &&
          GetLastError() == 6, 56);
    expect_event(1, CLOSING, request, request_context);
    CHECK(WinHttpCloseHandle(request) == 1, 50);
    check_event(request);
    CHECK(WinHttpCloseHandle(request) == 0 && GetLastError() == 6, 51);
    CHECK(WinHttpCloseHandle(live_session) == 1, 53);
    ExitProcess(0);
}
