// SPDX-License-Identifier: GPL-3.0-or-later
//
// Original guest programs for scheduler-backed XTaskQueueDispatch tests.
// Evidence is stored in the fixed shared block mapped by runtime_threads.rs.

typedef unsigned long long u64;
typedef unsigned int u32;
typedef void *HANDLE;
typedef u32 (*START)(void *);
typedef void (*QUEUE_CALLBACK)(void *, u32);

#define IMPORT __declspec(dllimport)
IMPORT HANDLE CreateThread(void *, u64, START, void *, u32, u32 *);
IMPORT u32 WaitForSingleObject(HANDLE, u32);
IMPORT int CloseHandle(HANDLE);
IMPORT u32 GetCurrentThreadId(void);
IMPORT int SwitchToThread(void);
IMPORT void Sleep(u32);
IMPORT u32 XTaskQueueCreate(u32, u32, HANDLE *);
IMPORT u32 XTaskQueueSubmitCallback(HANDLE, u32, void *, QUEUE_CALLBACK);
IMPORT u32 XTaskQueueSubmitDelayedCallback(HANDLE, u32, u32, void *, QUEUE_CALLBACK);
IMPORT u32 XTaskQueueDispatch(HANDLE, u32, u32);
IMPORT void XTaskQueueTerminate(HANDLE, u32, u32, u32);

#define INFINITE 0xFFFFFFFFu
#define THREAD(n) ((void *)(u64)(n))
struct shared {
    volatile u64 out[32];
    volatile u64 counter;
    volatile u64 queue;
    volatile u64 thread;
    volatile u64 main_thread;
};
#define S ((struct shared *)0x50000)

static HANDLE spawn(START start, u64 parameter) {
    return CreateThread(0, 0, start, THREAD(parameter), 0, 0);
}

static void join(HANDLE thread) {
    WaitForSingleObject(thread, INFINITE);
    CloseHandle(thread);
}

static u32 create_queue(void) {
    return XTaskQueueCreate(0, 0, (HANDLE *)(void *)&S->queue) == 0;
}

// context is the output slot. Bit zero records that the callback ran on the
// dispatching thread; bit 32 would record cancellation.
static void record(void *context, u32 cancelled) {
    u64 slot = (u64)context;
    S->out[slot] = (GetCurrentThreadId() == (u32)S->main_thread) | ((u64)cancelled << 32);
    S->counter++;
}

// Ready work is dispatched even when the timeout is above the old 1000 ms
// implementation limit, including INFINITE.
void main_tq_ready(void) {
    S->main_thread = GetCurrentThreadId();
    S->out[0] = create_queue();
    S->out[1] = XTaskQueueSubmitCallback((HANDLE)S->queue, 0, THREAD(8), record) == 0;
    S->out[2] = XTaskQueueDispatch((HANDLE)S->queue, 0, 1001);
    S->out[3] = S->out[8];
    S->out[4] = XTaskQueueSubmitCallback((HANDLE)S->queue, 0, THREAD(9), record) == 0;
    S->out[5] = XTaskQueueDispatch((HANDLE)S->queue, 0, INFINITE);
    S->out[6] = S->out[9];
    S->out[7] = S->counter;
}

static u32 submitter(void *unused) {
    // Let the dispatcher reach its wait first even at large quanta.
    for (int i = 0; i < 20; i++) SwitchToThread();
    S->out[5] = XTaskQueueSubmitCallback((HANDLE)S->queue, 0, THREAD(8), record) == 0;
    return 0;
}

// A dispatch waiting with INFINITE blocks only its own guest thread. The
// worker submits the callback and the blocked main thread receives it.
void main_tq_cross_thread_submit(void) {
    S->main_thread = GetCurrentThreadId();
    S->out[3] = create_queue();
    S->thread = (u64)spawn(submitter, 0);
    S->out[0] = XTaskQueueDispatch((HANDLE)S->queue, 0, INFINITE);
    S->out[1] = S->out[8];
    join((HANDLE)S->thread);
    S->out[2] = S->out[5];
}

static u32 terminator(void *unused) {
    // Let the dispatcher reach its INFINITE wait first even at large quanta.
    for (int i = 0; i < 20; i++) SwitchToThread();
    XTaskQueueTerminate((HANDLE)S->queue, 0, 0, 0);
    S->out[5] = 1;
    return 0;
}

// With no cancelled callback left to deliver, termination during an INFINITE
// wait makes the dispatch return false.
void main_tq_infinite_terminated(void) {
    S->out[3] = create_queue();
    S->thread = (u64)spawn(terminator, 0);
    S->out[0] = XTaskQueueDispatch((HANDLE)S->queue, 0, INFINITE);
    join((HANDLE)S->thread);
    S->out[1] = S->out[5];
}


static u32 late_submitter(void *unused) {
    Sleep(50);
    S->out[1] = XTaskQueueSubmitCallback((HANDLE)S->queue, 0, THREAD(8), record) == 0;
    return 0;
}

// Work submitted only after a finite deadline does not satisfy the expired
// wait. It remains queued for a later dispatch.
void main_tq_late_submit(void) {
    S->main_thread = GetCurrentThreadId();
    S->out[4] = create_queue();
    S->thread = (u64)spawn(late_submitter, 0);
    S->out[0] = XTaskQueueDispatch((HANDLE)S->queue, 0, 25);
    join((HANDLE)S->thread);
    S->out[2] = XTaskQueueDispatch((HANDLE)S->queue, 0, 0);
    S->out[3] = S->out[8];
}


// Delayed work uses the scheduler clock too: an INFINITE dispatcher sleeps
// until the delay instead of deadlocking behind host time.
void main_tq_delayed_infinite(void) {
    S->main_thread = GetCurrentThreadId();
    S->out[3] = create_queue();
    S->out[0] = XTaskQueueSubmitDelayedCallback((HANDLE)S->queue, 0, 25, THREAD(8), record) == 0;
    S->out[1] = XTaskQueueDispatch((HANDLE)S->queue, 0, INFINITE);
    S->out[2] = S->out[8];
}

// A finite wait expires without dropping the queue. Afterwards immediate work
// is still available to a zero poll.
void main_tq_timeout(void) {
    S->main_thread = GetCurrentThreadId();
    S->out[3] = create_queue();
    S->out[0] = XTaskQueueDispatch((HANDLE)S->queue, 0, 25);
    S->out[1] = XTaskQueueSubmitCallback((HANDLE)S->queue, 0, THREAD(8), record) == 0;
    S->out[2] = XTaskQueueDispatch((HANDLE)S->queue, 0, 0);
    S->out[4] = S->out[8];
}
