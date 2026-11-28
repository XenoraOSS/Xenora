// SPDX-License-Identifier: GPL-3.0-or-later
// Original freestanding probe of Xenora's narrow MSVC-ABI ostream slice.
// These declarations generate the imports without a vendor header or layout.
namespace std {
template <class Character> struct char_traits;
template <class Character, class Traits> class basic_ostream {
public:
    __declspec(dllimport) basic_ostream &write(const char *, long long);
    __declspec(dllimport) basic_ostream &flush();
};
using ostream = basic_ostream<char, char_traits<char>>;
extern __declspec(dllimport) ostream cout;
extern __declspec(dllimport) ostream clog;
class ios_base {
public:
    class Init {
    public:
        __declspec(dllimport) Init();
        __declspec(dllimport) ~Init();
    };
};
}

extern "C" __declspec(dllimport) bool good(const void *)
    __asm("?good@ios_base@std@@QEBA_NXZ");
extern "C" __declspec(dllimport) __declspec(noreturn) void ExitProcess(unsigned);
extern "C" __declspec(dllimport) int _close(int);
extern "C" __declspec(dllimport) void *GetProcessHeap();
extern "C" __declspec(dllimport) int HeapFree(void *, unsigned, void *);

struct Report {
    std::ostream *volatile out;
    std::ostream *volatile log;
    volatile unsigned phase;
};
#pragma section(".gate", read, write)
__declspec(allocate(".gate")) Report report;

extern "C" void entry() {
    report.out = &std::cout;
    report.log = &std::clog;
    if (report.out == report.log || !good(report.out) || !good(report.log))
        ExitProcess(1);

    {
        std::ios_base::Init outer;
        {
            std::ios_base::Init inner;
            if (&std::cout.write("out\n", 4).flush() != report.out ||
                &std::clog.write("log\n", 4).flush() != report.log)
                ExitProcess(2);
#if defined(OVERSIZED_WRITE)
            std::clog.write("x", (1 << 20) + 1);
            ExitProcess(3);
#elif defined(UNMAPPED_ON_FINAL_FLUSH)
            // Unmap both objects to make the first final flush observable.
            void *heap = GetProcessHeap();
            if (!HeapFree(heap, 0, report.out) || !HeapFree(heap, 0, report.log))
                ExitProcess(4);
#else
            if (_close(2) != 0)
                ExitProcess(5);
            std::clog.write("closed\n", 7);
            if (good(report.log) || !good(report.out))
                ExitProcess(6);
            std::cout.write("out still good\n", 15);
#endif
        }
        report.phase = 1;
    }
    report.phase = 2;
    ExitProcess(0);
}
