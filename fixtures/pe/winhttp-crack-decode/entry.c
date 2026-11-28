// SPDX-License-Identifier: GPL-3.0-or-later

// Original AMD64 guest. A failed check exits with its source line.
typedef unsigned int u32;
typedef unsigned short u16;
typedef unsigned char byte;

struct url_components {
    u32 size;
    u16 *scheme;
    u32 scheme_length, scheme_id;
    u16 *host;
    u32 host_length;
    u16 port;
    u16 *user;
    u32 user_length;
    u16 *password;
    u32 password_length;
    u16 *path;
    u32 path_length;
    u16 *extra;
    u32 extra_length;
};

_Static_assert(sizeof(struct url_components) == 104, "AMD64 URL_COMPONENTS");
_Static_assert(__builtin_offsetof(struct url_components, path) == 72, "path offset");
_Static_assert(__builtin_offsetof(struct url_components, extra) == 88, "extra offset");

#define IMPORT __declspec(dllimport)
IMPORT int WinHttpCrackUrl(const u16 *, u32, u32, struct url_components *);
IMPORT u32 GetLastError(void);
IMPORT __declspec(noreturn) void ExitProcess(u32);

#define CHECK(condition) do { if (!(condition)) ExitProcess(__LINE__); } while (0)
#define UNITS(array) ((u32)(sizeof(array) / sizeof((array)[0])))
#define ICU_DECODE 0x10000000u
#define ERROR_INSUFFICIENT_BUFFER 122u
#define ERROR_NOT_SUPPORTED 50u
#define SENTINEL 0x5a5au

static const u16 url[] = L"https://example.test/a%20b/%41/%C3%A9?word=%42";
static const u16 raw_path[] = L"/a%20b/%41/%C3%A9";
static const u16 raw_extra[] = L"?word=%42";
static const u16 decoded_path[] = L"/a b/A/\u00c3\u00a9";
static const u16 decoded_extra[] = L"?word=B";

static struct url_components parts;
static u16 path[48], extra[48];

static void reset(u32 path_size, u32 extra_size) {
    for (u32 i = 0; i < UNITS(path); ++i)
        path[i] = SENTINEL;
    for (u32 i = 0; i < UNITS(extra); ++i)
        extra[i] = SENTINEL;
    parts.size = sizeof(parts);
    parts.path = path;
    parts.path_length = path_size;
    parts.extra = extra;
    parts.extra_length = extra_size;
}

static void require_contents(const u16 *actual, u32 capacity, const u16 *wanted, u32 used) {
    for (u32 i = 0; i < used; ++i)
        CHECK(actual[i] == wanted[i]);
    for (u32 i = used; i < capacity; ++i)
        CHECK(actual[i] == SENTINEL);
}

void entry(void) {
    reset(UNITS(decoded_path), UNITS(decoded_extra));
    // Distinguish an unimplemented flag from a decoded payload mismatch.
    if (!WinHttpCrackUrl(url, 0, ICU_DECODE, &parts))
        ExitProcess(GetLastError() == ERROR_NOT_SUPPORTED ? 200 : 201);
    CHECK(parts.path_length == UNITS(decoded_path) - 1);
    CHECK(parts.extra_length == UNITS(decoded_extra) - 1);
    require_contents(path, UNITS(path), decoded_path, UNITS(decoded_path));
    require_contents(extra, UNITS(extra), decoded_extra, UNITS(decoded_extra));

    reset(UNITS(raw_path), UNITS(raw_extra));
    CHECK(WinHttpCrackUrl(url, UNITS(url) - 1, 0, &parts));
    CHECK(parts.path_length == UNITS(raw_path) - 1);
    CHECK(parts.extra_length == UNITS(raw_extra) - 1);
    require_contents(path, UNITS(path), raw_path, UNITS(raw_path));
    require_contents(extra, UNITS(extra), raw_extra, UNITS(raw_extra));

    // Only the late component is undersized, by its decoded terminator.
    reset(UNITS(decoded_path), UNITS(decoded_extra) - 1);
    byte before[sizeof(parts)];
    for (u32 i = 0; i < sizeof(parts); ++i)
        before[i] = ((const byte *)&parts)[i];
    CHECK(!WinHttpCrackUrl(url, 0, ICU_DECODE, &parts));
    CHECK(GetLastError() == ERROR_INSUFFICIENT_BUFFER);
    for (u32 i = 0; i < sizeof(parts); ++i)
        CHECK(((const byte *)&parts)[i] == before[i]);
    require_contents(path, UNITS(path), 0, 0);
    require_contents(extra, UNITS(extra), 0, 0);
    ExitProcess(0);
}
