// SPDX-License-Identifier: GPL-3.0-or-later
#include "journal.hpp"

using Initializer = void (*)();
using ExitCallback = int (*)();
struct OnExitTable {
    ExitCallback *first;
    ExitCallback *last;
    ExitCallback *end;
};

extern "C" __declspec(dllimport) void _initterm(const Initializer *, const Initializer *);
extern "C" __declspec(dllimport) int _initialize_onexit_table(OnExitTable *);
extern "C" __declspec(dllimport) int _register_onexit_function(OnExitTable *, ExitCallback);
extern "C" __declspec(dllimport) int _execute_onexit_table(OnExitTable *);

// The linker sorts these sentinels around Clang's own .CRT$XCU entries.
// See Microsoft's "CRT initialization" documentation.
#pragma section(".CRT$XCA", read)
#pragma section(".CRT$XCZ", read)
__declspec(allocate(".CRT$XCA")) static const Initializer first_initializer = nullptr;
__declspec(allocate(".CRT$XCZ")) static const Initializer past_initializers = nullptr;

static OnExitTable onexit;
static unsigned int constructed;
static bool failed;

extern "C" int atexit(Initializer callback) {
    // The UCRT table calls the same Win64 no-argument entry. Its executor
    // discards the result, so a compiler-generated void callback can use it.
    const int result = _register_onexit_function(&onexit, reinterpret_cast<ExitCallback>(callback));
    failed |= result != 0;
    return result;
}

struct Global {
    explicit Global(char construct, char destroy) : destroy(destroy) {
        failed |= !append_event(construct);
        ++constructed;
    }

    ~Global() { failed |= !append_event(destroy); }

    char destroy;
};

#ifdef LIFECYCLE_PROVIDER
static Global first('A', 'U');
static Global second('B', 'T');

extern "C" __declspec(dllexport) unsigned int ProviderConstructed() { return constructed; }
#else
static Global first('C', 'S');
static Global second('D', 'R');

extern "C" __declspec(dllimport) unsigned int ProviderConstructed();
extern "C" __declspec(dllexport) unsigned int ConsumerConstructed() { return constructed; }
#endif

extern "C" int DllMain(void *, Dword reason, void *) {
    constexpr Dword process_attach = 1;
    constexpr Dword process_detach = 0;
    if (reason == process_attach) {
#ifndef LIFECYCLE_PROVIDER
        if (ProviderConstructed() != 2) {
            return 0;
        }
#endif
        if (_initialize_onexit_table(&onexit) != 0) {
            return 0;
        }
        _initterm(&first_initializer + 1, &past_initializers);
        return !failed && constructed == 2;
    }
    if (reason == process_detach) {
        return _execute_onexit_table(&onexit) == 0 && !failed;
    }
    return 1;
}
