// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once

// Original declarations produce the Microsoft x64 import names. The stamp
// is the eight-byte Xenora fixture cell, with no locale runtime attached.
struct _Mbstatet {};
namespace std {
class locale {
public:
    class id {
    public:
        unsigned long long stamp;
    };
};
template<class Character, class Byte, class State> struct codecvt {
    static __declspec(dllimport) locale::id id;
};
}

using Id = std::locale::id;
static_assert(sizeof(Id) == 8);
using Utf16 = std::codecvt<char16_t, char, _Mbstatet>;
using Utf32 = std::codecvt<char32_t, char, _Mbstatet>;
using Utf16Char8 = std::codecvt<char16_t, char8_t, _Mbstatet>;
using Utf32Char8 = std::codecvt<char32_t, char8_t, _Mbstatet>;

inline volatile Id *cell(unsigned index) {
    volatile Id *const cells[] = {&Utf16::id, &Utf32::id, &Utf16Char8::id, &Utf32Char8::id};
    return cells[index];
}
