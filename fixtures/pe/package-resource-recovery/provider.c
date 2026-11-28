// SPDX-License-Identifier: GPL-3.0-or-later

typedef unsigned int u32;
typedef void *Handle;

__declspec(dllimport) Handle CreateEventA(void *attributes, int manual_reset,
                                          int initial_state, const char *name);

static Handle event;
static u32 acquisitions;

int DllMain(void *instance, u32 reason, void *reserved) {
    (void)instance;
    (void)reserved;
    if (reason == 1) {
        if (event != 0 || acquisitions != 0)
            return 0;
        event = CreateEventA(0, 1, 0, 0);
        return event != 0;
    }
    if (reason == 0) {
        // The host records this marker as the callback's actual return value.
        // No handle is used here: Xenora has already cleared the handle table.
        return event != 0 && acquisitions == 1 ? 77 : 0;
    }
    return 1;
}

__declspec(dllexport) Handle AcquireFixtureEvent(void) {
    if (event == 0 || acquisitions != 0)
        return 0;
    ++acquisitions;
    return event;
}
