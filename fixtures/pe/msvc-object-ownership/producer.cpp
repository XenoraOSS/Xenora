// SPDX-License-Identifier: GPL-3.0-or-later
#include "object.hpp"

extern "C" __declspec(dllimport) void *malloc(decltype(sizeof(0)) size);
extern "C" __declspec(dllimport) void free(void *block);

static unsigned int trace;
static void record(unsigned int event) {
    trace = trace * 10 + event;
}

unsigned int Object::value() const {
    return 0;
}

Object::~Object() {
    record(4);
}

void *Object::operator new(decltype(sizeof(0)) size) noexcept {
    return malloc(size);
}

void Object::operator delete(void *block) noexcept {
    free(block);
    record(5);
}

class Produced final : public Object {
    unsigned int seed;
public:
    explicit Produced(unsigned int value) : seed(value) {
        record(1);
    }
    unsigned int value() const override {
        record(2);
        return seed * 3 + 7;
    }
    ~Produced() override {
        record(3);
    }
};

extern "C" __declspec(dllexport) Object *XenoraConstruct(unsigned int seed) {
    return new Produced(seed);
}

extern "C" __declspec(dllexport) unsigned int XenoraObjectTrace() {
    return trace;
}
