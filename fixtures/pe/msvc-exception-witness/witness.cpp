// SPDX-License-Identifier: GPL-3.0-or-later
// Original C++ source for the Microsoft x64 exception boundary.
extern "C" __declspec(dllimport) __declspec(noreturn) void ExitProcess(unsigned);

#pragma section(".gate", read, write)
__declspec(allocate(".gate")) static volatile unsigned destruction_order;

struct Scope {
    unsigned digit;
    __declspec(noinline) ~Scope() {
        destruction_order = destruction_order * 10 + digit;
    }
};

#if defined(RETHROW_WITNESS)
static void return_normally() {}
// An indirect potentially throwing call keeps the real catch and cleanup
// code in the linked witness without a dependency on runtime type data.
static void (*volatile next)() = return_normally;
#endif

__declspec(noinline) static void inner() {
    Scope first{1};
#if defined(THROW_WITNESS)
    throw 73;
#elif defined(RETHROW_WITNESS)
    next();
#endif
}

__declspec(noinline) static void outer() {
    Scope second{2};
    inner();
}

extern "C" void entry() {
#ifdef THROW_WITNESS
    try {
        outer();
        ExitProcess(3);
    } catch (int code) {
        if (code != 73)
            ExitProcess(2);
    }
#elif defined(RETHROW_WITNESS)
    try {
        outer();
    } catch (...) {
        throw;
    }
#else
    outer();
#endif
    ExitProcess(destruction_order == 12 ? 0 : 1);
}
