// SPDX-License-Identifier: GPL-3.0-or-later
// Original guest exercising Microsoft x64 ostream and guest xsputn callbacks.
using u32 = unsigned int;
using u64 = unsigned long long;
using i64 = long long;
extern "C" __declspec(dllimport) void *write_stream(void *, const char *, i64) __asm("?write@?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV12@PEBD_J@Z");
extern "C" __declspec(dllimport) __declspec(noreturn) void ExitProcess(u32);
struct Buffer { void **vtable; u32 locks, unlocks, writes; i64 result; char bytes[8]; };
extern "C" void do_lock(Buffer *p) { ++p->locks; }
extern "C" void do_unlock(Buffer *p) { ++p->unlocks; }
extern "C" i64 do_write(Buffer *p, const char *text, i64 count) {
    ++p->writes;
    for (i64 i = 0; i < count && i < 8; ++i) p->bytes[i] = text[i];
    return p->result;
}
struct Ios { void *vtable; u64 standard; u32 state, exceptions, flags, pad; u64 precision, width, array, calls, locale; Buffer *buffer; void *tie; char fill; };
struct Stream { const int *vbtable; u32 padding, vtordisp; Ios ios; };
extern "C" void entry() {
#ifdef XENORA_INVALID
    write_stream(reinterpret_cast<void *>(~u64(0)), "abc", 3);
    ExitProcess(8);
#else
    static const int vbtable[] = {0, 16};
    void *table[15]{};
    table[1] = reinterpret_cast<void *>(&do_lock);
    table[2] = reinterpret_cast<void *>(&do_unlock);
    table[9] = reinterpret_cast<void *>(&do_write);
    Buffer buffer{table, 0, 0, 0, 3, {}};
    Stream stream{}; stream.vbtable = vbtable; stream.ios.buffer = &buffer;
#ifdef XENORA_EXCEPTION
    stream.ios.exceptions = 4; buffer.result = 1;
    write_stream(&stream, "abc", 3);
    ExitProcess(9);
#endif
    if (write_stream(&stream, "abc", 3) != &stream || buffer.locks != 1 || buffer.writes != 1 || buffer.unlocks != 1 || stream.ios.state != 0) ExitProcess(1);
    if (buffer.bytes[0] != 'a' || buffer.bytes[1] != 'b' || buffer.bytes[2] != 'c') ExitProcess(2);
    if (write_stream(&stream, "", 0) != &stream || buffer.writes != 1) ExitProcess(3);
    buffer.result = 1;
    if (write_stream(&stream, "abc", 3) != &stream || stream.ios.state != 4 || buffer.writes != 2) ExitProcess(4);
    if (write_stream(&stream, "abc", 3) != &stream || stream.ios.state != 4 || buffer.writes != 2 || buffer.locks != 4 || buffer.unlocks != 4) ExitProcess(5);
    ExitProcess(0);
#endif
}
