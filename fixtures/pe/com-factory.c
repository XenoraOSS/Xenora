// SPDX-License-Identifier: GPL-3.0-or-later
// Original synthetic class; it exists only when the host opts into the fixture.
typedef unsigned int u32;
typedef int i32;
typedef unsigned char u8;
typedef struct { u32 a; unsigned short b, c; u8 d[8]; } GUID;
typedef struct Unknown Unknown;
typedef struct Factory Factory;
typedef struct {
    i32 (*query)(Unknown*, const GUID*, void**);
    u32 (*add)(Unknown*);
    u32 (*release)(Unknown*);
} UnknownTable;
struct Unknown { const UnknownTable *vtable; };
typedef struct { const GUID *iid; Unknown *object; i32 hr; } Multi;
_Static_assert(sizeof(Multi) == 24, "Windows x64 MULTI_QI layout");
typedef struct {
    i32 (*query)(Factory*, const GUID*, void**);
    u32 (*add)(Factory*);
    u32 (*release)(Factory*);
    i32 (*create)(Factory*, Unknown*, const GUID*, void**);
    i32 (*lock)(Factory*, int);
} FactoryTable;
struct Factory { const FactoryTable *vtable; };

__declspec(dllimport) __declspec(noreturn) void ExitProcess(u32);
__declspec(dllimport) i32 CoInitializeEx(void*, u32);
__declspec(dllimport) void CoUninitialize(void);
__declspec(dllimport) i32 CoGetClassObject(const GUID*, u32, void*, const GUID*, void**);
__declspec(dllimport) i32 CoCreateInstance(const GUID*, Unknown*, u32, const GUID*, void**);
__declspec(dllimport) i32 CoCreateInstanceEx(const GUID*, Unknown*, u32, void*, u32, Multi*);
static const GUID clsid = {0x58713751, 0x7490, 0x414e, {0xa0, 0x58, 0x63, 0x6f, 0x6d, 0, 0, 1}};
static const GUID unknown = {0, 0, 0, {0xc0, 0, 0, 0, 0, 0, 0, 0x46}};
static const GUID factory_iid = {1, 0, 0, {0xc0, 0, 0, 0, 0, 0, 0, 0x46}};
static const GUID unsupported = {2, 0, 0, {0, 0, 0, 0, 0, 0, 0, 0}};
#define REQUIRE(n, expr) do { if (!(expr)) ExitProcess(n); } while (0)
void entry(void) {
    Factory *factory = 0;
    Unknown *object = 0;
    void *other = 0;
    Multi interfaces[3] = {{&unknown, 0, 0}, {&unsupported, 0, 0}, {&unknown, 0, 0}};
    REQUIRE(1, CoInitializeEx(0, 0) == 0);
    REQUIRE(2, CoGetClassObject(&clsid, 1, 0, &factory_iid, (void**)&factory) == 0 && factory);
    REQUIRE(3, factory->vtable->query(factory, &unknown, &other) == 0 && other == factory);
    REQUIRE(4, ((Unknown*)other)->vtable->release((Unknown*)other) == 1);
    other = factory;
    REQUIRE(5, factory->vtable->query(factory, &unsupported, &other) == (i32)0x80004002 && !other);
    REQUIRE(6, factory->vtable->create(factory, 0, &unknown, (void**)&object) == 0 && object);
    other = factory;
    REQUIRE(13, factory->vtable->create(factory, 0, &unsupported, &other) == (i32)0x80004002 && !other);
    other = factory;
    REQUIRE(14, factory->vtable->create(factory, object, &unknown, &other) == (i32)0x80040110 && !other);
    REQUIRE(7, object->vtable->add(object) == 2);
    REQUIRE(8, object->vtable->release(object) == 1);
    REQUIRE(9, object->vtable->release(object) == 0);
#ifdef TEST_STALE
    { const UnknownTable *volatile stale = object->vtable; (void)stale; }
    ExitProcess(99);
#endif
    REQUIRE(10, factory->vtable->release(factory) == 0);
    REQUIRE(11, CoCreateInstance(&clsid, 0, 1, &unknown, (void**)&object) == 0 && object);
    REQUIRE(12, object->vtable->release(object) == 0);
    REQUIRE(15, CoCreateInstanceEx(&clsid, 0, 1, 0, 3, interfaces) == 0x80012);
    REQUIRE(16, interfaces[0].object && interfaces[0].hr == 0 && !interfaces[1].object && interfaces[1].hr == (i32)0x80004002);
    REQUIRE(17, interfaces[2].object == interfaces[0].object && interfaces[2].hr == 0);
    REQUIRE(18, interfaces[0].object->vtable->release(interfaces[0].object) == 1);
    REQUIRE(19, interfaces[2].object->vtable->release(interfaces[2].object) == 0);
    interfaces[0].iid = &unsupported;
    interfaces[0].object = 0;
    interfaces[0].hr = 0;
    REQUIRE(20, CoCreateInstanceEx(&clsid, 0, 1, 0, 1, interfaces) == (i32)0x80004002);
    REQUIRE(21, !interfaces[0].object && interfaces[0].hr == (i32)0x80004002);
    interfaces[0].iid = &unknown;
    interfaces[0].hr = 0;
    REQUIRE(22, CoCreateInstanceEx(&clsid, 0, 1, 0, 1, interfaces) == 0);
    REQUIRE(23, interfaces[0].object && interfaces[0].object->vtable->release(interfaces[0].object) == 0);
    CoUninitialize();
    ExitProcess(0);
}
