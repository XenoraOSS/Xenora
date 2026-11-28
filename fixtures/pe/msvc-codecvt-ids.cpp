// SPDX-License-Identifier: GPL-3.0-or-later
// Original freestanding guest checking Microsoft x64 locale::id data imports.
struct _Mbstatet {};
namespace std {
class locale { public: class id { public: unsigned long long value; }; };
template<class A, class B, class C> struct codecvt {
    static __declspec(dllimport) locale::id id;
};
}
using A = std::codecvt<char16_t, char, _Mbstatet>;
using B = std::codecvt<char32_t, char, _Mbstatet>;
using C = std::codecvt<char16_t, char8_t, _Mbstatet>;
using D = std::codecvt<char32_t, char8_t, _Mbstatet>;
extern "C" __declspec(dllimport) __declspec(noreturn) void ExitProcess(unsigned);
extern "C" void entry() {
    std::locale::id *ids[] = {&A::id, &B::id, &C::id, &D::id};
    for (unsigned i = 0; i < 4; ++i) {
        if (ids[i]->value != 0) ExitProcess(1);
        for (unsigned j = 0; j < i; ++j) if (ids[i] == ids[j]) ExitProcess(2);
    }
    ids[0]->value = 42;
    if (ids[0]->value != 42 || ids[1]->value || ids[2]->value || ids[3]->value) ExitProcess(3);
    ExitProcess(0);
}
