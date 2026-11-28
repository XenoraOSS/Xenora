/* SPDX-License-Identifier: GPL-3.0-or-later */

typedef unsigned int dword;
typedef unsigned long long ulong_ptr;

struct exception_record {
    dword code;
    dword flags;
    struct exception_record *previous;
    void *address;
    dword count;
    ulong_ptr information[15];
};

struct exception_pointers {
    struct exception_record *record;
    void *context;
};

void *_exception_info(void);
__declspec(dllimport) void RaiseException(dword, dword, dword, const ulong_ptr *);
__declspec(dllimport) __declspec(noreturn) void ExitProcess(dword);

#define EXCEPTION_CODE 0xe1541339u
#define FIRST_ARGUMENT 0x0123456789abcdefull
#define SECOND_ARGUMENT 0xfedcba9876543210ull

static volatile dword checked;

static int inspect(struct exception_pointers *pointers) {
    const struct exception_record *record = pointers->record;
    if (record->code != EXCEPTION_CODE || record->flags != 0)
        checked = 11;
#if TEST_CASE == 2
    else if (record->count != 2 ||
             record->information[0] != FIRST_ARGUMENT ||
             record->information[1] != SECOND_ARGUMENT)
        checked = 12;
#else
    else if (record->count != 0)
        checked = 13;
#endif
    else
        checked = 1;
    return 1;
}

void entry(void) {
    __try {
#if TEST_CASE == 2
        const ulong_ptr arguments[] = {FIRST_ARGUMENT, SECOND_ARGUMENT};
        RaiseException(EXCEPTION_CODE, 0, 2, arguments);
#elif TEST_CASE == 1
        RaiseException(EXCEPTION_CODE, 0, 0xffffffffu, 0);
#else
        RaiseException(EXCEPTION_CODE, 0, 3, 0);
#endif
        ExitProcess(14);
    } __except (inspect(_exception_info())) {
        ExitProcess(checked == 1 ? 0 : checked == 0 ? 15 : checked);
    }
    ExitProcess(16);
}
