// SPDX-License-Identifier: GPL-3.0-or-later

typedef unsigned long u32;
typedef void (*signal_handler)(int);
struct exception_pointers;

#define IMPORT __declspec(dllimport)
#define CHECK(condition, code) do { if (!(condition)) ExitProcess(code); } while (0)
#define SIG_DFL ((signal_handler)0)
#define SIG_IGN ((signal_handler)1)

IMPORT __declspec(noreturn) void ExitProcess(unsigned int status);
IMPORT void RaiseException(u32 code, u32 flags, u32 count, const void *arguments);
IMPORT int _seh_filter_exe(u32 code, struct exception_pointers *exception);
IMPORT signal_handler signal(int number, signal_handler handler);

enum { SIGILL = 4, SIGFPE = 8, SIGSEGV = 11, CAUGHT = 0, RESUMED = 1 };
static const u32 illegal_instruction = 0xc000001d;
static const u32 access_violation = 0xc0000005;
static const u32 privileged_instruction = 0xc0000096;
static const u32 float_divide_by_zero = 0xc000008e;
static const u32 unrecognized = 0xe0005eaf;

static volatile unsigned int ill_calls;
static volatile unsigned int segv_calls;

static void on_illegal_instruction(int number) {
    CHECK(number == SIGILL, 10);
    // Reenter the CRT. The one-shot handler must have reset before being called.
    CHECK(signal(number, SIG_DFL) == SIG_DFL, 11);
    ++ill_calls;
    // Void signal handlers have no return value for the SEH filter to use.
    __asm__ volatile("movl $24, %%eax" : : : "rax");
}

static void on_access_violation(int number) {
    CHECK(number == SIGSEGV, 12);
    CHECK(signal(number, SIG_DFL) == SIG_DFL, 13);
    ++segv_calls;
    __asm__ volatile("movl $24, %%eax" : : : "rax");
}

static __declspec(noinline) int try_filter(u32 code) {
    __try {
        // The raise is continuable. On resume it returns to this function.
        RaiseException(code, 0, 0, 0);
        return RESUMED;
    } __except (_seh_filter_exe(_exception_code(), _exception_info())) {
        // The CRT filter may decline or resume, but must never enter this body.
        ExitProcess(90);
    }
}

static int dispatch(u32 code) {
    __try {
        return try_filter(code);
    } __except (_exception_code() == code ? 1 : 0) {
        return CAUGHT;
    }
}

void entry(void) {
#ifdef FPE_REFUSAL
    CHECK(signal(SIGFPE, on_illegal_instruction) == SIG_DFL, 61);
    dispatch(float_divide_by_zero);
    ExitProcess(62);
#endif

    CHECK(dispatch(illegal_instruction) == CAUGHT, 20);
    CHECK(dispatch(access_violation) == CAUGHT, 21);
    CHECK(dispatch(float_divide_by_zero) == CAUGHT, 26);

    CHECK(signal(SIGSEGV, SIG_IGN) == SIG_DFL, 22);
    CHECK(dispatch(access_violation) == RESUMED, 23);
    CHECK(dispatch(access_violation) == RESUMED, 24);
    CHECK(ill_calls == 0 && segv_calls == 0, 25);

    CHECK(signal(SIGILL, on_illegal_instruction) == SIG_DFL, 30);
    CHECK(signal(SIGSEGV, on_access_violation) == SIG_IGN, 31);
    CHECK(dispatch(unrecognized) == CAUGHT, 32);
    CHECK(ill_calls == 0 && segv_calls == 0, 33);

    CHECK(dispatch(illegal_instruction) == RESUMED, 40);
    CHECK(ill_calls == 1 && segv_calls == 0, 41);
    CHECK(dispatch(illegal_instruction) == CAUGHT, 42);
    CHECK(ill_calls == 1 && segv_calls == 0, 43);

    CHECK(dispatch(access_violation) == RESUMED, 50);
    CHECK(ill_calls == 1 && segv_calls == 1, 51);
    CHECK(dispatch(access_violation) == CAUGHT, 52);
    CHECK(ill_calls == 1 && segv_calls == 1, 53);
    CHECK(signal(SIGILL, on_illegal_instruction) == SIG_DFL, 54);
    CHECK(dispatch(privileged_instruction) == RESUMED, 55);
    CHECK(ill_calls == 2 && segv_calls == 1, 56);
    ExitProcess(0);
}
