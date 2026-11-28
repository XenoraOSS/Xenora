// SPDX-License-Identifier: GPL-3.0-or-later

#ifndef PRODUCER_SEED
#define PRODUCER_SEED 7
#endif

static thread_local unsigned int seed = PRODUCER_SEED;
static thread_local unsigned int visits;

extern "C" __declspec(dllexport) unsigned int *ProducerSeed() {
    return &seed;
}

extern "C" __declspec(dllexport) unsigned int ProducerVisit() {
    return visits++;
}
