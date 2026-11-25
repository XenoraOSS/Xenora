// crt_stdin: read lines from the standard input with a small buffer and
// check what ISO C says about the edges (#467).
//
// The host pipes a fixed input: "alpha\r\nbeta\r\n0123456789\r\nlast" -
// two short lines in CR LF, one line longer than the buffer, and a last
// line with no newline. After the text-mode replacement of CR LF by LF
// the stream is "alpha\nbeta\n0123456789\nlast", 26 bytes, which fgets
// with a buffer of 8 reads as "alpha\n", "beta\n", "0123456", "789\n"
// and "last". With no input at all - the program's argument is then
// "empty" - every count is zero and end of file comes at once.
//
// Exit status: the first failing check, or zero when all passed.

#include <stdio.h>
#include <string.h>

static unsigned checks;
static unsigned failures;
static unsigned first_failed;

#define CHECK(name, condition)                       \
    do {                                             \
        checks++;                                    \
        if (!(condition)) {                          \
            failures++;                              \
            if (!first_failed) {                     \
                first_failed = checks;               \
            }                                        \
            printf("check %u failed: %s\n", checks, name); \
        }                                            \
    } while (0)

int main(int argc, char **argv) {
    int empty = argc > 1 && strcmp(argv[1], "empty") == 0;
    char line[8];
    unsigned reads = 0;
    unsigned bytes = 0;
    unsigned with_lf = 0;
    unsigned with_cr = 0;
    while (fgets(line, sizeof line, stdin)) {
        reads++;
        for (char *at = line; *at; at++) {
            bytes++;
            if (*at == '\n') {
                with_lf++;
            }
            if (*at == '\r') {
                with_cr++;
            }
        }
    }
    int eof = feof(stdin) != 0;
    int extra = getchar();

    if (empty) {
        CHECK("no reads without input", reads == 0);
        CHECK("no bytes without input", bytes == 0);
    } else {
        CHECK("five reads for the fixed input", reads == 5);
        // 29 bytes piped, less the three CR of the CR LF pairs.
        CHECK("26 bytes after the replacement", bytes == 26);
        CHECK("three line feeds", with_lf == 3);
        CHECK("no carriage return survives the replacement", with_cr == 0);
    }
    CHECK("end of file is indicated", eof);
    CHECK("getchar at the end is EOF", extra == EOF);
#ifdef CRT_STDIN_BREAK
    // The control: a wrong expectation that must be caught.
    CHECK("control", reads == (empty ? 1u : 4u));
#else
    CHECK("control", reads == (empty ? 0u : 5u));
#endif

    printf("crt_stdin: %u of %u checks passed\n", checks - failures, checks);
    return (int)first_failed;
}
