// SPDX-License-Identifier: GPL-3.0-or-later
#ifndef XENORA_WORKER_FLS_RECORDS_H
#define XENORA_WORKER_FLS_RECORDS_H

typedef unsigned int U32;
typedef void *Handle;
typedef struct Trace {
    void *volatile record;
    volatile U32 owner_id;
    volatile U32 callback_id;
    volatile U32 constructed;
    volatile U32 destroyed;
    volatile U32 callbacks;
    volatile U32 freed;
} Trace;

#endif
