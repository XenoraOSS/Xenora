// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once

class Shared {
protected:
    unsigned shared_seed;
    explicit Shared(unsigned seed);

public:
    virtual unsigned shared_value() const;
    virtual ~Shared();
    static void *operator new(decltype(sizeof(0)) size) noexcept;
    static void operator delete(void *block) noexcept;
};

class Left : public virtual Shared {
protected:
    unsigned left_seed;
    explicit Left(unsigned seed);

public:
    virtual unsigned left_value() const;
    ~Left() override;
};

class Right : public virtual Shared {
protected:
    unsigned right_seed;
    explicit Right(unsigned seed);

public:
    virtual unsigned right_value() const;
    ~Right() override;
};

struct Views {
    Left *left;
    Right *right;
    void *allocation;
};
