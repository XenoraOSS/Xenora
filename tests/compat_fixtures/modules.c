// SPDX-License-Identifier: GPL-3.0-or-later
//
// An original C program with no C library linked that asks which module it is,
// where its image lies and how long other modules stay. It compares the answers
// with what it can see itself: the linker's __ImageBase, its own entry point and
// the SizeOfImage in its mapped header.
//
// Build with tests/compat_fixtures/build-modules.sh. The test that runs it is
// crates/loader/tests/modules.rs.

typedef unsigned long long u64;
typedef unsigned int u32;
typedef unsigned short u16;
typedef void *HANDLE;

#define IMPORT __declspec(dllimport)
IMPORT HANDLE GetModuleHandleA(const char *name);
IMPORT HANDLE GetModuleHandleW(const u16 *name);
IMPORT int GetModuleHandleExA(u32 flags, const void *name, HANDLE *module);
IMPORT int GetModuleHandleExW(u32 flags, const void *name, HANDLE *module);
IMPORT HANDLE LoadLibraryA(const char *name);
IMPORT int FreeLibrary(HANDLE module);
IMPORT void *GetProcAddress(HANDLE module, const char *name);
IMPORT int K32GetModuleInformation(HANDLE process, HANDLE module, void *info, u32 size);
IMPORT HANDLE GetCurrentProcess(void);
IMPORT u32 GetLastError(void);
IMPORT void SetLastError(u32 error);

typedef struct {
    void *base;
    u32 size;
    void *entry;
} MODULE_INFO;

extern const unsigned char __ImageBase[];
void start(void);

#define STATUS 0x4D0Du
#define PIN 1
#define UNCHANGED 2
#define FROM_ADDRESS 4
#define NOT_FOUND 126
#define BAD_PARAMETER 87
#define BAD_HANDLE 6

static u32 checks;
static void (*finish)(u32);

static void check(int holds) {
    checks++;
#ifdef MODULES_BREAK
    if (checks == 5) holds = 0; // control
#endif
    if (!holds) {
        finish(checks);
        for (;;) {}
    }
}

static int fails(int result, u32 error) {
    return !result && GetLastError() == error;
}

static int static_data = 1;

void start(void) {
    HANDLE kernel = GetModuleHandleA("kernel32.dll");
    // Not imported: found by name, so the catalog must hold it.
    finish = (void (*)(u32))GetProcAddress(kernel, "ExitProcess");
    if (!finish) {
        *(volatile int *)0 = 1;
    }

    // The program itself.
    HANDLE self = (HANDLE)__ImageBase, found;
    check(GetModuleHandleA(0) == self);
    check(GetModuleHandleW(0) == self);
    check(GetModuleHandleExA(0, 0, &found) && found == self);
    check(GetModuleHandleExW(UNCHANGED, 0, &found) && found == self);
    check(FreeLibrary(self));
    check(fails(FreeLibrary(self), BAD_HANDLE)); // only one was counted
    check(GetModuleHandleA(0) == self);          // and it stays loaded

    // By an address inside it, and outside.
    check(GetModuleHandleExA(FROM_ADDRESS | UNCHANGED, (void *)start, &found) && found == self);
    check(GetModuleHandleExW(FROM_ADDRESS | UNCHANGED, &static_data, &found) && found == self);
    check(GetModuleHandleExA(FROM_ADDRESS | UNCHANGED, __ImageBase, &found) && found == self);
    int local;
    found = self;
    check(fails(GetModuleHandleExA(FROM_ADDRESS, &local, &found), NOT_FOUND) && !found);
    check(fails(GetModuleHandleExA(FROM_ADDRESS, 0, &found), BAD_PARAMETER));

    // Where it lies. SizeOfImage is at offset 56 of the optional header.
    const u32 header = *(const u32 *)(__ImageBase + 0x3C);
    const u32 size = *(const u32 *)(__ImageBase + header + 24 + 56);
    MODULE_INFO info = {0, 0, 0};
    check(K32GetModuleInformation(GetCurrentProcess(), self, &info, sizeof info));
    check(sizeof info == 24 && info.base == self && info.size == size);
    check(info.entry == (void *)start);
    check(fails(K32GetModuleInformation(GetCurrentProcess(), self, &info, 23), 122));
    check(fails(K32GetModuleInformation(GetCurrentProcess(), self, 0, sizeof info), 998));
    check(GetModuleHandleExA(FROM_ADDRESS | UNCHANGED, (void *)((u64)__ImageBase + size - 1), &found));
    check(fails(GetModuleHandleExA(FROM_ADDRESS, (void *)((u64)__ImageBase + size), &found), NOT_FOUND));
    check(!GetModuleHandleExA(FROM_ADDRESS, (void *)((u64)__ImageBase - 1), &found));

    // Names: without a dot .dll is appended, and case does not matter.
    static const u16 mixed[] = u"KeRnEl32";
    check(GetModuleHandleW(mixed) == kernel && kernel != self);
    check(fails(GetModuleHandleA("kernel32.") != 0, NOT_FOUND));
    check(fails(GetModuleHandleA("no-such-module") != 0, NOT_FOUND));
    check(fails(LoadLibraryA(0) != 0, NOT_FOUND));
    HANDLE base = GetModuleHandleA("kernelbase");
    check(base && base != kernel);
    check(GetProcAddress(base, "GetModuleHandleA") == GetProcAddress(kernel, "GetModuleHandleA"));
    check(fails(GetProcAddress(base, "ExitProcess") != 0, 127));
    check(fails(GetProcAddress(self, "start") != 0, 127));

    // Flags that exclude each other, unknown ones, and no place for the handle.
    found = self;
    check(fails(GetModuleHandleExA(PIN | UNCHANGED, "kernel32", &found), BAD_PARAMETER) && !found);
    check(fails(GetModuleHandleExA(8, "kernel32", &found), BAD_PARAMETER));
    check(fails(GetModuleHandleExA(0, "kernel32", 0), BAD_PARAMETER));

    // The C runtime starts with one reference, the program's own.
    HANDLE crt = GetModuleHandleA("ucrtbase");
    check(crt != 0);
    for (int i = 0; i < 3; i++)
        check(GetModuleHandleExA(UNCHANGED, "ucrtbase", &found) && found == crt);
    check(GetModuleHandleExA(0, "ucrtbase", &found) && found == crt);
    check(LoadLibraryA("ucrtbase") == crt);
    check(FreeLibrary(crt) && FreeLibrary(crt));
    check(GetModuleHandleA("ucrtbase") == crt);
#ifdef MODULES_PIN
    check(GetModuleHandleExA(PIN, "ucrtbase", &found) && found == crt);
    check(FreeLibrary(crt));
    check(fails(FreeLibrary(crt), BAD_HANDLE));
    check(GetModuleHandleA("ucrtbase") == crt);
    check(GetProcAddress(crt, "strlen") != 0);
#else
    check(FreeLibrary(crt));
    check(fails(GetModuleHandleA("ucrtbase") != 0, NOT_FOUND));
    check(fails(GetModuleHandleExA(0, "ucrtbase", &found), NOT_FOUND) && !found);
    check(fails(GetProcAddress(crt, "strlen") != 0, BAD_HANDLE));
    check(LoadLibraryA("ucrtbase") == crt);
#endif
    finish(STATUS);
}
