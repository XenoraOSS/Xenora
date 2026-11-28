// SPDX-License-Identifier: GPL-3.0-or-later

extern "C" __declspec(dllimport) unsigned int *ProducerSeed();
static thread_local unsigned int seed = 11;
static thread_local unsigned int visits;

extern "C" __declspec(dllexport) unsigned int *ConsumerSeed() {
    return &seed;
}

extern "C" __declspec(dllexport) unsigned int ConsumerVisit() {
    return visits++;
}

extern "C" __declspec(dllexport) void ConsumerUpdate() {
    *ProducerSeed() += 10;
    seed += 20;
}
