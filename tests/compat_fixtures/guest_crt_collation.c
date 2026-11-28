// SPDX-License-Identifier: GPL-3.0-or-later

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    char *key;
    unsigned ordinal;
} Record;

typedef struct {
    const char *text;
    unsigned ordinal;
} Input;

static int compare(const void *left, const void *right) {
    const Record *a = left;
    const Record *b = right;
    const int by_key = strcmp(a->key, b->key);
    return by_key ? by_key : (a->ordinal > b->ordinal) - (a->ordinal < b->ordinal);
}

static int boundaries(void) {
    const char source[] = {(char)0x80, (char)0xff, 0, 'X'};
    char guard[] = {'a', 'b', 'c', '!'};
    if (strxfrm(NULL, source, 0) != 2 ||
        strxfrm(guard, source, 0) != 2 || memcmp(guard, "abc!", 4) != 0)
        return 10;
    if (strxfrm(guard, source, 2) != 2 || guard[2] != 'c' || guard[3] != '!')
        return 11;
    if (strxfrm(guard, source, 3) != 2 || memcmp(guard, source, 3) != 0 ||
        guard[3] != '!' || source[3] != 'X')
        return 12;
    if (!(strcoll("Beta", "alpha") < 0 && strcoll("alpha", "Beta") > 0 &&
          strcoll("Beta", "Beta") == 0 && strcoll("z", "\x80") < 0 &&
          strcoll("\x80", "\xff") < 0))
        return 13;
    return 0;
}

static int index_records(int reverse) {
    static const Input input[] = {
        {"\xff", 6}, {"Beta", 4}, {"alpha", 3}, {"", 1}, {"\x80", 5}, {"Beta", 2},
    };
    static const unsigned order[] = {1, 2, 4, 3, 5, 6};
    static const unsigned positions[] = {5, 2, 3, 0, 4, 1};
    enum { COUNT = sizeof input / sizeof *input };
    Record *records = calloc(COUNT, sizeof *records);
    if (!records)
        return 20;

    int status = 0;
    for (unsigned i = 0; i < COUNT; ++i) {
        const Input *source = &input[reverse ? COUNT - i - 1 : i];
        const size_t length = strxfrm(NULL, source->text, 0);
        if (length != strlen(source->text)) {
            status = 26;
            goto cleanup;
        }
        char *key = malloc(length + 2);
        if (!key) {
            status = 21;
            goto cleanup;
        }
        records[i].key = key;
        records[i].ordinal = source->ordinal;
        key[length + 1] = '!';
        if (strxfrm(key, source->text, length + 1) != length ||
            memcmp(key, source->text, length + 1) != 0 || key[length + 1] != '!') {
            status = 22;
            goto cleanup;
        }
    }

    qsort(records, COUNT, sizeof *records, compare);
    for (unsigned i = 0; i < COUNT; ++i) {
        if (records[i].ordinal != order[i]) {
            status = 23;
            goto cleanup;
        }
    }
    for (unsigned i = 0; i < COUNT; ++i) {
        const Record needle = {(char *)input[i].text, input[i].ordinal};
        const Record *found = bsearch(&needle, records, COUNT, sizeof *records, compare);
        if (found != &records[positions[i]]) {
            status = 24;
            goto cleanup;
        }
    }
    const Record absent[] = {{"Beta", 9}, {"missing", 9}};
    for (unsigned i = 0; i < sizeof absent / sizeof *absent; ++i) {
        if (bsearch(&absent[i], records, COUNT, sizeof *records, compare) != NULL) {
            status = 25;
            goto cleanup;
        }
    }
    printf("collation: %u %u %u %u %u %u\n", records[0].ordinal, records[1].ordinal,
           records[2].ordinal, records[3].ordinal, records[4].ordinal, records[5].ordinal);
    puts("lookup: six positions, two misses");

cleanup:
    for (unsigned i = 0; i < COUNT; ++i)
        free(records[i].key);
    free(records);
    return status;
}

int main(int argc, char **argv) {
    static unsigned launches;
    if (launches++ != 0)
        return 30;
    if (argc != 2)
        return 31;
    const int reverse = strcmp(argv[1], "replay") == 0;
    if (!reverse && strcmp(argv[1], "initial") != 0)
        return 32;
    const int status = boundaries();
    if (status)
        return status;
    const int indexed = index_records(reverse);
    if (!indexed)
        puts(reverse ? "input: reverse" : "input: forward");
    return indexed;
}
