/* SPDX-License-Identifier: GPL-3.0-or-later */

struct tm {
    int tm_sec, tm_min, tm_hour, tm_mday, tm_mon, tm_year;
    int tm_wday, tm_yday, tm_isdst;
};

typedef __SIZE_TYPE__ size_t;
__declspec(dllimport) size_t strftime(char *, size_t, const char *, const struct tm *);
__declspec(dllimport) __declspec(noreturn) void ExitProcess(unsigned int);

void entry(void) {
    /* Wednesday, 3 February 2021, 04:05:06. */
    const struct tm time = {6, 5, 4, 3, 1, 121, 3, 33, 0};
#if CASE_LITERAL
    const char format[] = "x\xc3\xa9!%S";
    const unsigned char expected[] = {'x', 0xc3, 0xa9, '!', '0', '6', 0};
    const unsigned int failure = 10;
#else
    const char format[] = "%D|%F|%R|%T%n%t%%";
    const unsigned char expected[] = "02/03/21|2021-02-03|04:05|04:05:06\n\t%";
    const unsigned int failure = 20;
#endif
    unsigned char output[sizeof(expected) + 1];
    for (size_t i = 0; i < sizeof(output); ++i)
        output[i] = 0x5a;

    size_t count = strftime((char *)output, sizeof(expected), format, &time);
    if (count != sizeof(expected) - 1)
        ExitProcess(failure + 1);
    for (size_t i = 0; i < sizeof(expected); ++i)
        if (output[i] != expected[i])
            ExitProcess(failure + 2);
    if (output[sizeof(expected)] != 0x5a)
        ExitProcess(failure + 3);
    if (strftime((char *)output, sizeof(expected) - 1, format, &time) != 0)
        ExitProcess(failure + 4);

#if !CASE_LITERAL
    /* The synthetic formatter accepts short years. %F must agree with %Y. */
    struct tm short_year = time;
    short_year.tm_year = -1899;
    const char equivalent[] = "1-02-03|1-02-03";
    count = strftime((char *)output, sizeof(equivalent), "%F|%Y-%m-%d", &short_year);
    if (count != sizeof(equivalent) - 1)
        ExitProcess(failure + 5);
    for (size_t i = 0; i < sizeof(equivalent); ++i)
        if (output[i] != (unsigned char)equivalent[i])
            ExitProcess(failure + 6);
#endif

    ExitProcess(0);
}
