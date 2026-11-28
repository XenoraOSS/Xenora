// SPDX-License-Identifier: GPL-3.0-or-later

typedef unsigned int u32;
typedef unsigned long long key;
typedef __WCHAR_TYPE__ wide;

#define IMPORT __declspec(dllimport)
#define CHECK(condition, code) do { if (!(condition)) ExitProcess(code); } while (0)
#define CURRENT_USER 0xffffffff80000001ull

IMPORT long RegCreateKeyExW(key parent, const wide *subkey, u32 reserved, wide *class_name,
                           u32 options, u32 access, void *security, key *result, u32 *disposition);
IMPORT long RegOpenKeyW(key parent, const wide *subkey, key *result);
IMPORT long RegOpenKeyExW(key parent, const wide *subkey, u32 options, u32 access, key *result);
IMPORT long RegOpenKeyExA(key parent, const char *subkey, u32 options, u32 access, key *result);
IMPORT long RegSetValueExW(key handle, const wide *name, u32 reserved, u32 type,
                          const void *data, u32 size);
IMPORT long RegQueryValueExA(key handle, const char *name, void *reserved, u32 *type, void *data, u32 *size);
IMPORT long RegQueryValueExW(key handle, const wide *name, void *reserved, u32 *type, void *data, u32 *size);
IMPORT long RegCloseKey(key handle);
IMPORT __declspec(noreturn) void ExitProcess(u32 status);

enum {
    FILE_NOT_FOUND = 2,
    ACCESS_DENIED = 5,
    INVALID_HANDLE = 6,
    NOT_ENOUGH_MEMORY = 8,
    INVALID_PARAMETER = 87,
    MORE_DATA = 234,
    KEY_SET_VALUE = 2,
    KEY_READ = 0x20019,
    REG_DWORD = 4,
    CREATED_NEW_KEY = 1,
    OPENED_EXISTING_KEY = 2,
};

static const wide path[] = L"Software\\XenoraOriginal\\Prefs";

#ifdef COLD_CONTROL

void entry(void) {
    key absent = 0;
    CHECK(RegOpenKeyExW(CURRENT_USER, path, 0, KEY_READ, &absent) == FILE_NOT_FOUND, 60);
    CHECK(RegOpenKeyW(CURRENT_USER, L"Software\\XenoraOriginal", &absent) == FILE_NOT_FOUND, 61);
    ExitProcess(0);
}

#else

static const u32 setting = 0x93ed0215;
static const u32 initial_default = 0x7c0432a1;
static const u32 updated_default = 0x63ad41c2;

void entry(void) {
    key original = 0;
    CHECK(RegOpenKeyExW(CURRENT_USER, path, 0, KEY_READ, &original) == FILE_NOT_FOUND, 10);
    u32 disposition = 0;
    CHECK(RegCreateKeyExW(CURRENT_USER, path, 0, 0, 0, KEY_READ | KEY_SET_VALUE, 0,
                          &original, &disposition) == 0, 11);
    CHECK(original != 0 && original != CURRENT_USER && disposition == CREATED_NEW_KEY, 12);
    key failed = 0x459821bcull;
    disposition = 91;
    CHECK(RegCreateKeyExW(CURRENT_USER, L"NeverCreated", 0, 0, 0, KEY_READ | 0x300,
                          0, &failed, &disposition) == INVALID_PARAMETER, 53);
    CHECK(RegOpenKeyExW(CURRENT_USER, path, 0, KEY_READ | 0x300, &failed) == INVALID_PARAMETER, 54);
    CHECK(failed == 0x459821bcull && disposition == 91, 55);
    CHECK(RegOpenKeyW(CURRENT_USER, L"NeverCreated", &failed) == FILE_NOT_FOUND, 56);

    u32 type = 0;
    u32 size = 0;
    CHECK(RegQueryValueExW(original, L"Missing", 0, &type, 0, &size) == FILE_NOT_FOUND, 13);
    CHECK(RegSetValueExW(original, L"Scene", 0, REG_DWORD, &setting, sizeof(setting)) == 0, 14);
    CHECK(RegSetValueExW(original, 0, 0, REG_DWORD, &initial_default, sizeof(initial_default)) == 0, 15);

    key existing = 0;
    disposition = 0;
    CHECK(RegCreateKeyExW(CURRENT_USER, L"software\\XENORAORIGINAL\\PREFS", 0, 0, 0,
                          KEY_READ, 0, &existing, &disposition) == 0, 16);
    CHECK(existing != 0 && existing != original && disposition == OPENED_EXISTING_KEY, 17);

    key reader = 0;
    key writer = 0;
    CHECK(RegOpenKeyExA(CURRENT_USER, "sOFtWaRe\\xenoraoriginal\\prefs", 0, KEY_READ, &reader) == 0, 18);
    CHECK(RegOpenKeyExW(CURRENT_USER, path, 0, KEY_SET_VALUE, &writer) == 0, 19);
    CHECK(reader != original && writer != original && reader != writer, 20);
    CHECK(RegSetValueExW(reader, L"Scene", 0, REG_DWORD, &updated_default, sizeof(updated_default)) ==
          ACCESS_DENIED, 21);

    type = 0;
    size = 0;
    CHECK(RegQueryValueExA(reader, "sCENE", 0, &type, 0, &size) == 0, 22);
    CHECK(type == REG_DWORD && size == sizeof(setting), 23);

    u32 result = 0x7a5bc639;
    size = sizeof(result) - 1;
    CHECK(RegQueryValueExA(reader, "SCENE", 0, &type, &result, &size) == MORE_DATA, 24);
    CHECK(size == sizeof(setting) && result == 0x7a5bc639, 25);
    size = sizeof(result);
    CHECK(RegQueryValueExW(existing, L"scene", 0, &type, &result, &size) == 0, 26);
    CHECK(type == REG_DWORD && size == sizeof(setting) && result == setting, 27);

    size = sizeof(result);
    CHECK(RegQueryValueExA(reader, "", 0, &type, &result, &size) == 0, 28);
    CHECK(type == REG_DWORD && size == sizeof(initial_default) && result == initial_default, 29);
    CHECK(RegSetValueExW(writer, L"", 0, REG_DWORD, &updated_default, sizeof(updated_default)) == 0, 30);

    key parent = 0;
    key legacy = 0;
    CHECK(RegOpenKeyW(CURRENT_USER, L"Software\\XenoraOriginal", &parent) == 0, 31);
    CHECK(RegOpenKeyW(parent, L"pReFs", &legacy) == 0, 32);
    CHECK(parent != 0 && legacy != 0 && parent != legacy, 33);

    key reopened = 0;
    CHECK(RegOpenKeyW(legacy, 0, &reopened) == 0 && reopened == legacy, 34);
    reopened = 0;
    CHECK(RegOpenKeyW(legacy, L"", &reopened) == 0 && reopened == legacy, 35);
    key separate = 0;
    CHECK(RegOpenKeyExW(legacy, L"", 0, KEY_READ, &separate) == 0, 36);
    CHECK(separate != legacy && separate != 0, 37);

    CHECK(RegCloseKey(legacy) == 0, 38);
    size = sizeof(result);
    CHECK(RegQueryValueExW(legacy, 0, 0, &type, &result, &size) == INVALID_HANDLE, 39);
    CHECK(RegCloseKey(legacy) == INVALID_HANDLE, 40);

    size = sizeof(result);
    CHECK(RegQueryValueExW(separate, 0, 0, &type, &result, &size) == 0, 41);
    CHECK(type == REG_DWORD && size == sizeof(updated_default) && result == updated_default, 42);
    CHECK(RegCloseKey(separate) == 0, 43);
    CHECK(RegCloseKey(parent) == 0, 44);
    CHECK(RegCloseKey(writer) == 0, 45);
    CHECK(RegCloseKey(reader) == 0, 46);
    CHECK(RegCloseKey(existing) == 0, 47);
    CHECK(RegCloseKey(original) == 0, 48);

    // Values persist across close and reopen inside this one guest machine.
    CHECK(RegOpenKeyExA(CURRENT_USER, "software\\xenoraoriginal\\prefs", 0, KEY_READ, &reader) == 0, 49);
    size = sizeof(result);
    CHECK(RegQueryValueExA(reader, "scene", 0, &type, &result, &size) == 0, 50);
    CHECK(type == REG_DWORD && size == sizeof(setting) && result == setting, 51);
    CHECK(RegCloseKey(reader) == 0, 52);

    // Exhausting handles must not publish a key or change the failed
    // call's outputs. Closing a handle makes room for another open.
    key handles[128];
    for (u32 index = 0; index < 128; ++index) {
        CHECK(RegOpenKeyExW(CURRENT_USER, path, 0, KEY_READ, &handles[index]) == 0, 70);
    }
    failed = 0x459821bcull;
    disposition = 91;
    CHECK(RegCreateKeyExW(CURRENT_USER, L"NeverCreated\\Child", 0, 0, 0, KEY_READ,
                          0, &failed, &disposition) == NOT_ENOUGH_MEMORY, 71);
    CHECK(failed == 0x459821bcull && disposition == 91, 72);
    CHECK(RegCloseKey(handles[0]) == 0, 73);
    CHECK(RegOpenKeyW(CURRENT_USER, L"NeverCreated", &failed) == FILE_NOT_FOUND, 74);
    CHECK(RegCreateKeyExW(handles[1], L"", 0, 0, 0, KEY_READ, 0, &failed, &disposition) == 0, 75);
    CHECK(disposition == OPENED_EXISTING_KEY && failed != handles[0] && failed != handles[1], 76);
    CHECK(RegCloseKey(failed) == 0, 77);
    for (u32 index = 1; index < 128; ++index) {
        CHECK(RegCloseKey(handles[index]) == 0, 78);
    }
    ExitProcess(0);
}

#endif
