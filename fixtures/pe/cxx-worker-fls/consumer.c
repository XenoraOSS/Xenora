// SPDX-License-Identifier: GPL-3.0-or-later
#include "records.h"

__declspec(dllimport) U32 GetCurrentThreadId(void);
__declspec(dllimport) void FinishRecord(void *, U32);

__declspec(dllexport) void WorkerCleanup(void *payload) {
    FinishRecord(payload, GetCurrentThreadId());
}
