# Guest threads

`crates/loader/src/scheduler.rs` runs guest threads deterministically on one host thread. A guest thread is a CPU context, a guarded stack and a TEB; it is never a host thread. Guest threads never execute at the same time, so no guest memory-ordering model is relied on (#199).

## Scheduling

- Threads run in first-in, first-out order for a quantum of guest instructions. A turn ends at the first translated-block boundary that reaches the quantum, so small quanta do not compile extra blocks.
- The same program, quantum and clock always interleave the same way.
- A service that has to wait returns `GuestBlocked` with RIP still on its trampoline. Apart from recording the wait, and releasing the lock of a condition-variable sleep, it has no effect until a retry succeeds. The scheduler runs the call again at each turn, oldest waiter first. Each retry counts towards the instruction budget, and so does every call that ends a turn.
- A timed wait is over once its deadline has passed. An object signaled after that stays signaled.
- Signaling an event, semaphore or mutex, waking a condition variable, or releasing a lock while a thread is blocked stops the caller so that waiters are served first. It keeps what is left of its turn and cannot renew it, so repeated signals do not starve other threads.
- With nothing ready and budget left, the clock sleeps until the earliest deadline. With no deadline either, the run ends with `Outcome::Deadlock` and the blocked and suspended thread IDs.
- A fault in any thread ends the run with `Outcome::Fault` and that thread's ID. HLT ends the process only in the main thread.

## Memory

Each thread slot at `0x6_0000_0000 + n * 0x100_0000` holds a guard page, the stack, a guard page and the TEB. Stacks reserve 256 KiB by default, whatever the PE header asks. A nonzero size without `STACK_SIZE_PARAM_IS_A_RESERVATION` is an initial commitment and keeps at least that reservation; with the flag it is the reservation, rounded up to 64 KiB and at most 8 MiB. At most 64 threads live at once, the main thread included, and all thread memory together is bounded by 64 MiB. The guest cannot free it, and a slot the guest has mapped something into is skipped.

Ending a thread releases its memory and slot and drops its last error, TLS and FLS values, callback frames and pending wait. A 4-byte `errno` cell per used slot stays mapped and is zeroed for the next thread. Critical sections, SRW locks and one-time initializations it held stay held by nobody, so a thread that reuses its TEB address does not inherit them. A run keeps the main thread's state, so one that stopped on its budget can continue if no other thread was alive; a wait or sleep it was in continues with the same objects and deadline when the run starts from the reported main thread context, and is dropped otherwise. Thread IDs are never reused.

## Services

`CreateThread`, `ExitThread`, `TerminateThread`, `GetExitCodeThread`, `SuspendThread`, `ResumeThread`, `SwitchToThread`, `GetCurrentThreadStackLimits`, `GetThreadGroupAffinity`, `SetThreadGroupAffinity`, and thread handles in `GetThreadId`, `OpenThread`, `DuplicateHandle`, `CloseHandle`, `WaitForSingleObject` and `WaitForMultipleObjects`. Blocking waits with timeouts work for threads, events, semaphores and mutexes. A wait keeps the objects it was given when it blocked. A mutex whose owner ended is reported once as abandoned.

Critical sections, SRW locks, condition variables and one-time initialization block only the calling thread:

- Blocked threads are retried oldest first when a lock is released, before the thread that released it continues. A thread whose condition-variable sleep ends queues for the lock behind the threads already waiting.
- A reader waits behind a writer that was waiting first, unless that writer is suspended. The try forms ignore that order.
- A condition-variable sleep releases its lock and returns only with the lock held again. Its timeout covers the sleep, not the wait for the lock. A sleep whose timeout has passed takes no wake-up.
- A thread that waits for another thread's one-time initialization becomes the initializer if that one fails.
- A lock held by an ended thread, a slim lock acquired again by its holder, and an initialization abandoned by an ended thread are never released. Waiting for them is reported as a deadlock once nothing else can run.

`Sleep` and `SleepEx` delay only the calling thread. Zero gives up the rest of the turn, and `INFINITE` sleeps until the thread is terminated.

`GetCurrentThreadStackLimits` reads the bounds from the calling thread's TEB. Affinity has one group whose mask is every processor the guest is shown; setting that mask succeeds, a processor or group that does not exist fails with `ERROR_INVALID_PARAMETER`, and a narrower mask fails with `ERROR_NOT_SUPPORTED`. `TerminateProcess` and `GetExitCodeProcess` (in `services/process.rs`) accept only the current-process pseudo-handle and fail with `ERROR_INVALID_HANDLE` for any other; `TerminateProcess` ends the guest as `ExitProcess` does and never touches a host process. `tests/compat_fixtures/process_lifecycle.c` exercises them (`crates/loader/tests/process_lifecycle.rs`).

All are partial. Without an attached scheduler `CreateThread` fails with `ERROR_NOT_SUPPORTED` and waits behave as before.

## Not supported

- `LockCount` does not show how many threads wait, and spin counts have no effect. A condition-variable sleep in a section entered more than once stops the guest.
- Asynchronous one-time initialization, `WaitOnAddress` and `SignalObjectAndWait`.
- `XTaskQueueDispatch` and `WSAPoll` still delay every guest thread. `XTaskQueueDispatch` uses host time, so interleaving is not deterministic across it.
- The budget bounds guest instructions and service calls, not host time: with nothing else to run, a long timeout sleeps the host thread for all of it.
- The mock clock advances only when nothing can run or a service sleeps, so a guest that spins until a timed wait expires uses up its budget.
- PE static TLS and DLL thread-attach and detach callbacks for new threads, and FLS callbacks at thread exit.
- Waiting on a handle to the main thread or on a pseudo handle, thread priorities of worker threads, APCs and alertable waits, security attributes and inherited handles.
- A wait is not satisfied while its thread is suspended.
- Faults are not delivered to the guest (no SEH).

## Tests

`crates/loader/tests/runtime_threads.rs` runs C programs from `tests/threads/program.c`, compiled for x86-64 Windows by `compile.py` with clang, as translated guest code at several quanta. `compile.py --check` rebuilds the committed listing.
