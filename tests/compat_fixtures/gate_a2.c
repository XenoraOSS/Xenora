/* SPDX-License-Identifier: GPL-3.0-or-later */

/*
 * Gate A2 (issue #419): a hosted C program. Original code written for Xenora.
 *
 * It is built with a real C runtime's startup code, which runs before main,
 * and uses the standard C library the way an ordinary program does:
 * command-line arguments, heap memory, sorting with a callback, formatted
 * output, a file written and read back, a function registered with atexit,
 * and a normal return from main.
 *
 * Every result is checked by the program itself. The exit status is 0 when
 * every check passed and otherwise names the first one that failed, so a
 * runtime that computes something wrongly cannot report success.
 *
 * It needs no floating point and no input.
 *
 * Control variants, selected at build time, each end in a different way so
 * that the test can tell a real pass from a runtime that reports success for
 * everything:
 *   GATE_BREAK  expects a wrong total, so check 22 fails
 *   GATE_EXIT   calls exit(42) half way, without returning from main
 *   GATE_ABORT  calls abort() half way
 *
 * One more variant is not a control of the gate. `xenora run` uses it to test
 * its wall deadline (issue #448):
 *   GATE_SPIN   prints its arguments, counts to the first one and ends;
 *               with 0 it never ends
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define COUNT 48

static unsigned checks_passed;
static const char *report_name = "gate-a2-report.txt";

static void say_goodbye(void) {
    /* Runs from exit, after main has returned. */
    printf("atexit: %u checks passed\n", checks_passed);
}

static int by_value(const void *left, const void *right) {
    unsigned a = *(const unsigned *)left, b = *(const unsigned *)right;
    return (a > b) - (a < b);
}

/* A linear congruential generator, so the data does not depend on a library. */
static unsigned next_value(unsigned *state) {
    *state = *state * 1103515245u + 12345u;
    return (*state >> 8) & 0xffffu;
}

static unsigned long long checksum(const unsigned *values, size_t count) {
    unsigned long long sum = 14695981039346656037ull; /* FNV-1a, 64-bit */
    for (size_t i = 0; i < count; i++) {
        for (int shift = 0; shift < 32; shift += 8) {
            sum ^= (values[i] >> shift) & 0xffu;
            sum *= 1099511628211ull;
        }
    }
    return sum;
}

#define CHECK(code, condition) \
    do { \
        if (!(condition)) { \
            fprintf(stderr, "gate a2: check %d failed\n", (code)); \
            return (code); \
        } \
        checks_passed++; \
    } while (0)

int main(int argc, char **argv) {
    CHECK(10, atexit(say_goodbye) == 0);

    /* Arguments: a program name, a seed and a label. */
    printf("gate a2: %d arguments\n", argc);
    for (int i = 0; i < argc; i++) printf("  argv[%d] = \"%s\"\n", i, argv[i]);
#ifdef GATE_SPIN
    /* Count to the first argument, then end. With 0 the count is never
       reached, and the program runs until something outside stops it. */
    unsigned long long limit = argc > 1 ? strtoull(argv[1], NULL, 10) : 0;
    fprintf(stderr, "gate a2: counting to %llu\n", limit);
    volatile unsigned long long turns = 0;
    do {
        turns = turns + 1;
    } while (turns != limit);
    printf("gate a2: %llu turns\n", turns);
    return 0;
#endif
    CHECK(11, argc == 3);
    CHECK(12, argv[argc] == NULL);
    char *end = NULL;
    unsigned long seed = strtoul(argv[1], &end, 10);
    CHECK(13, end != argv[1] && *end == '\0');
    const char *label = argv[2];
    CHECK(14, strlen(label) > 0 && strlen(label) < 32);

    /* Heap memory, filled, sorted through a callback and verified. */
    unsigned *values = malloc(COUNT * sizeof *values);
    CHECK(20, values != NULL);
    unsigned state = (unsigned)seed;
    unsigned long long total = 0;
    for (size_t i = 0; i < COUNT; i++) {
        values[i] = next_value(&state);
        total += values[i];
    }
    unsigned long long before = checksum(values, COUNT);
    qsort(values, COUNT, sizeof *values, by_value);
    unsigned long long sorted_total = 0;
    for (size_t i = 0; i < COUNT; i++) {
        if (i > 0) CHECK(21, values[i - 1] <= values[i]);
        sorted_total += values[i];
    }
#ifdef GATE_BREAK
    CHECK(22, sorted_total == total + 1);
#else
    CHECK(22, sorted_total == total);
#endif
    unsigned long long after = checksum(values, COUNT);
    printf("sorted %d values: min %u, max %u, total %llu\n",
           COUNT, values[0], values[COUNT - 1], total);
    printf("checksum before %016llx, after %016llx\n", before, after);

#ifdef GATE_EXIT
    /* The registered function still runs, and nothing after this does. */
    exit(42);
#endif
#ifdef GATE_ABORT
    /* Nothing registered runs, and the status is abort's. */
    abort();
#endif

    /* A file written with formatted output. */
    FILE *report = fopen(report_name, "w");
    if (report == NULL) {
        perror(report_name);
        return 30;
    }
    checks_passed++;
    CHECK(31, fprintf(report, "label %s\n", label) > 0);
    CHECK(32, fprintf(report, "total %llu\n", total) > 0);
    CHECK(33, fprintf(report, "range %u %u\n", values[0], values[COUNT - 1]) > 0);
    CHECK(34, fputs("end\n", report) >= 0);
    CHECK(35, fclose(report) == 0);

    /* The same file read back line by line and compared. */
    report = fopen(report_name, "r");
    CHECK(40, report != NULL);
    char line[96];
    CHECK(41, fgets(line, sizeof line, report) != NULL);
    CHECK(42, strncmp(line, "label ", 6) == 0);
    CHECK(43, strlen(line) == 6 + strlen(label) + 1);
    CHECK(44, strncmp(line + 6, label, strlen(label)) == 0 && line[strlen(line) - 1] == '\n');
    CHECK(45, fgets(line, sizeof line, report) != NULL);
    CHECK(46, strncmp(line, "total ", 6) == 0);
    CHECK(47, strtoull(line + 6, &end, 10) == total && *end == '\n');
    CHECK(48, fgets(line, sizeof line, report) != NULL);
    CHECK(49, strncmp(line, "range ", 6) == 0);
    CHECK(50, strtoul(line + 6, &end, 10) == values[0] && *end == ' ');
    CHECK(51, strtoul(end, &end, 10) == values[COUNT - 1] && *end == '\n');
    CHECK(52, fgets(line, sizeof line, report) != NULL && strcmp(line, "end\n") == 0);
    CHECK(53, fgets(line, sizeof line, report) == NULL);
    CHECK(54, fclose(report) == 0);

    /* A file that does not exist is reported, not opened. */
    CHECK(60, fopen("gate-a2-missing.txt", "r") == NULL);

    free(values);
    printf("report %s read back and verified\n", report_name);
    return 0;
}
