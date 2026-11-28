// SPDX-License-Identifier: GPL-3.0-or-later
// Original guest exercise of the XAsync provider protocol end to end, from
// the layouts Microsoft Learn documents in XAsync.h and XAsyncProvider.h:
// a manual queue, a custom provider driven through Begin, the scheduled
// DoWork, the completion callback on the completion port, the status and
// the GetResult payload retrieval ending in Cleanup. ExitProcess carries
// the step that failed, or zero when the whole protocol round-trips.

struct XAsyncBlock {
    void* queue;
    void* context;
    void* callback;
    unsigned char internal[32];
};

struct ProviderData {
    XAsyncBlock* async;
    unsigned long long bufferSize;
    void* buffer;
    void* context;
};

extern "C" {
__declspec(dllimport) long XTaskQueueCreate(unsigned int, unsigned int, void**);
__declspec(dllimport) long XTaskQueueDispatch(void*, unsigned int, unsigned int);
__declspec(dllimport) void XTaskQueueCloseHandle(void*);
__declspec(dllimport) long XAsyncBegin(XAsyncBlock*, void*, const void*, const char*, void*);
__declspec(dllimport) long XAsyncSchedule(XAsyncBlock*, unsigned int);
__declspec(dllimport) void XAsyncComplete(XAsyncBlock*, long, unsigned long long);
__declspec(dllimport) long XAsyncGetStatus(XAsyncBlock*, bool);
__declspec(dllimport) long XAsyncGetResult(XAsyncBlock*, const void*, unsigned long long, void*, unsigned long long*);
__declspec(dllimport) void XAsyncCancel(XAsyncBlock*);
__declspec(dllimport) void* XTaskQueueGetCurrentProcessTaskQueue();
__declspec(dllimport) void XTaskQueueSetCurrentProcessTaskQueue(void*);
__declspec(dllimport) void ExitProcess(unsigned int);
}

static unsigned int ops_seen[8];
static int completion_seen;
static int cleanup_seen;
static const char payload[8] = {'p', 'a', 'y', 'l', 'o', 'a', 'd', '!'};

static long provider(unsigned int op, ProviderData* data) {
    if (op < 8) {
        ops_seen[op] += 1;
    }
    if (op == 0) {
        // Begin: schedule the work, as the documentation directs.
        XAsyncSchedule(data->async, 0);
        return 0;
    }
    if (op == 1) {
        // DoWork: the work is trivially done, completing with the payload
        // size the retrieval will ask about.
        XAsyncComplete(data->async, 0, 8);
        return 0;
    }
    if (op == 2) {
        // GetResult: copy the payload into the validated buffer.
        char* out = static_cast<char*>(data->buffer);
        unsigned long long count = data->bufferSize < 8 ? data->bufferSize : 8;
        for (unsigned long long index = 0; index < count; index++) {
            out[index] = payload[index];
        }
        return 0;
    }
    if (op == 3) {
        // Cancel: abort the work and complete with E_ABORT, as the
        // operation page documents the cancellation contract.
        XAsyncComplete(data->async, (long)0x80004004, 0);
        return 0;
    }
    if (op == 4) {
        cleanup_seen = 1;
        return 0;
    }
    return 0;
}

static void completion(XAsyncBlock*) {
    completion_seen = 1;
}

extern "C" void entry() {
    void* queue = 0;
    if (XTaskQueueCreate(0, 0, &queue) != 0 || queue == 0) {
        ExitProcess(10);
    }
    XAsyncBlock block;
    block.queue = queue;
#ifdef XENORA_PROCESS_QUEUE
    XTaskQueueSetCurrentProcessTaskQueue(queue);
    block.queue = 0;
#endif
    block.context = 0;
    block.callback = reinterpret_cast<void*>(&completion);
    for (int index = 0; index < 32; index++) {
        block.internal[index] = 0;
    }
    if (XAsyncBegin(&block, 0, reinterpret_cast<const void*>(0x1234), "entry", reinterpret_cast<void*>(&provider)) != 0) {
        ExitProcess(11);
    }
    // Begin ran synchronously inside the begin call itself.
    if (ops_seen[0] != 1) {
        ExitProcess(12);
    }
#ifdef XENORA_CANCEL
    // Cancelling before the work is pumped invokes the provider's Cancel
    // synchronously, and the operation ends aborted: the status is E_ABORT
    // and the retrieval answers it without ever invoking GetResult.
    XAsyncCancel(&block);
    if (ops_seen[3] != 1) {
        ExitProcess(30);
    }
    if ((unsigned long)XAsyncGetStatus(&block, false) != 0x80004004) {
        ExitProcess(31);
    }
    char nothing[8];
    if ((unsigned long)XAsyncGetResult(&block, reinterpret_cast<const void*>(0x1234), 8, nothing, 0) != 0x80004004) {
        ExitProcess(32);
    }
    if (ops_seen[2] != 0) {
        ExitProcess(33);
    }
    XTaskQueueCloseHandle(queue);
    ExitProcess(0);
#endif
#ifdef XENORA_PROCESS_QUEUE
    // The process queue pair: a block with no queue of its own schedules
    // through the process one, exactly as the documentation describes.
    if (XTaskQueueGetCurrentProcessTaskQueue() != queue) {
        ExitProcess(40);
    }
#endif
    // The work port pumps the scheduled DoWork, whose completion queues the
    // caller's callback on the completion port.
    XTaskQueueDispatch(queue, 0, 0);
    if (ops_seen[1] != 1) {
        ExitProcess(13);
    }
    XTaskQueueDispatch(queue, 1, 0);
    if (!completion_seen) {
        ExitProcess(14);
    }
    if (XAsyncGetStatus(&block, false) != 0) {
        ExitProcess(15);
    }
    char buffer[8];
    for (int index = 0; index < 8; index++) {
        buffer[index] = 0;
    }
    unsigned long long used = 0;
    if (XAsyncGetResult(&block, reinterpret_cast<const void*>(0x1234), 8, buffer, &used) != 0) {
        ExitProcess(16);
    }
    if (used != 8) {
        ExitProcess(17);
    }
    for (int index = 0; index < 8; index++) {
        if (buffer[index] != payload[index]) {
            ExitProcess(18);
        }
    }
    if (ops_seen[2] != 1) {
        ExitProcess(19);
    }
    // Cleanup is the operation's final invocation, chained inside the
    // retrieval before it returns.
    if (!cleanup_seen) {
        ExitProcess(20);
    }
    XTaskQueueCloseHandle(queue);
    ExitProcess(0);
}
