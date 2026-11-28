// SPDX-License-Identifier: GPL-3.0-or-later

#include <errno.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static const char report_variable[] = "XENORA_RECORD_LABEL";
static const char input_path[] = "crt-record-input.txt";
static const char report_path[] = "crt-record-report.txt";

static int path_is_absent(const char *path) {
    errno = 0;
    FILE *stream = fopen(path, "rb");
    if (stream) {
        fclose(stream);
        return 0;
    }
    return errno == ENOENT;
}

int main(int argc, char **argv) {
    if (argc != 2)
        return 10;
    const int malformed = strcmp(argv[1], "malformed") == 0;
    const int replay = strcmp(argv[1], "replay") == 0;
    if (!malformed && !replay && strcmp(argv[1], "initial") != 0)
        return 11;

    char label[16] = {'?'};
    size_t required = 99;
    if (getenv_s(&required, label, sizeof label, report_variable) != 0 ||
        required != 0 || label[0] != '\0')
        return 12;
    const char *assignment = malformed ? "XENORA_RECORD_LABEL=reject"
        : replay ? "XENORA_RECORD_LABEL=beta" : "XENORA_RECORD_LABEL=alpha";
    const char *expected_label = malformed ? "reject" : replay ? "beta" : "alpha";
    if (_putenv(assignment) != 0 ||
        getenv_s(&required, label, sizeof label, report_variable) != 0 ||
        required != strlen(expected_label) + 1 || strcmp(label, expected_label) != 0)
        return 13;

    // The same guest paths are used on every run. Creating these must not
    // change files owned by a previously completed service state.
    if (!path_is_absent(input_path))
        return 26;
    if (!path_is_absent(report_path))
        return 27;
    FILE *stream = fopen(input_path, "wb");
    if (!stream)
        return 14;
    const char *input = malformed ? "invalid 17.25\n"
        : replay ? "1709164800 5.50\n" : "1709164800 17.25\n";
    const int wrote = fputs(input, stream);
    if (fclose(stream) != 0 || wrote == EOF)
        return 15;
    stream = fopen(input_path, "rb");
    if (!stream)
        return 16;

    unsigned epoch = 99;
    double amount = -1.0;
    const int fields = fscanf(stream, "%u %lf", &epoch, &amount);
    const int input_error = ferror(stream);
    if (fclose(stream) != 0)
        return 17;
    if (malformed) {
        if (fields != 0 || epoch != 99 || amount != -1.0 || input_error)
            return 18;
        puts("records: rejected malformed input");
        return 23;
    }
    if (fields != 2 || input_error || epoch != 1709164800u ||
        amount != (replay ? 5.50 : 17.25))
        return 19;

    double whole = -1.0;
    const double fraction = modf(amount, &whole);
    if (whole != (replay ? 5.0 : 17.0) || fraction != (replay ? 0.50 : 0.25))
        return 20;

    const __time64_t timestamp = epoch;
    const struct tm *date = _gmtime64(&timestamp);
    if (!date || date->tm_year != 124 || date->tm_mon != 1 ||
        date->tm_mday != 29 || date->tm_wday != 4 || date->tm_yday != 59 ||
        date->tm_hour != 0 || date->tm_min != 0 || date->tm_sec != 0)
        return 21;
    char formatted[20];
    if (strftime(formatted, sizeof formatted, "%Y-%m-%d %H:%M:%S", date) != 19 ||
        strcmp(formatted, "2024-02-29 00:00:00") != 0)
        return 22;

    stream = fopen(report_path, "wb");
    if (!stream)
        return 24;
    const int length = fprintf(stream, "%s|%s|%.2f|%.2f\n", label, formatted, whole, fraction);
    if (fclose(stream) != 0 || length != (replay ? 35 : 37))
        return 25;
    printf("records: %s\n", label);
    return 0;
}
