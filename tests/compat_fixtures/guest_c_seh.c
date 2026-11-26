/* SPDX-License-Identifier: GPL-3.0-or-later
 *
 * Original guest program for #453: MSVC-style C structured exception
 * handling (`__try`, `__except`, `__finally`) around CPU faults and raised
 * exceptions. tests/compat_fixtures/build-guest-c-seh.sh builds one
 * executable per case with clang -fasync-exceptions, choosing the case with
 * -DCASE=<name>. The program is freestanding: it has no C runtime, so its only
 * imports are the services named below. #476 added the cases that use
 * the unhandled-exception filter and change the exception context.
 *
 * Every case reports what ran by calling xenora_note, a service the test
 * registers, and exits with its own status. The expected notes and status of
 * each case are stated next to it and follow from the language rules of
 * `try-except` and `try-finally` and the documented exception codes:
 * STATUS_ACCESS_VIOLATION 0xC0000005, STATUS_ILLEGAL_INSTRUCTION 0xC000001D,
 * STATUS_INTEGER_DIVIDE_BY_ZERO 0xC0000094, the documented
 * SetUnhandledExceptionFilter results and Xenora's documented default action
 * for an exception nothing handles: the process ends with the exception code
 * as its exit status.
 */

typedef unsigned long u32; /* 32 bits under the Windows x64 data model */
typedef unsigned long long u64;

__declspec(dllimport) void ExitProcess(unsigned int status);
__declspec(dllimport) void RaiseException(u32 code, u32 flags, u32 count, const u64 *arguments);
__declspec(dllimport) void DebugBreak(void);
__declspec(dllimport) void xenora_note(u64 value);

struct pointers;
typedef long (*top_filter)(struct pointers *exception);
__declspec(dllimport) top_filter SetUnhandledExceptionFilter(top_filter filter);

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
    u64 *context;
};

/* Fields of the x64 CONTEXT record, as indexes of 8-byte words: Rcx at byte
 * offset 0x80, Rsp at 0x98 and Rip at 0xF8. */
enum { CONTEXT_RCX = 0x80 / 8, CONTEXT_RSP = 0x98 / 8, CONTEXT_RIP = 0xF8 / 8 };

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

/* No filter accepts the fault and no unhandled-exception filter is set, so
 * the default action ends the process with 0xC0000094. Notes 5. */
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

/* The unhandled-exception filter continues execution after a continuable
 * raise, so the raise returns. Notes 0xE0005EA3, 61; exits 51. */
static long note_and_continue(struct pointers *exception) {
    xenora_note(exception->record->code);
    return -1;
}
static int filter_continue(void) {
    SetUnhandledExceptionFilter(note_and_continue);
    RaiseException(0xE0005EA3, 0, 0, 0);
    xenora_note(61);
    return 51;
}

/* A filter can continue past DebugBreak. With the filter removed, the
 * second breakpoint ends the process with STATUS_BREAKPOINT. */
static int debug_break(void) {
    SetUnhandledExceptionFilter(note_and_continue);
    DebugBreak();
    xenora_note(67);
    SetUnhandledExceptionFilter(0);
    DebugBreak();
    xenora_note(99);
    return 1;
}

/* The unhandled-exception filter returns EXCEPTION_EXECUTE_HANDLER for a
 * fault, so the default action ends the process with 0xC0000094.
 * Notes 62, 0xC0000094. */
static long note_and_execute(struct pointers *exception) {
    xenora_note(62);
    xenora_note(exception->record->code);
    return 1;
}
static int filter_execute(void) {
    SetUnhandledExceptionFilter(note_and_execute);
    sink = 100 / zero;
    xenora_note(99);
    return 1;
}

/* The unhandled-exception filter returns EXCEPTION_CONTINUE_SEARCH, so the
 * default action ends the process with 0xE0005EA4. Notes 63. */
static long note_and_search(struct pointers *exception) {
    (void)exception;
    xenora_note(63);
    return 0;
}
static int filter_search(void) {
    SetUnhandledExceptionFilter(note_and_search);
    RaiseException(0xE0005EA4, 0, 0, 0);
    xenora_note(99);
    return 1;
}

/* Where a changed context continues: as if `landing(status)` had been
 * called. Notes 64 and exits with `status`. */
static __declspec(noinline) void landing(u64 status) {
    xenora_note(64);
    ExitProcess((unsigned int)status);
}
/* Continue at `landing(status)` instead of after the raise: RIP, the first
 * argument, and RSP lowered by the return address a call would push. */
static void redirect(u64 *context, u64 status) {
    context[CONTEXT_RIP] = (u64)landing;
    context[CONTEXT_RCX] = status;
    context[CONTEXT_RSP] -= 8;
}

/* The unhandled-exception filter changes the context and continues
 * execution. Notes 64; exits 52. */
static long redirect_52(struct pointers *exception) {
    redirect(exception->context, 52);
    return -1;
}
static int filter_context(void) {
    SetUnhandledExceptionFilter(redirect_52);
    RaiseException(0xE0005EA5, 0, 0, 0);
    xenora_note(99);
    return 1;
}

/* An `__except` filter changes the context and continues execution.
 * Notes 64; exits 53. */
static int redirect_53(struct pointers *exception) {
    redirect(exception->context, 53);
    return -1;
}
static int except_context(void) {
    __try {
        RaiseException(0xE0005EA6, 0, 0, 0);
        xenora_note(99);
    } __except (redirect_53(_exception_info())) {
        xenora_note(98);
    }
    return 1;
}

/* The unhandled-exception filter raises an exception of its own. Xenora does
 * not call the filter again while it runs, so the default action ends the
 * process with the new code, 0xE0005EA7. Notes 66. */
static long raise_again(struct pointers *exception) {
    (void)exception;
    xenora_note(66);
    RaiseException(0xE0005EA7, 0, 0, 0);
    xenora_note(99);
    return -1;
}
static int filter_nested(void) {
    SetUnhandledExceptionFilter(raise_again);
    RaiseException(0xE0005EA8, 0, 0, 0);
    xenora_note(98);
    return 1;
}

void entry(void) {
    ExitProcess(CASE());
}
