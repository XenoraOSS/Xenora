// SPDX-License-Identifier: GPL-3.0-or-later

#include <cstdio>
#include <windows.h>

static int attempts;
static int completions;

struct Value {
    int number;
    Value();
};

[[gnu::noinline]] Value::Value() {
    if (++attempts == 1)
        throw 73;
    number = 41;
    ++completions;
}

[[gnu::noinline]] static const Value &local_value() {
    static const Value value;
    return value;
}

static const Value *worker_value;

static DWORD WINAPI retry_on_worker(void *) {
    worker_value = &local_value();
    return worker_value->number == 41 && attempts == 2 && completions == 1 ? 17 : 18;
}

int main() {
    try {
        (void)local_value();
        return 1;
    } catch (int code) {
        if (code != 73 || attempts != 1 || completions != 0)
            return 2;
        std::printf("caught %d after %d attempt\n", code, attempts);
    }

    HANDLE worker = CreateThread(nullptr, 0, retry_on_worker, nullptr, 0, nullptr);
    if (!worker)
        return 3;
    DWORD status = 0;
    const bool joined = WaitForSingleObject(worker, INFINITE) == WAIT_OBJECT_0;
    const bool got_status = joined && GetExitCodeThread(worker, &status);
    const bool closed = CloseHandle(worker);
    if (!got_status || !closed || status != 17)
        return 4;
    std::printf("worker returned %lu after %d attempts\n", status, attempts);

    const auto &value = local_value();
    if (&value != worker_value || value.number != 41 || attempts != 2 || completions != 1)
        return 5;
    std::printf("main reused %d after %d attempts and %d completion\n",
                value.number, attempts, completions);
    return 0;
}
