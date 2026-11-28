// SPDX-License-Identifier: GPL-3.0-or-later
// Original freestanding MSVC-ABI guest exercising the public STL Init ABI.
// This is not a copy of the Microsoft STL header or a complete iostream program.
namespace std {
class ios_base {
public:
    class Init {
    public:
        Init() { _Init_ctor(this); }
        ~Init() { _Init_dtor(this); }
        static int &count() { return _Init_cnt_func(); }
        static int &data() { return _Init_cnt; }
    private:
        __declspec(dllimport) static void _Init_ctor(Init *);
        __declspec(dllimport) static void _Init_dtor(Init *);
        __declspec(dllimport) static int &_Init_cnt_func();
        __declspec(dllimport) static int _Init_cnt;
    };
};
}
extern "C" __declspec(dllimport) int puts(const char *);
extern "C" __declspec(dllimport) __declspec(noreturn) void ExitProcess(unsigned);
extern "C" void entry() {
    if (&std::ios_base::Init::data() != &std::ios_base::Init::count() || std::ios_base::Init::data() != -1) ExitProcess(1);
    {
        std::ios_base::Init outer;
        if (std::ios_base::Init::count() != 1) ExitProcess(2);
        {
            std::ios_base::Init inner;
            if (std::ios_base::Init::count() != 2) ExitProcess(3);
        }
        if (std::ios_base::Init::count() != 1) ExitProcess(4);
        puts("nested initialization complete");
    }
    ExitProcess(5); // The final destructor requires unsupported stream flushing.
}
