// SPDX-License-Identifier: GPL-3.0-or-later

// Original AMD64 Windows guest. A failed check exits with its source line.
typedef unsigned int u32;
typedef unsigned short atom;
typedef unsigned long long uptr;
typedef long long iptr;
typedef void *handle;
typedef iptr (*window_proc)(handle, u32, uptr, iptr);

struct window_class {
    u32 size, style;
    window_proc proc;
    int class_extra, window_extra;
    handle instance, icon, cursor, background;
    const char *menu_name, *class_name;
    handle small_icon;
};

struct create_struct {
    void *params;
    handle instance, menu, parent;
    int height, width, y, x, style;
    const char *title, *class_name;
    u32 exstyle;
};

struct message {
    handle window;
    u32 id;
    uptr wparam;
    iptr lparam;
    u32 time;
    int x, y;
    u32 private_data;
};

_Static_assert(sizeof(struct window_class) == 80, "AMD64 WNDCLASSEXA");
_Static_assert(sizeof(struct create_struct) == 80, "AMD64 CREATESTRUCTA");
_Static_assert(sizeof(struct message) == 48, "AMD64 MSG");

#define IMPORT __declspec(dllimport)
IMPORT atom RegisterClassExA(const struct window_class *);
IMPORT handle CreateWindowExA(u32, const char *, const char *, u32, int, int, int, int,
                             handle, handle, handle, void *);
IMPORT iptr DefWindowProcA(handle, u32, uptr, iptr);
IMPORT int ShowWindow(handle, int);
IMPORT iptr DispatchMessageA(const struct message *);
IMPORT int PeekMessageA(struct message *, handle, u32, u32, u32);
IMPORT int TranslateMessage(const struct message *);
IMPORT int PostMessageA(handle, u32, uptr, iptr);
IMPORT int DestroyWindow(handle);
IMPORT int IsWindowVisible(handle);
IMPORT int UnregisterClassA(const char *, handle);
IMPORT handle CreateThread(void *, uptr, u32 (*)(void *), void *, u32, u32 *);
IMPORT u32 WaitForSingleObject(handle, u32);
IMPORT int CloseHandle(handle);
IMPORT u32 GetLastError(void);
IMPORT __declspec(noreturn) void ExitProcess(u32);

#define CHECK(condition) do { if (!(condition)) ExitProcess(__LINE__); } while (0)
#define WM_CREATE 0x0001u
#define WM_DESTROY 0x0002u
#define WM_CLOSE 0x0010u
#define WM_SHOWWINDOW 0x0018u
#define WM_NCCREATE 0x0081u
#define WM_NCDESTROY 0x0082u
#define PAYLOAD_MESSAGE 0x8001u
#define OTHER_MESSAGE 0x8002u
#define PAYLOAD_WPARAM 0xfedcba9876543210ull
#define PAYLOAD_LPARAM 0x1234567887654321ll
#define PAYLOAD_RESULT 0x23456789abcd1234ll
#define PM_REMOVE 1u
#define DESTROY_ON_CREATE (-1)

extern char __ImageBase;
static const char class_name[] = "Xenora1357Window";
static const char title[] = "Original synthetic window";
static u32 last_phase, delivery_count;
static handle callback_window;
static handle volatile worker_window;
static int reject_phase;
static int tag;

static int same_text(const char *left, const char *right) {
    while (*left && *left == *right) {
        ++left;
        ++right;
    }
    return *left == *right;
}

static iptr on_message(handle window, u32 message, uptr wparam, iptr lparam) {
    switch (message) {
    case WM_NCCREATE:
    case WM_CREATE: {
        const struct create_struct *create = (const struct create_struct *)lparam;
        CHECK(create && wparam == 0);
        CHECK(create->params == &tag && create->instance == &__ImageBase);
        CHECK(!create->menu && !create->parent && !create->style && !create->exstyle);
        CHECK(create->x == 11 && create->y == 23 && create->width == 160 && create->height == 96);
        CHECK(same_text(create->title, title) && same_text(create->class_name, class_name));
        CHECK(window && !IsWindowVisible(window));
        if (message == WM_NCCREATE) {
            CHECK(last_phase == 0);
            callback_window = window;
            last_phase = WM_NCCREATE;
            return reject_phase == WM_NCCREATE ? 0 : DefWindowProcA(window, message, wparam, lparam);
        }
        CHECK(last_phase == WM_NCCREATE && callback_window == window);
        last_phase = WM_CREATE;
        if (reject_phase == DESTROY_ON_CREATE)
            CHECK(DestroyWindow(window) && last_phase == WM_NCDESTROY);
        return reject_phase == WM_CREATE ? -1 : 0;
    }
    case PAYLOAD_MESSAGE:
        CHECK(window == callback_window && last_phase == WM_CREATE);
        CHECK(wparam == PAYLOAD_WPARAM && lparam == PAYLOAD_LPARAM);
        CHECK(!IsWindowVisible(window));
        ++delivery_count;
        return PAYLOAD_RESULT;
    case WM_SHOWWINDOW:
        CHECK((IsWindowVisible(window) != 0) != (wparam != 0));
        return DefWindowProcA(window, message, wparam, lparam);
    case WM_DESTROY:
        CHECK(callback_window == window);
        last_phase = WM_DESTROY;
        return 0;
    case WM_NCDESTROY:
        CHECK(callback_window == window);
        if (reject_phase != WM_NCCREATE)
            CHECK(last_phase == WM_DESTROY);
        last_phase = WM_NCDESTROY;
        return DefWindowProcA(window, message, wparam, lparam);
    default:
        return DefWindowProcA(window, message, wparam, lparam);
    }
}

static handle create_window(void) {
    last_phase = 0;
    callback_window = 0;
    return CreateWindowExA(0, class_name, title, 0, 11, 23, 160, 96,
                           0, 0, &__ImageBase, &tag);
}

static u32 create_worker_window(void *parameter) {
    (void)parameter;
    handle window = create_window();
    CHECK(window && !ShowWindow(window, 5) && IsWindowVisible(window));
    worker_window = window;
    return 0;
}

static const struct window_class specification = {
    .size = sizeof(struct window_class),
    .proc = on_message,
    .instance = &__ImageBase,
    .class_name = class_name,
};

void entry(void) {
    CHECK(RegisterClassExA(&specification));
    handle window = create_window();
    CHECK(window && window == callback_window && last_phase == WM_CREATE);
    CHECK(!UnregisterClassA(class_name, &__ImageBase) && GetLastError() == 1412u);
    CHECK(!IsWindowVisible(window));
    CHECK(!ShowWindow(window, 5));
    CHECK(IsWindowVisible(window));
    CHECK(ShowWindow(window, 0));
    CHECK(!IsWindowVisible(window));

    struct message msg;
    CHECK(PostMessageA(window, PAYLOAD_MESSAGE, PAYLOAD_WPARAM, PAYLOAD_LPARAM));
    CHECK(!PeekMessageA(&msg, window, OTHER_MESSAGE, OTHER_MESSAGE, PM_REMOVE));
    CHECK(PeekMessageA(&msg, window, PAYLOAD_MESSAGE, PAYLOAD_MESSAGE, 0));
    CHECK(msg.window == window && msg.id == PAYLOAD_MESSAGE);
    CHECK(msg.wparam == PAYLOAD_WPARAM && msg.lparam == PAYLOAD_LPARAM);
    CHECK(PeekMessageA(&msg, window, PAYLOAD_MESSAGE, PAYLOAD_MESSAGE, PM_REMOVE));
    CHECK(msg.window == window && msg.id == PAYLOAD_MESSAGE);
    CHECK(!TranslateMessage(&msg));
    CHECK(DispatchMessageA(&msg) == PAYLOAD_RESULT && delivery_count == 1);
    CHECK(!PeekMessageA(&msg, window, PAYLOAD_MESSAGE, PAYLOAD_MESSAGE, PM_REMOVE));

    CHECK(PostMessageA(window, OTHER_MESSAGE, 0, 0));
    CHECK(PeekMessageA(&msg, 0, OTHER_MESSAGE, OTHER_MESSAGE, 0) && msg.window == window);
    CHECK(DestroyWindow(window) && last_phase == WM_NCDESTROY);
    CHECK(!PeekMessageA(&msg, 0, OTHER_MESSAGE, OTHER_MESSAGE, PM_REMOVE));
    CHECK(!IsWindowVisible(window) && !DestroyWindow(window));
    CHECK(GetLastError() == 1400u);

    window = create_window();
    CHECK(window && PostMessageA(window, WM_CLOSE, 0, 0));
    CHECK(PeekMessageA(&msg, window, WM_CLOSE, WM_CLOSE, PM_REMOVE));
    CHECK(DispatchMessageA(&msg) == 0 && last_phase == WM_NCDESTROY);
    CHECK(!DestroyWindow(window));

    reject_phase = WM_NCCREATE;
    CHECK(!create_window() && callback_window);
    CHECK(last_phase != WM_CREATE && !DestroyWindow(callback_window));

    reject_phase = WM_CREATE;
    CHECK(!create_window() && callback_window);
    CHECK(!DestroyWindow(callback_window));
    reject_phase = DESTROY_ON_CREATE;
    CHECK(!create_window() && callback_window && last_phase == WM_NCDESTROY);
    CHECK(!DestroyWindow(callback_window));

    reject_phase = 0;
    handle worker = CreateThread(0, 0, create_worker_window, 0, 0, 0);
    CHECK(worker && WaitForSingleObject(worker, 0xffffffffu) == 0);
    CHECK(CloseHandle(worker));
    CHECK(worker_window && !IsWindowVisible(worker_window));
    CHECK(UnregisterClassA(class_name, &__ImageBase));
    CHECK(!create_window() && !callback_window);
    ExitProcess(0);
}
