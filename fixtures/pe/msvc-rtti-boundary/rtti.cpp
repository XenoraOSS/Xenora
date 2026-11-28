// SPDX-License-Identifier: GPL-3.0-or-later
#ifndef CASE
#define CASE 0
#endif

namespace std {
class type_info;
}

extern "C" __declspec(dllimport) __declspec(noreturn) void ExitProcess(unsigned);

struct Left {
    virtual unsigned left() const { return 29; }
};
struct Right {
    virtual unsigned right() const { return 31; }
};
struct Both final : Left, Right {
    unsigned left() const override { return 43; }
    unsigned right() const override { return 47; }
};
struct Unrelated final : Left {
    unsigned left() const override { return 53; }
};

__declspec(noinline) unsigned dispatch(const Left *p) { return p->left(); }
__declspec(noinline) unsigned dispatch(const Right *p) { return p->right(); }

#if CASE == 1
__declspec(noinline) const std::type_info *dynamic_type(const Left *p) {
    return &typeid(*p);
}
#elif CASE == 2
__declspec(noinline) Both *down(Left *p) { return dynamic_cast<Both *>(p); }
__declspec(noinline) Left *across(Right *p) { return dynamic_cast<Left *>(p); }
#elif CASE == 3
__declspec(noinline) void *complete_object(Right *p) { return dynamic_cast<void *>(p); }
#endif

extern "C" void entry(void) {
    Both object;
    Unrelated other;
    Left *left = &object;
    Right *right = &object;
    if (dispatch(left) != 43 || dispatch(right) != 47 ||
        (void *)right == (void *)&object || dispatch(&other) != 53) {
        ExitProcess(10);
    }
#if CASE == 1
    if (dynamic_type(left) != &typeid(Both) ||
        dynamic_type(&other) != &typeid(Unrelated) ||
        dynamic_type(left) == dynamic_type(&other)) {
        ExitProcess(11);
    }
#elif CASE == 2
    if (down(left) != &object || across(right) != left ||
        down(&other) != nullptr || down(nullptr) != nullptr) {
        ExitProcess(12);
    }
#elif CASE == 3
    if (complete_object(right) != &object || complete_object(nullptr) != nullptr) {
        ExitProcess(13);
    }
#endif
    ExitProcess(0);
}
