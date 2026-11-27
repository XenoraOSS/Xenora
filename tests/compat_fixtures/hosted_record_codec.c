// SPDX-License-Identifier: GPL-3.0-or-later

#include <stdint.h>
#include <stdio.h>
#include <string.h>

enum { RAW_CAPACITY = 64, PACKED_CAPACITY = 2 * RAW_CAPACITY };

struct record {
    uint16_t id;
    uint32_t value;
    const char *tag;
    uint8_t tag_length;
};

static const struct record records[] = {
    {0x1234, 0x01020304, "AAAAB", 5},
    {0x00ff, 0, "ccccc", 5},
    {0xbeef, 0xffffffff, "XYZ", 3},
};

static size_t serialize(uint8_t *output) {
    size_t length = 0;
    output[length++] = sizeof records / sizeof records[0];
    for (size_t i = 0; i < sizeof records / sizeof records[0]; i++) {
        const struct record *record = &records[i];
        output[length++] = (uint8_t)record->id;
        output[length++] = (uint8_t)(record->id >> 8);
        for (unsigned shift = 0; shift < 32; shift += 8)
            output[length++] = (uint8_t)(record->value >> shift);
        output[length++] = record->tag_length;
        memcpy(output + length, record->tag, record->tag_length);
        length += record->tag_length;
    }
    return length;
}

static int records_match(const uint8_t *input, size_t length) {
    size_t cursor = 0;
    if (!length || input[cursor++] != sizeof records / sizeof records[0])
        return 0;
    for (size_t i = 0; i < sizeof records / sizeof records[0]; i++) {
        if (length - cursor < 7)
            return 0;
        unsigned id = input[cursor] | (unsigned)input[cursor + 1] << 8;
        cursor += 2;
        uint32_t value = 0;
        for (unsigned shift = 0; shift < 32; shift += 8)
            value |= (uint32_t)input[cursor++] << shift;
        size_t tag_length = input[cursor++];
        if (tag_length > length - cursor || id != records[i].id ||
            value != records[i].value || tag_length != records[i].tag_length ||
            memcmp(input + cursor, records[i].tag, tag_length))
            return 0;
        cursor += tag_length;
    }
    return cursor == length;
}

static size_t run_length(const uint8_t *input, size_t length) {
    size_t run = 1;
    while (run < length && run < 130 && input[run] == input[0])
        run++;
    return run;
}

// With its high bit clear, the header encodes 1-128 literal bytes. With
// its high bit set, it encodes 3-130 copies of the following byte.
static size_t pack(const uint8_t *input, size_t length, uint8_t *output) {
    size_t read = 0;
    size_t written = 0;
    while (read < length) {
        size_t run = run_length(input + read, length - read);
        if (run >= 3) {
            output[written++] = (uint8_t)(0x80 | (run - 3));
            output[written++] = input[read];
            read += run;
        } else {
            size_t start = read++;
            while (read < length && read - start < 128 &&
                   run_length(input + read, length - read) < 3)
                read++;
            size_t literal = read - start;
            output[written++] = (uint8_t)(literal - 1);
            memcpy(output + written, input + start, literal);
            written += literal;
        }
    }
    return written;
}

enum decode_status { DECODE_OK, DECODE_TRUNCATED, DECODE_OVERFLOW, DECODE_LENGTH };

static enum decode_status unpack(const uint8_t *input, size_t length,
                                 uint8_t *output, size_t expected_length) {
    size_t read = 0;
    size_t written = 0;
    while (read < length) {
        unsigned header = input[read++];
        int is_run = (header & 0x80) != 0;
        size_t count = is_run ? (header & 0x7f) + 3 : header + 1;
        size_t payload = is_run ? 1 : count;
        if (payload > length - read)
            return DECODE_TRUNCATED;
        if (count > expected_length - written)
            return DECODE_OVERFLOW;
        if (is_run)
            memset(output + written, input[read], count);
        else
            memcpy(output + written, input + read, count);
        read += payload;
        written += count;
    }
    return written == expected_length ? DECODE_OK : DECODE_LENGTH;
}

static int store_and_reload(const uint8_t *packed, size_t length, uint8_t *reloaded) {
    FILE *file = fopen("records.rle", "wb");
    if (!file)
        return 0;
    size_t written = fwrite(packed, 1, length, file);
    if (fclose(file) || written != length)
        return 0;

    file = fopen("records.rle", "rb");
    if (!file)
        return 0;
    size_t read = fread(reloaded, 1, PACKED_CAPACITY, file);
    int read_error = ferror(file);
    return !fclose(file) && !read_error && read == length;
}

static void print_hex(const char *label, const uint8_t *input, size_t length) {
    static const char digits[] = "0123456789abcdef";
    char hex[2 * PACKED_CAPACITY + 1];
    for (size_t i = 0; i < length; i++) {
        hex[2 * i] = digits[input[i] >> 4];
        hex[2 * i + 1] = digits[input[i] & 0xf];
    }
    hex[2 * length] = '\0';
    printf("%s=%s\n", label, hex);
}

int main(void) {
    uint8_t raw[RAW_CAPACITY], decoded[RAW_CAPACITY];
    uint8_t packed[PACKED_CAPACITY], reloaded[PACKED_CAPACITY];
    size_t raw_length = serialize(raw);
    size_t packed_length = pack(raw, raw_length, packed);

    if (!store_and_reload(packed, packed_length, reloaded) ||
        memcmp(reloaded, packed, packed_length)) {
        puts("codec file round trip failed");
        return 1;
    }
    if (unpack(reloaded, packed_length, decoded, raw_length) != DECODE_OK ||
        memcmp(raw, decoded, raw_length) || !records_match(decoded, raw_length)) {
        puts("codec record round trip failed");
        return 2;
    }

    const uint8_t short_stream[] = {0, 'a'};
    if (unpack(reloaded, packed_length - 1, decoded, raw_length) != DECODE_TRUNCATED ||
        unpack(reloaded, packed_length, decoded, raw_length - 1) != DECODE_OVERFLOW ||
        unpack(short_stream, sizeof short_stream, decoded, raw_length) != DECODE_LENGTH ||
        records_match(raw, raw_length - 1)) {
        puts("codec malformed input accepted");
        return 3;
    }
    print_hex("records", raw, raw_length);
    print_hex("packed", reloaded, packed_length);
    puts("controls=truncated,overflow,length,record");
    return 0;
}
