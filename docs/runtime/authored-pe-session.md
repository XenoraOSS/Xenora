# Persistent sessions for authored PE programs

`xenora-pe-session` starts an explicitly supplied PE32+ application in a
separate worker. Each input runs on the same translated CPU, guest address
space, system-service state and graphics context. The guest implements the
frame boundary and performs its own application teardown.

On Apple Silicon, build the original example and the native launcher from
the repository root.

```sh
sh fixtures/pe/authored-session/build-authored-session.sh artifacts/authored-session
cargo build --locked -p xenora --bin xenora-pe-session
guest=artifacts/authored-session/authored-session.exe
digest=$(shasum -a 256 "$guest" | cut -d ' ' -f 1)
printf '10\n21\n10\n' | cargo run --locked -p xenora --bin xenora-pe-session -- \
    --experimental --pe "$guest" --sha256 "$digest"
```

The build also produces a packaged example. The companion DLL initializes
its state on attach and updates it across all three inputs.

```sh
guest=artifacts/authored-session/authored-session-package.exe
companion=artifacts/authored-session/xenora-authored-companion.dll
guest_pin=$(shasum -a 256 "$guest" | cut -d ' ' -f 1)
companion_pin=$(shasum -a 256 "$companion" | cut -d ' ' -f 1)
printf '10\n21\n10\n' | cargo run --locked -p xenora --bin xenora-pe-session -- \
    --experimental --pe "$guest" --sha256 "$guest_pin" \
    --package "$companion" "$companion_pin" --graphics-backend metal
```

Repeat `--package <dll-path> <sha256>` for each companion. The package
loader resolves imports using the supplied basenames and registered Xenora
services. Package basenames use printable ASCII and are case-insensitive.
`xenora-session-v1.dll` is reserved for the host protocol. The worker
reads regular files into a combined snapshot of at most 64 MiB and
verifies the SHA-256 of each image before executing guest code. Up to 255
companions may accompany the executable. Package startup orders attach and
initialization according to dependencies and uses a 64 KiB entry stack.

`--graphics-backend reference` selects the default software Reference
renderer. `--graphics-backend metal` uses the native offscreen Metal
adapter and surfaces backend failures. Both backends report the same RGBA8
format and use the same session protocol.

The same command can read lines from a terminal. Each line contains one
unsigned 32-bit decimal input followed by a newline. Closing stdin sends
Stop. The launcher writes `ready` after startup, a numbered `frame` line
with the dimensions and SHA-256 of each accepted RGBA8 image, and
`done frames=<count>` after clean exit. Add `--pixels` to include lowercase
hexadecimal RGBA bytes on each frame line. A previously delivered frame can
be followed by a session failure. Consumers should require the terminal
`done` line and a successful exit status when recording a complete replay.

The phase deadline defaults to 15 seconds for startup,
each input-to-frame exchange and shutdown. Use `--phase-seconds <1..=60>` to
change it. Host stdout writes share the active phase deadline, so a
consumer must continue draining the output pipe. Waiting at a confirmed
input boundary has no idle deadline. A disconnected output pipe ends the
session on macOS.
`--max-steps <1..=10000000>` bounds the guest entry and workers across the
session, with a default of 2,000,000. Package initialization and detach
use the loader's callback budgets. The launcher accepts up to 64 inputs
and frame sizes up to 256×256. Exit code 2 indicates a rejected command
line, 124 a phase deadline and 125 a session failure.

`--deterministic-clock` sets the guest UTC clock to 1970-01-01 00:00:00 and
starts guest monotonic time at zero. Both advance by 16 ms just before the
guest receives each input. Guest sleeps and scheduler timeout waits also
advance this clock. Host idle time and Stop add no time. The launcher still
uses real wall time for phase deadlines. The default uses the system clock.
This mode stabilizes programs whose timing comes from the guest clock.

## Guest contract

Link against the Win64 C ABI from `xenora-session-v1.dll`. The
[`xenora-session-v1.h`](../../fixtures/pe/authored-session/xenora-session-v1.h)
header and the example's import-library build script define the three calls.

1. Call `XenoraSessionNextInput(&value)` on the main guest thread during
   root executable entry and again after each frame. The worker pauses at
   this call and returns 1 with the input, or 0 on Stop. Guest workers must
   be quiescent at the boundary since they share the scheduler and can
   only run while guest execution proceeds.
2. Execute exactly one supported graphics command list per accepted input,
   then call `XenoraSessionPresent()`. The worker checks the completed
   presentation and sends its pixels. A frame is accepted when the guest
   has returned to its next input boundary.
3. After Stop, close system-service handles and release the application's
   graphics resources. Call `XenoraSessionFinish()` to confirm cleanup and
   terminate with status zero, for example through `ExitProcess(0)`. At
   Finish the worker checks the kernel, socket, WinHTTP and GDK task queue
   handle tables. Graphics cleanup remains the guest's responsibility. The
   worker checks the final frame index before reporting completion. For
   packages it also requires completed detach callbacks and checks for
   handles left open by those callbacks before returning `done`. Session
   ABI calls from package attach or detach callbacks are rejected.

The example keeps one synthetic file open across requests. Its journal
records the full input and sequence, and the guest rereads and verifies it
before closing the file. Each 4×3 frame encodes the input and same session's
sequence. The CLI tests compare every RGBA byte and exercise invalid input,
digest rejection, a stalled guest, protocol errors and forgotten handles.

This entry point is for authored guests that implement the session ABI.
