// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once

class Object {
public:
    virtual unsigned int value() const;
    virtual ~Object();
    static void *operator new(decltype(sizeof(0)) size) noexcept;
    static void operator delete(void *block) noexcept;
};
