// SPDX-License-Identifier: GPL-3.0-or-later
#ifndef XENORA_COM_PACKAGE_INTERFACES_H
#define XENORA_COM_PACKAGE_INTERFACES_H

typedef int HResult;
typedef unsigned int U32;
typedef struct Guid {
    U32 data1;
    unsigned short data2;
    unsigned short data3;
    unsigned char data4[8];
} Guid;
_Static_assert(sizeof(Guid) == 16, "Windows GUID size");

typedef struct Unknown Unknown;
typedef struct UnknownVTable {
    HResult (*query_interface)(Unknown *, const Guid *, void **);
    U32 (*add_ref)(Unknown *);
    U32 (*release)(Unknown *);
} UnknownVTable;
struct Unknown {
    const UnknownVTable *vtable;
};

#define S_OK 0
#define S_FALSE 1
#define E_NOINTERFACE ((HResult)0x80004002u)
#define E_POINTER ((HResult)0x80004003u)

static const Guid iid_unknown = {0, 0, 0, {0xc0, 0, 0, 0, 0, 0, 0, 0x46}};

#endif
