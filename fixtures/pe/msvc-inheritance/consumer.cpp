// SPDX-License-Identifier: GPL-3.0-or-later
#include "hierarchy.hpp"

// The producer's concrete class is deliberately absent from this translation unit.
extern "C" __declspec(dllexport) int ConsumeViews(Left *left, Right *right) {
    if (!left || !right)
        return -1;
    Shared *left_shared = left;
    Shared *right_shared = right;
    if (left_shared != right_shared || static_cast<void *>(left) == static_cast<void *>(right))
        return -2;

    const unsigned left_value = left->left_value();
    const unsigned right_value = right->right_value();
    const unsigned shared_value = left_shared->shared_value();
    if (right_shared->shared_value() != shared_value)
        return -3;
    delete right_shared;
    return static_cast<int>(left_value * 10000 + right_value * 100 + shared_value);
}
