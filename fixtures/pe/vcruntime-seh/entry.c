// SPDX-License-Identifier: GPL-3.0-or-later

// Original AMD64 guest. Clang emits the handler RVA and scope tables.
typedef unsigned int u32;
typedef unsigned long long u64;

#define IMPORT __declspec(dllimport)
#define NOINLINE __declspec(noinline)
#define FAILURE 99u
#define EXCEPTION_CODE 0xe0421438u

IMPORT void RaiseException(u32, u32, u32, const u64 *);
IMPORT __declspec(noreturn) void ExitProcess(u32);

#pragma section(".gate", read, write)
__declspec(allocate(".gate"))
static volatile u32 gate[10] = {0x53454838u, 0};

static void note(u32 event) {
    u32 count = gate[1];
    if (count >= 8)
        ExitProcess(FAILURE);
    gate[2 + count] = event;
    gate[1] = count + 1;
}

static NOINLINE int filter(u32 code, u32 event, int accept) {
    if (code != EXCEPTION_CODE)
        ExitProcess(FAILURE);
    note(event);
    return accept;
}

static NOINLINE void leaf(void) {
    __try {
        note(2);
        RaiseException(EXCEPTION_CODE, 0, 0, 0);
        ExitProcess(FAILURE);
    } __finally {
        note(_abnormal_termination() ? 5 : FAILURE);
    }
}

static NOINLINE void middle(void) {
    __try {
        leaf();
        ExitProcess(FAILURE);
    } __except (filter(_exception_code(), 3, 0)) {
        ExitProcess(FAILURE);
    }
}

void entry(void) {
    note(1);
    __try {
        middle();
        ExitProcess(FAILURE);
    } __except (filter(_exception_code(), 4, 1)) {
        note(6);
    }
    note(7);
    ExitProcess(0);
}
