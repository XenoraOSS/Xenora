// SPDX-License-Identifier: GPL-3.0-or-later

#include <cstdio>
#include <cstring>
#include <map>
#include <memory>
#include <string>
#include <utility>

namespace {
int constructed;
int destroyed;
int live;

struct Record {
    const int id;

    explicit Record(int value) : id(value) {
        constructed = 10 * constructed + id;
        ++live;
    }
    ~Record() {
        destroyed = 10 * destroyed + id;
        --live;
    }

    Record(const Record &) = delete;
    Record &operator=(const Record &) = delete;
};

using Records = std::map<std::string, std::unique_ptr<Record>>;
constexpr const char long_key[] = "a long key stored outside a short string buffer";

int exercise(bool reject_only) {
    Records records;
    records.try_emplace("Beta", std::make_unique<Record>(1));
    records.try_emplace(long_key, std::make_unique<Record>(2));
    records.try_emplace("gamma", std::make_unique<Record>(3));
    if (constructed != 123 || live != 3)
        return 10;
    int order = 0;
    for (const auto &entry : records)
        order = 10 * order + entry.second->id;
    if (order != 123)
        return 11;

    const Record *const beta = records.at("Beta").get();
    const Record *const gamma = records.at("gamma").get();
    auto node = records.extract("gamma");
    if (node.empty() || node.mapped().get() != gamma || records.count("gamma") != 0)
        return 12;
    node.key() = "Beta";
    auto rejected = records.insert(std::move(node));
    if (rejected.inserted || rejected.node.empty() || rejected.node.key() != "Beta" ||
        rejected.node.mapped().get() != gamma || rejected.position != records.find("Beta") ||
        rejected.position->second.get() != beta || records.size() != 2 ||
        live != 3 || destroyed != 0)
        return 13;

    if (!reject_only) {
        rejected.node.key() = "delta";
        auto accepted = records.insert(std::move(rejected.node));
        if (!accepted.inserted || !accepted.node.empty() || accepted.position->first != "delta" ||
            accepted.position->second.get() != gamma || records.size() != 3)
            return 14;
        rejected.node = records.extract(accepted.position);
    }
    if (rejected.node.empty() || rejected.node.mapped().get() != gamma ||
        records.erase("Beta") != 1 || destroyed != 1 || live != 2)
        return 15;
    {
        auto owner = std::move(rejected.node);
        if (owner.empty() || !rejected.node.empty() || owner.mapped().get() != gamma)
            return 16;
    }
    if (destroyed != 13 || live != 1 || records.erase(long_key) != 1 || !records.empty())
        return 17;
    return 0;
}
} // namespace

int main(int argc, char **argv) {
    const bool reject_only = argc == 2 && std::strcmp(argv[1], "reject") == 0;
    if (constructed != 0 || destroyed != 0 || live != 0)
        return 18;
    const int result = exercise(reject_only);
    if (result != 0)
        return result;
    if (destroyed != 132 || live != 0)
        return 19;
    std::printf("ordered nodes: %s %d %d %d\n",
                reject_only ? "rejected" : "reinserted", constructed, destroyed, live);
    return reject_only ? 23 : 0;
}
