/* SPDX-License-Identifier: GPL-3.0-or-later
 *
 * Original guest program for #453: MSVC-style C structured exception
 * handling (`__try`, `__except`, `__finally`) around CPU faults and raised
 * exceptions. tests/compat_fixtures/build-guest-c-seh.sh builds one
 * executable per case with clang -fasync-exceptions, choosing the case with
 * -DCASE=<name>. The program is freestanding: it has no C runtime, so its only
 * imports are the four services named below.
 *
 * Every case reports what ran by calling xenora_note, a service the test
 * registers, and exits with its own status. The expected notes and status of
 * each case are stated next to it and follow from the language rules of
 * `try-except` and `try-finally` and the documented exception codes:
 * STATUS_ACCESS_VIOLATION 0xC0000005, STATUS_ILLEGAL_INSTRUCTION 0xC000001D,
 * STATUS_INTEGER_DIVIDE_BY_ZERO 0xC0000094.
 */

typedef unsigned long u32; /* 32 bits under the Windows x64 data model */
typedef unsigned long long u64;

__declspec(dllimport) void ExitProcess(unsigned int status);
__declspec(dllimport) void RaiseException(u32 code, u32 flags, u32 count, const u64 *arguments);
__declspec(dllimport) void xenora_note(u64 value);

/* The documented EXCEPTION_RECORD and EXCEPTION_POINTERS fields. */
struct record {
    u32 code;
    u32 flags;
    struct record *chained;
    void *address;
    u32 count;
    u64 information[15];
};
struct pointers {
    struct record *record;
    void *context;
};

static volatile int zero;
static volatile int sink;

/* Notes the record's code, parameter count, first two parameters and
 * address, then accepts the exception. */
static int report(struct pointers *exception) {
    struct record *r = exception->record;
    xenora_note(r->code);
    xenora_note(r->count);
    xenora_note(r->information[0]);
    xenora_note(r->information[1]);
    xenora_note((u64)r->address);
    return 1;
}

/* Notes 0xC0000094; exits 42. */
static int divide(void) {
    int result = 0;
    __try {
        result = 100 / zero;
        xenora_note(99);
    } __except (xenora_note(_exception_code()), _exception_code() == 0xC0000094 ? 1 : 0) {
        result = 42;
    }
    return result;
}

/* The inner filter declines, so the search reaches the caller's filter.
 * Notes 2, 3, 4; exits 43. */
static __declspec(noinline) int search_inner(void) {
    int result = 0;
    __try {
        result = 100 / zero;
    } __except (xenora_note(2), 0) {
        xenora_note(99);
    }
    return result;
}
static int search(void) {
    __try {
        search_inner();
        xenora_note(98);
    } __except (xenora_note(3), _exception_code() == 0xC0000094) {
        xenora_note(4);
        return 43;
    }
    return 0;
}

/* No filter accepts the fault. Notes 5 and never exits by itself. */
static int unhandled(void) {
    __try {
        sink = 100 / zero;
    } __except (xenora_note(5), 0) {
        xenora_note(99);
    }
    xenora_note(98);
    return 1;
}

/* A write to an unmapped address. Notes 0xC0000005, 2, 1, 0x1230 and the
 * faulting instruction's address; exits 44. */
static int write_fault(void) {
    __try {
        *(volatile int *)0x1230 = 7;
        xenora_note(99);
    } __except (report(_exception_info())) {
        return 44;
    }
    return 0;
}

/* A read of an unmapped address. Notes 0xC0000005, 2, 0, 0x1238 and the
 * faulting instruction's address; exits 45. */
static int read_fault(void) {
    __try {
        sink = *(volatile int *)0x1238;
        xenora_note(99);
    } __except (report(_exception_info())) {
        return 45;
    }
    return 0;
}

/* A call to an unmapped address. Notes 0xC0000005, 2, 8, 0x1240 and 0x1240;
 * exits 46. */
static int execute_fault(void) {
    __try {
        ((void (*)(void))(u64)0x1240)();
        xenora_note(99);
    } __except (report(_exception_info())) {
        return 46;
    }
    return 0;
}

/* UD2 under a constant filter; the handler reads the code. Notes 0xC000001D;
 * exits 47. */
static int illegal(void) {
    __try {
        __asm__ volatile("ud2");
        xenora_note(99);
    } __except (1) {
        xenora_note(_exception_code());
        return 47;
    }
    return 0;
}

/* A raised exception leaves a `__try`/`__finally` one frame below the
 * handler: the filter runs first, then the termination handler, then the
 * handler body. Notes 10, 13, 11, 14; exits 48. */
static __declspec(noinline) void finally_inner(void) {
    __try {
        xenora_note(10);
        RaiseException(0xE0005EA1, 0, 0, 0);
        xenora_note(99);
    } __finally {
        xenora_note(_abnormal_termination() ? 11 : 12);
    }
}
static int finally_across_frames(void) {
    __try {
        finally_inner();
        xenora_note(98);
    } __except (xenora_note(13), _exception_code() == 0xE0005EA1) {
        xenora_note(14);
        return 48;
    }
    return 0;
}

/* A `__finally` left normally, then one left by a fault inside the frame
 * whose `__except` accepts it. Notes 30, 32, 20, 23, 21, 24; exits 49. */
static int finally_same_frame(void) {
    __try {
        xenora_note(30);
    } __finally {
        xenora_note(_abnormal_termination() ? 31 : 32);
    }
    __try {
        __try {
            xenora_note(20);
            sink = 100 / zero;
            xenora_note(99);
        } __finally {
            xenora_note(_abnormal_termination() ? 21 : 22);
        }
    } __except (xenora_note(23), 1) {
        xenora_note(24);
        return 49;
    }
    return 0;
}

/* The filter continues execution after a continuable raise, so the raise
 * returns. Notes 41, 40; exits 50. */
static int continue_execution(void) {
    __try {
        RaiseException(0xE0005EA2, 0, 0, 0);
        xenora_note(40);
    } __except (xenora_note(41), -1) {
        xenora_note(99);
    }
    return 50;
}

void entry(void) {
    ExitProcess(CASE());
}
