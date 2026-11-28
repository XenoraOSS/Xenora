// SPDX-License-Identifier: GPL-3.0-or-later
// Original guest compiled against Microsoft STL headers. Its entry point is
// freestanding; the Microsoft runtime remains a dynamically linked import.
#include <iostream>
extern "C" __declspec(dllimport) __declspec(noreturn) void ExitProcess(unsigned);
extern "C" void entry() {
    {
        std::ios_base::Init init;
        std::cout.write("hello from std::cout\n", 21);
        std::cout.flush();
        if (!std::cout.good()) ExitProcess(2);
    }
    ExitProcess(0);
}
