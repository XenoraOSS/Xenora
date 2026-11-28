# USER32 windows and posted messages

Headless PE applications can post messages to another live thread in the
same guest process. A thread establishes its queue with `PeekMessageA` or
`PeekMessageW`, including an empty peek. After that call,
`PostThreadMessageA` and `PostThreadMessageW` can enqueue a message for its
guest thread ID without waiting for delivery. Posting fails when there is
no queue for the ID or when the receiver has terminated. Each queue holds
up to 10,000 posted messages.

The 48-byte Win64 `MSG` contains a window handle or zero for a thread
message, the message ID, the full pointer-sized `wParam` and `lParam`, and
the guest clock's milliseconds at posting. The cursor coordinates and
private field are zero. A null selector admits both kinds of messages.
`HWND(-1)` selects only thread messages. An exact HWND selects messages
for that window on the calling thread. Two zero filter bounds accept any
message, and `WM_QUIT` bypasses the numeric bounds. A peek takes
the oldest matching record, while `PM_REMOVE` consumes it after a successful
copy. `PM_NOREMOVE` leaves it available. Empty peeks leave the output buffer
intact, and an invalid destination leaves a queued message intact. Either
removal mode can also use `PM_NOYIELD`. Category flags remain unsupported.

Applications can synchronize the producer and consumer with the existing
guest event services. Signal a ready event after the receiver's first
peek, post messages on the producer, then signal a delivery event before
polling on the receiver. Threads can join and release their handles
through the normal cooperative scheduler.

## Headless ANSI windows

`RegisterClassExA` accepts a Win64 `WNDCLASSEXA` with an executable guest
window procedure, an instance and an ASCII class name. Optional display
resources, class styles and extra class or window storage are unsupported.
Each guest can hold up to 64 classes and 128 live windows.

`CreateWindowExA` creates a hidden top-level window with default extended
style, an overlapped style and no parent or menu. A registered name or class
atom identifies the class. Its window procedure receives `WM_NCCREATE` and
`WM_CREATE` with the guest's Win64 `CREATESTRUCTA` before a successful
return. Rejecting creation retires the provisional handle.

`ShowWindow` supports hide, show normal, show, show without activation and
show with the existing placement without activation. It returns the
previous visibility. Visibility changes invoke `WM_SHOWWINDOW` before
the new state is visible, with the documented exception for show normal.
`PostMessageA` queues `WM_CLOSE` and application-defined messages for a
synthetic window. `DispatchMessageA` synchronously invokes its owning
thread's guest procedure and preserves the full x64 `LRESULT`.

`DestroyWindow` delivers `WM_DESTROY` and `WM_NCDESTROY`. The default
procedure accepts `WM_NCCREATE` and destroys the window on `WM_CLOSE`.
Destruction removes queued window messages and retires the handle.
Windows left by an exited owner are retired when the next window is
created or checked for visibility, or when a class is unregistered.
`UnregisterClassA` requires all windows of the class to have been retired.

These windows have no native surface, activation, painting or hardware
input. `TranslateMessage` leaves ordinary messages unchanged and refuses
keyboard translation. The message wait service still waits only on kernel
objects. Child windows and cross-thread synchronous window calls remain
unsupported.

The seven RTCore imports use the four API-set bindings listed in Microsoft's
SDK metadata. The original PE fixture imports those exact contracts and
runs the window lifecycle with a scheduled guest worker as well as on the
initial thread. [Microsoft SDK API metadata](https://github.com/MicrosoftDocs/sdk-api/tree/656e9656bfaef34dd29cd7ee9e1146aec17e924c/sdk-api-src/content/winuser)

See Microsoft's [PostThreadMessage](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-postthreadmessagew),
[PeekMessage](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-peekmessagew)
and [MSG](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-msg)
contracts.
