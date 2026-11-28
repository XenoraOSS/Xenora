// SPDX-License-Identifier: GPL-3.0-or-later

extern "C" __declspec(dllimport) int RelayOuter(void (*callback)(), volatile unsigned int *trace);

static volatile unsigned int trace;

struct Mark {
    unsigned int digit;
    ~Mark() { trace = 10 * trace + digit; }
};

__attribute__((noinline)) static void ReturnToInner() {
    trace = 10 * trace + 5;
}

__attribute__((noinline)) static void RaiseInRoot() {
    Mark marker{1};
    throw 71;
}

int main() {
    if (RelayOuter(ReturnToInner, &trace) != 36 || trace != 345)
        return 10;
    trace = 0;

    try {
        Mark marker{2};
        RelayOuter(RaiseInRoot, &trace);
        return 11;
    } catch (int value) {
        if (value != 71)
            return 12;
        return trace == 3412 ? 0 : 13;
    } catch (...) {
        return 14;
    }
}
