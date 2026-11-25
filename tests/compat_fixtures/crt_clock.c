// SPDX-License-Identifier: GPL-3.0-or-later
//
// A hosted C program for package 5 of #459: the C time services. The
// expectations are the civil calendar itself and the Microsoft pages.
// Prints how many checks passed and returns the number of the first
// failure, or 0.
#include <time.h>
#include <stdio.h>
#include <string.h>

static unsigned passed, total, first;

static void check(int ok, const char *what) {
    total++;
    if (ok) {
        passed++;
    } else {
        if (!first) first = total;
        printf("check %u failed: %s\n", total, what);
    }
}

int main(void) {
    // 1,000,000,000 seconds after the epoch: 2001-09-09 01:46:40 UTC, a
    // Sunday.
    time_t stamp = 1000000000;
    struct tm *tm = gmtime(&stamp);
    check(tm && tm->tm_sec == 40 && tm->tm_min == 46 && tm->tm_hour == 1, "time of day");
    check(tm->tm_mday == 9 && tm->tm_mon == 8 && tm->tm_year == 101, "date");
    check(tm->tm_wday == 0 && tm->tm_yday == 251, "weekday and year day");
    check(tm->tm_isdst == 0, "no daylight saving");

    check(localtime(&stamp) == tm, "localtime shares the UTC static buffer");

    // The CRT string forms.
    check(strcmp(asctime(tm), "Sun Sep  9 01:46:40 2001\n") == 0, "asctime");
    check(strcmp(ctime(&stamp), "Sun Sep  9 01:46:40 2001\n") == 0, "ctime");

    char buffer[64];
    check(asctime_s(buffer, sizeof buffer, tm) == 0, "asctime_s");
    check(strcmp(buffer, "Sun Sep  9 01:46:40 2001\n") == 0, "asctime_s string");
    check(asctime_s(buffer, 10, tm) != 0 && buffer[0] == 0, "asctime_s small buffer empties");
    check(ctime_s(buffer, sizeof buffer, &stamp) == 0, "ctime_s");

    // mktime composes what gmtime breaks down, normalizing as it goes.
    struct tm copy = *tm;
    copy.tm_wday = copy.tm_yday = 0;
    check(mktime(&copy) == stamp, "mktime round trip");
    check(copy.tm_wday == 0 && copy.tm_yday == 251, "mktime fills the weekday");
    copy.tm_year = 69;
    check(mktime(&copy) == (time_t)-1, "1969 is out of range");

    // strftime for the C locale.
    char text[128];
    tm = gmtime(&stamp);
    size_t len = strftime(text, sizeof text, "%Y-%m-%d %H:%M:%S %a %A %b %B %j %w %p %I %x %X %y %%", tm);
    check(len > 0 && strcmp(text, "2001-09-09 01:46:40 Sun Sunday Sep September 252 0 AM 01 09/09/01 01:46:40 01 %") == 0, "strftime");
    check(strftime(text, 4, "%Y-%m-%d", tm) == 0, "strftime too small is 0");

    check(difftime(stamp, stamp - 500) == 500.0, "difftime");
    check(clock() != (clock_t)-1, "clock");

#ifdef CRT_CLOCK_BREAK
    check(1 == 2, "control");
#endif
    printf("crt_clock: %u of %u checks passed\n", passed, total);
    return (int)first;
}
