// SPDX-License-Identifier: GPL-3.0-or-later
#include "hierarchy.hpp"

extern "C" __declspec(dllimport) void *malloc(decltype(sizeof(0)));
extern "C" __declspec(dllimport) void free(void *);
extern "C" __declspec(dllimport) int putchar(int);

static unsigned event_count;
static void record(char event) {
    ++event_count;
    putchar(event);
}

Shared::Shared(unsigned seed) : shared_seed(seed) { record('S'); }
Shared::~Shared() { record('s'); }
unsigned Shared::shared_value() const { return 0; }

Left::Left(unsigned seed) : Shared(0), left_seed(seed) { record('L'); }
Left::~Left() { record('l'); }
unsigned Left::left_value() const { return 0; }

Right::Right(unsigned seed) : Shared(0), right_seed(seed) { record('R'); }
Right::~Right() { record('r'); }
unsigned Right::right_value() const { return 0; }

void *Shared::operator new(decltype(sizeof(0)) size) noexcept { return malloc(size); }
void Shared::operator delete(void *block) noexcept {
    free(block);
    record('F');
}

class Product final : public Left, public Right {
    unsigned seed;

public:
    explicit Product(unsigned seed) : Shared(13), Left(7), Right(11), seed(seed) {
        record('P');
    }
    unsigned left_value() const override {
        record('a');
        return seed + left_seed + shared_seed;
    }
    unsigned right_value() const override {
        record('b');
        return 2 * seed + right_seed + shared_seed;
    }
    unsigned shared_value() const override {
        record('c');
        return 2 * seed + shared_seed;
    }
    ~Product() override { record('p'); }
};

extern "C" __declspec(dllexport) bool ProduceViews(Views *views) {
    Product *product = new Product(5);
    if (!product)
        return false;
    *views = {product, product, product};
    return true;
}

extern "C" __declspec(dllexport) unsigned EventCount() {
    return event_count;
}
