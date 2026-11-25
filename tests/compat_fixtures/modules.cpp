// SPDX-License-Identifier: GPL-3.0-or-later
//
// An original hosted C++ program that holds module references with a class and
// asks the Windows headers' own functions about its image. It uses the headers'
// MODULEINFO, independent of the one in modules.c, and psapi.dll's export.
//
// Build with tests/compat_fixtures/build-modules.sh. The test that runs it is
// crates/loader/tests/modules.rs.

#define PSAPI_VERSION 1
#include <cstdio>
#include <windows.h>
#include <psapi.h>

static_assert(sizeof(MODULEINFO) == 24);

namespace {

class Reference {
public:
    Reference(DWORD flags, const void *what) {
        if (!GetModuleHandleExW(flags, static_cast<const wchar_t *>(what), &module_)) module_ = nullptr;
    }
    ~Reference() {
        if (module_) FreeLibrary(module_);
    }
    Reference(const Reference &) = delete;
    Reference &operator=(const Reference &) = delete;
    HMODULE get() const { return module_; }

private:
    HMODULE module_ = nullptr;
};

HMODULE before_main = GetModuleHandleW(nullptr);

}  // namespace

extern "C" const unsigned char __ImageBase[];

int main() {
    HMODULE self = reinterpret_cast<HMODULE>(const_cast<unsigned char *>(__ImageBase));
    std::printf("self %d\n", before_main == self && GetModuleHandleA(nullptr) == self);
    {
        Reference by_name(0, nullptr);
        Reference by_address(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, reinterpret_cast<void *>(&main));
        std::printf("held %d\n", by_name.get() == self && by_address.get() == self);
    }
    std::printf("freed %d\n", !FreeLibrary(self) && GetLastError() == ERROR_INVALID_HANDLE);
    MODULEINFO info{};
    BOOL known = GetModuleInformation(GetCurrentProcess(), self, &info, sizeof info);
    auto *dos = reinterpret_cast<const IMAGE_DOS_HEADER *>(__ImageBase);
    auto *nt = reinterpret_cast<const IMAGE_NT_HEADERS64 *>(__ImageBase + dos->e_lfanew);
    std::printf("info %d\n", known && info.lpBaseOfDll == self &&
                                 info.SizeOfImage == nt->OptionalHeader.SizeOfImage &&
                                 reinterpret_cast<char *>(info.EntryPoint) ==
                                     reinterpret_cast<char *>(self) + nt->OptionalHeader.AddressOfEntryPoint);
    return 0;
}
