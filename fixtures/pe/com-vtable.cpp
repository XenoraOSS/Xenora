// SPDX-License-Identifier: GPL-3.0-or-later
// Original freestanding guest fixture for Windows x64 COM-shaped vtable calls.
using u32 = unsigned int;
using i32 = int;
using u8 = unsigned char;

struct Guid { u32 a; unsigned short b, c; u8 d[8]; };
constexpr Guid unknown_iid = {0, 0, 0, {0xc0, 0, 0, 0, 0, 0, 0, 0x46}};
constexpr Guid other_iid = {1, 0, 0, {0, 0, 0, 0, 0, 0, 0, 0}};
constexpr i32 no_interface = static_cast<i32>(0x80004002u);

struct Unknown {
    virtual i32 QueryInterface(const Guid* iid, void** out) = 0;
    virtual u32 AddRef() = 0;
    virtual u32 Release() = 0;
};
struct TestInterface : Unknown { virtual u32 Value(u32 arg) = 0; };

struct Object final : TestInterface {
    u32 references = 1;
    bool destroyed = false;
    i32 QueryInterface(const Guid* iid, void** out) override {
        *out = nullptr;
        const auto* a = reinterpret_cast<const u8*>(iid);
        const auto* b = reinterpret_cast<const u8*>(&unknown_iid);
        for (u32 i = 0; i < sizeof(Guid); ++i) if (a[i] != b[i]) return no_interface;
        *out = static_cast<Unknown*>(this);
        AddRef();
        return 0;
    }
    u32 AddRef() override { return ++references; }
    u32 Release() override { const u32 result = --references; if (!result) destroyed = true; return result; }
    u32 Value(u32 arg) override { return arg + 7; }
};

extern "C" __declspec(dllimport) __declspec(noreturn) void ExitProcess(u32);
__declspec(noinline) static void exercise(TestInterface* iface, Object* object) {
    void* queried = nullptr;
    if (iface->QueryInterface(&unknown_iid, &queried) != 0 || queried != iface) ExitProcess(1);
    if (iface->Value(35) != 42) ExitProcess(2);
    if (static_cast<Unknown*>(queried)->Release() != 1) ExitProcess(3);
    queried = iface;
    if (iface->QueryInterface(&other_iid, &queried) != no_interface || queried) ExitProcess(4);
    if (iface->Release() != 0 || !object->destroyed) ExitProcess(5);
}
extern "C" void entry() {
    Object object;
    exercise(&object, &object);
    ExitProcess(0);
}
