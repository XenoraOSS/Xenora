// SPDX-License-Identifier: GPL-3.0-or-later
#ifndef XENORA_COM_LIFECYCLE_INTERFACES_H
#define XENORA_COM_LIFECYCLE_INTERFACES_H

typedef int HResult;
typedef unsigned int U32;
typedef void *Handle;

typedef struct Guid {
    U32 data1;
    unsigned short data2;
    unsigned short data3;
    unsigned char data4[8];
} Guid;
_Static_assert(sizeof(Guid) == 16, "Windows GUID size");

typedef struct Unknown Unknown;
struct Unknown {
    const struct {
        HResult (*query)(Unknown *, const Guid *, void **);
        U32 (*add_ref)(Unknown *);
        U32 (*release)(Unknown *);
    } *vtable;
};

typedef struct Factory Factory;
struct Factory {
    const struct {
        HResult (*query)(Factory *, const Guid *, void **);
        U32 (*add_ref)(Factory *);
        U32 (*release)(Factory *);
        HResult (*create)(Factory *, Unknown *, const Guid *, void **);
        HResult (*lock)(Factory *, int);
    } *vtable;
};

#define S_OK 0
#define S_FALSE 1
#define E_NOINTERFACE ((HResult)0x80004002u)
#define E_POINTER ((HResult)0x80004003u)
#define CO_E_NOTINITIALIZED ((HResult)0x800401f0u)
#define RPC_E_CHANGED_MODE ((HResult)0x80010106u)

static const Guid iid_unknown = {0, 0, 0, {0xc0, 0, 0, 0, 0, 0, 0, 0x46}};

#endif
