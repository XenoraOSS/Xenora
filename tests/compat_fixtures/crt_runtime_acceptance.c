// SPDX-License-Identifier: GPL-3.0-or-later

#include <errno.h>
#include <limits.h>
#include <locale.h>
#include <process.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>
#include <wctype.h>
#include <windows.h>

static int check_conversion(void) {
    const wchar_t invalid[] = {L'A', 0x100, 0};
    char text[] = {'?', '?', '?'};

    errno = EDOM;
    if (wcstombs(text, invalid, 1) != 1 || text[0] != 'A' ||
        text[1] != '?' || text[2] != '?' || errno != EDOM)
        return 21;
    text[0] = '?';
    if (wcstombs(text, invalid + 1, 0) != 0 || memcmp(text, "???", 3) != 0 || errno != EDOM)
        return 22;

    if (wcstombs(text, invalid, 2) != (size_t)-1 || errno != EILSEQ)
        return 23;
    errno = 0;
    if (wcstombs(NULL, invalid, 0) != (size_t)-1 || errno != EILSEQ)
        return 24;

    wchar_t wide[3];
    if (mbstowcs(wide, "Ok", 3) != 2 || wide[0] != L'O' ||
        wide[1] != L'k' || wide[2] != 0)
        return 25;
    if (wcstombs(NULL, wide, 0) != 2)
        return 26;
    text[2] = '?';
    if (wcstombs(text, wide, 2) != 2 || memcmp(text, "Ok?", 3) != 0)
        return 27;
    if (wcstombs(text, wide, sizeof text) != 2 || memcmp(text, "Ok", 3) != 0)
        return 28;

    puts("conversion: limits, terminator and EILSEQ");
    return 0;
}

static unsigned __stdcall errno_worker(void *parent_errno) {
    if (_errno() == parent_errno)
        return 1;
    errno = 0;
    const wchar_t input[] = L"2147483648!";
    wchar_t *end = NULL;
    if (wcstol(input, &end, 10) != LONG_MAX || end != input + 10 || errno != ERANGE)
        return 2;
    return 17;
}

static int check_heap_locale_threads(void) {
    unsigned char *bytes = calloc(4, 1);
    if (bytes == NULL)
        return 30;
    if (memcmp(bytes, "\0\0\0\0", 4) != 0) {
        free(bytes);
        return 31;
    }
    memcpy(bytes, "heap", 4);
    unsigned char *grown = realloc(bytes, 16);
    if (grown == NULL) {
        free(bytes);
        return 32;
    }
    const int kept = memcmp(grown, "heap", 4) == 0;
    free(grown);
    if (!kept)
        return 33;

    const wchar_t number[] = L" \t-17!";
    wchar_t *end = NULL;
#ifdef WRONG_EXPECTATION
    const long expected = -18;
#else
    const long expected = -17;
#endif
    if (wcstol(number, &end, 10) != expected || end != number + 5)
        return 34;
    if (!iswalpha(L'a') || iswalpha(L'1') || towupper(L'x') != L'X')
        return 35;
    if (strcmp(localeconv()->decimal_point, ".") != 0)
        return 36;

    if (_set_errno(EDOM) != 0)
        return 37;
    HANDLE worker = (HANDLE)_beginthreadex(NULL, 0, errno_worker, _errno(), 0, NULL);
    if (!worker)
        return 38;
    DWORD status = 0;
    const int joined = WaitForSingleObject(worker, INFINITE) == WAIT_OBJECT_0 &&
                       GetExitCodeThread(worker, &status);
    const int closed = CloseHandle(worker);
    int error = 0;
    if (!joined || !closed || status != 17 || _get_errno(&error) != 0 || error != EDOM)
        return 39;

    puts("runtime: heap, wide classes, locale and thread errno");
    return 0;
}

static int check_files(void) {
    errno = 0;
    FILE *file = fopen("missing-runtime-acceptance.txt", "rb");
    if (file != NULL) {
        fclose(file);
        return 40;
    }
    if (errno != ENOENT)
        return 41;

    file = fopen("runtime-acceptance.txt", "wb");
    if (file == NULL)
        return 42;
    const int wrote = fputs("A=17\n", file);
    if (fclose(file) != 0 || wrote == EOF)
        return 43;

    file = fopen("runtime-acceptance.txt", "rb");
    if (file == NULL)
        return 44;
    char line[16];
    const int read_back = fgets(line, sizeof line, file) != NULL &&
                          strcmp(line, "A=17\n") == 0 &&
                          fgets(line, sizeof line, file) == NULL &&
                          feof(file) && !ferror(file);
    if (fclose(file) != 0 || !read_back)
        return 45;

    puts("files: ENOENT, exact bytes and EOF");
    return 0;
}

static void first_registered(void) { puts("exit: first registration"); }
static void second_registered(void) { puts("exit: second registration"); }

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "conversion") == 0)
        return check_conversion();

    if (atexit(first_registered) != 0 || atexit(second_registered) != 0)
        return 10;
    const int runtime_error = check_heap_locale_threads();
    if (runtime_error != 0)
        return runtime_error;
    const int file_error = check_files();
    if (file_error != 0)
        return file_error;
    return check_conversion();
}
