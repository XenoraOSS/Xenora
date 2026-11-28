// SPDX-License-Identifier: GPL-3.0-or-later
struct Payload {
    unsigned *const trace;
    const unsigned value;
    Payload(unsigned *record, unsigned payload) noexcept : trace(record), value(payload) {}
    Payload(const Payload &) = delete;
    Payload &operator=(const Payload &) = delete;
    ~Payload();
};
