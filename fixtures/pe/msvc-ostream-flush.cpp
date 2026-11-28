// SPDX-License-Identifier: GPL-3.0-or-later
// Original guest supplies a synthetic Microsoft x64 ostream/streambuf layout.
using u32 = unsigned int;
using u64 = unsigned long long;
using i32 = int;
extern "C" __declspec(dllimport) void *flush(void *) __asm("?flush@?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV12@XZ");
extern "C" __declspec(dllimport) __declspec(noreturn) void ExitProcess(u32);
struct Buffer { void **vtable; u32 locks, unlocks, syncs; i32 result; };
extern "C" void do_lock(Buffer *p) { ++p->locks; }
extern "C" void do_unlock(Buffer *p) { ++p->unlocks; }
extern "C" i32 do_sync(Buffer *p) { ++p->syncs; return p->result; }
struct Ios { void *vtable; u64 standard; u32 state, exceptions, flags, pad; u64 precision, width, array, calls, locale; Buffer *buffer; void *tie; char fill; };
struct Stream { const i32 *vbtable; u32 padding; u32 vtordisp; Ios ios; };
extern "C" void entry() {
#ifdef XENORA_INVALID
    flush(reinterpret_cast<void *>(~u64(0)));
    ExitProcess(8);
#else
    static const i32 vbtable[] = {0, 16};
    void *table[15]{};
    table[1] = reinterpret_cast<void *>(&do_lock);
    table[2] = reinterpret_cast<void *>(&do_unlock);
    table[13] = reinterpret_cast<void *>(&do_sync);
    Buffer buffer{table, 0, 0, 0, 0};
    Stream stream{};
    stream.vbtable = vbtable;
    stream.ios.buffer = &buffer;
#ifdef XENORA_TIED
    stream.ios.tie = &buffer;
    flush(&stream);
    ExitProcess(9);
#endif
    if (flush(&stream) != &stream || buffer.locks != 1 || buffer.syncs != 1 || buffer.unlocks != 1 || stream.ios.state != 0) ExitProcess(1);
    buffer.result = -1;
    if (flush(&stream) != &stream || buffer.locks != 2 || buffer.syncs != 2 || buffer.unlocks != 2 || stream.ios.state != 4) ExitProcess(2);
    if (flush(&stream) != &stream || buffer.locks != 3 || buffer.syncs != 2 || buffer.unlocks != 3) ExitProcess(3);
    stream.ios.buffer = nullptr;
    if (flush(&stream) != &stream || buffer.locks != 3) ExitProcess(4);
    ExitProcess(0);
#endif
}
