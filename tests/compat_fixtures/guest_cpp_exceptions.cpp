// SPDX-License-Identifier: GPL-3.0-or-later
//
// An original C++ program that throws, catches and unwinds (#447). It is
// linked with -static-libstdc++ -static-libgcc, so GCC's unwinder and C++
// personality run as guest code and call the operating system's exception
// services: RaiseException, RtlUnwindEx, RtlVirtualUnwind,
// RtlLookupFunctionEntry and RtlCaptureContext.
//
// It prints one line per check and returns the number of checks that failed.
// Run with the argument "uncaught", it throws an exception nobody catches.

#include <cstdio>
#include <cstring>
#include <stdexcept>

static int destroyed;

struct Guard {
    ~Guard() { ++destroyed; }
};

struct Code {
    int value;
};

static int failures;

static void check(const char *name, int got, int want) {
    std::printf("guest cpp exceptions: %s %d\n", name, got);
    if (got != want) {
        ++failures;
    }
}

// Each level owns a Guard, so unwinding through five frames runs five
// destructors, plus the one in the frame that throws.
__attribute__((noinline)) static int deep(int depth) {
    Guard guard;
    if (depth == 0) {
        throw std::runtime_error("deep");
    }
    return deep(depth - 1) + 1;
}

__attribute__((noinline)) static void rethrow() {
    try {
        throw std::logic_error("again");
    } catch (const std::logic_error &) {
        throw;
    }
}

__attribute__((noinline)) static void throw_code(int value) {
    throw Code{value};
}

// A large stack allocation, and floating-point values that live in
// callee-saved XMM registers across a call that throws.
__attribute__((noinline)) static double thrower(volatile double *scale) {
    volatile char big[8192];
    big[0] = 1;
    big[sizeof big - 1] = 2;
    double a = *scale * 3.0;
    double b = *scale * 5.0;
    throw_code(static_cast<int>(a + b + big[0] + big[sizeof big - 1]));
    return a * b;
}

__attribute__((noinline)) static int keeps_registers(volatile double *scale) {
    double kept = *scale * 7.0;
    double other = *scale * 11.0;
    int caught = 0;
    try {
        thrower(scale);
    } catch (const Code &code) {
        caught = code.value;
    }
    // kept and other were held across the throwing call.
    return caught * 1000 + static_cast<int>(kept + other);
}

int main(int argc, char **argv) {
    if (argc > 1 && std::strcmp(argv[1], "uncaught") == 0) {
        std::printf("guest cpp exceptions: throwing\n");
        throw std::runtime_error("nobody catches this");
    }

    int value = 0;
    try {
        throw 7;
    } catch (int thrown) {
        value = thrown;
    }
    check("int", value, 7);

    int matched = 0;
    try {
        deep(5);
    } catch (const std::exception &error) {
        matched = std::strcmp(error.what(), "deep") == 0;
    }
    check("deep", matched, 1);
    check("destroyed", destroyed, 6);

    int rethrown = 0;
    try {
        rethrow();
    } catch (const std::logic_error &error) {
        rethrown = std::strcmp(error.what(), "again") == 0;
    }
    check("rethrow", rethrown, 1);

    int any = 0;
    try {
        throw_code(3);
    } catch (...) {
        any = 1;
    }
    check("any", any, 1);

    volatile double scale = 2.0;
    // thrower throws (2*3 + 2*5 + 1 + 2) = 19; kept + other = 14 + 22 = 36.
    check("registers", keeps_registers(&scale), 19036);

    int nested = 0;
    try {
        try {
            throw 1;
        } catch (int inner) {
            nested = inner;
            throw Code{inner + 1};
        }
    } catch (const Code &outer) {
        nested += outer.value * 10;
    }
    check("nested", nested, 21);

    std::printf("guest cpp exceptions: failures %d\n", failures);
    return failures;
}
