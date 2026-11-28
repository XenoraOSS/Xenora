// SPDX-License-Identifier: GPL-3.0-or-later

extern "C" __declspec(dllimport) unsigned int *ProducerSeed();
extern "C" __declspec(dllimport) unsigned int ProducerVisit();
extern "C" __declspec(dllimport) unsigned int *ConsumerSeed();
extern "C" __declspec(dllimport) unsigned int ConsumerVisit();
extern "C" __declspec(dllimport) void ConsumerUpdate();
static thread_local volatile unsigned int seed = 13;
static thread_local volatile unsigned int visits;

extern "C" unsigned int entry() {
    unsigned int *producer = ProducerSeed();
    unsigned int *consumer = ConsumerSeed();
    if (producer == consumer || producer == &seed || consumer == &seed)
        return 10;
    if (*producer != 7)
        return 11;
    if (*consumer != 11 || seed != 13)
        return 12;
    if (ProducerVisit() != 0 || ConsumerVisit() != 0 || visits != 0)
        return 13;

    *consumer = 21;
    ConsumerUpdate();
    seed = 33;
    visits = 5;
    if (*producer != 17 || *consumer != 41 || seed != 33)
        return 14;
    if (ProducerVisit() != 1 || ConsumerVisit() != 1 || visits != 5)
        return 15;
    if (ProducerSeed() != producer || ConsumerSeed() != consumer)
        return 16;
    return 0;
}
