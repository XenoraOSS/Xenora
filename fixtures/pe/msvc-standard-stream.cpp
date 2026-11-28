// SPDX-License-Identifier: GPL-3.0-or-later
// Original freestanding MSVC-ABI guest for bounded std::cout initialization.
// The declarations are the documented x64 decorated exports; this is not a
// copy of the Microsoft STL headers or a general iostream compatibility test.
using u32 = unsigned int;
using u64 = unsigned long long;
using i64 = long long;

namespace std {
class ios_base {
public:
    class Init {
    public:
        __declspec(dllimport) Init();
        __declspec(dllimport) ~Init();
    };
};
}

extern "C" {
__declspec(dllimport) void *write_stream(void *, const char *, i64)
    __asm("?write@?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV12@PEBD_J@Z");
__declspec(dllimport) void *flush_stream(void *)
    __asm("?flush@?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV12@XZ");
__declspec(dllimport) bool good_stream(void *)
    __asm("?good@ios_base@std@@QEBA_NXZ");
__declspec(dllimport) unsigned char cout_object[128]
    __asm("?cout@std@@3V?$basic_ostream@DU?$char_traits@D@std@@@1@A");
__declspec(dllimport) __declspec(noreturn) void ExitProcess(u32);
}

extern "C" void entry() {
#ifdef XENORA_EXCEPTION
    // A bad stream whose exception mask includes badbit must stop before any
    // transfer. The fields are the ones the bounded synthetic object reads.
    *reinterpret_cast<int *>(cout_object + 16) = 4;
    *reinterpret_cast<int *>(cout_object + 20) = 4;
#endif
    {
        std::ios_base::Init init;
        void *stream = cout_object;
#ifdef XENORA_INVALID
        stream = reinterpret_cast<void *>(~u64(0));
#endif
        i64 count = 21;
#ifdef XENORA_OVERSIZE
        count = (i64{1} << 20) + 1;
#endif
        if (write_stream(stream, "hello from std::cout\n", count) != stream) ExitProcess(1);
        if (flush_stream(stream) != stream) ExitProcess(2);
#ifndef XENORA_EXCEPTION
        if (!good_stream(stream)) ExitProcess(3);
#endif
    }
#ifndef XENORA_EXCEPTION
    ExitProcess(0);
#else
    ExitProcess(9); // The exception-mask case must stop before this line.
#endif
}
