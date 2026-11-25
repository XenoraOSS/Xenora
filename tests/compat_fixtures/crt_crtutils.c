// SPDX-License-Identifier: GPL-3.0-or-later
//
// Hosted checks for the small CRT utilities (the survey residue of #459).
// Built with mingw-w64, every call is an import of ucrtbase. A wrong check
// prints its number and the program exits with it. With CRT_CRTUTILS_BREAK
// one expectation is wrong on purpose.

#include <ctype.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>

static unsigned checks;
static unsigned failed;
static unsigned failures;

#define CHECK(note, expr)                                                \
    do {                                                                 \
        checks++;                                                        \
        if (!(expr)) {                                                   \
            if (!failed)                                                 \
                failed = checks;                                         \
            failures++;                                                  \
            printf("check %u failed: %s\n", checks, note);              \
        }                                                                \
    } while (0)

int main(void) {
    char buffer[64];

    // 1. strrchr finds the last occurrence and the terminator counts.
    CHECK("strrchr", strrchr("a.b.c", '.') != NULL && *strrchr("a.b.c", '.') == '.');
    CHECK("strrchr missing", strrchr("a.b.c", 'z') == NULL);

    // 2. _strdup copies into owned memory.
    char *copy = _strdup("owned");
    CHECK("strdup", copy != NULL && strcmp(copy, "owned") == 0);
    free(copy);

    // 3. Case conversion and reversal in place.
    strcpy(buffer, "AbC");
    CHECK("strlwr", strcmp(_strlwr(buffer), "abc") == 0);
    CHECK("strupr", strcmp(_strupr(buffer), "ABC") == 0);
    strcpy(buffer, "abcde");
    CHECK("strrev", strcmp(_strrev(buffer), "edcba") == 0);

    // 4. Collation is byte-wise in the C locale.
    CHECK("strcoll", strcoll("abc", "abd") < 0 && strcoll("abc", "abc") == 0);
    size_t length = strxfrm(buffer, "abc", 16);
    CHECK("strxfrm", length == 3 && strcmp(buffer, "abc") == 0);
    CHECK("strxfrm query", strxfrm(buffer, "abcde", 0) == 5);

    // 5. The Annex K copies enforce their constraints.
    errno = 0;
    CHECK("memcpy_s", memcpy_s(buffer, 16, "source", 7) == 0 && strcmp(buffer, "source") == 0);
    memset(buffer, 1, 16);
    errno = 0;
    CHECK("memcpy_s range",
          memcpy_s(buffer, 8, "source", 12) == ERANGE && buffer[0] == 0 && errno == ERANGE);
    errno = 0;
    CHECK("memcpy_s null", memcpy_s(NULL, 8, "source", 4) == EINVAL && errno == EINVAL);

    // 6. The missing character classes.
    CHECK("isblank", isblank(' ') && isblank('\t') && !isblank('\n'));
    CHECK("isgraph", isgraph('!') && !isgraph(' ') && !isgraph(0x7f));

    // 7. The integer formatting family.
    CHECK("itoa decimal", strcmp(_itoa(-42, buffer, 10), "-42") == 0);
    CHECK("itoa hex", strcmp(_itoa(255, buffer, 16), "ff") == 0);
    CHECK("itoa binary", strcmp(_itoa(5, buffer, 2), "101") == 0);
    CHECK("itoa hex negative", strcmp(_itoa(-1, buffer, 16), "ffffffff") == 0);
    CHECK("i64toa", strcmp(_i64toa(-9000000000LL, buffer, 10), "-9000000000") == 0);
    CHECK("ultoa", strcmp(_ultoa(4000000000UL, buffer, 10), "4000000000") == 0);

    // 8. The _s form enforces the buffer size.
    errno = 0;
    CHECK("itoa_s small", _itoa_s(12345, buffer, 4, 10) == EINVAL && errno == EINVAL);
    CHECK("itoa_s fits", _itoa_s(12345, buffer, 8, 10) == 0 && strcmp(buffer, "12345") == 0);

    // 9. getenv_s over the empty environment is the documented empty.
    size_t needed = 99;
    errno = 0;
    CHECK("getenv_s", getenv_s(&needed, buffer, 16, "PATH") == 0 && needed == 0);

#ifdef CRT_CRTUTILS_BREAK
    CHECK("the control fails", length == 999);
#endif

    if (failures) {
        printf("crt-crtutils: %u/%u checks failed, first %u\n", failures, checks, failed);
        return failed;
    }
    printf("crt-crtutils: %u/%u checks passed\n", checks, checks);
    return 0;
}
