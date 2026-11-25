// SPDX-License-Identifier: GPL-3.0-or-later
//
// An original C program with no C library linked that reads what its process
// was started with, as a C runtime's startup code does: the startup
// information and the command line, narrow and wide.
//
// It splits the command line by the rules Microsoft documents in "Parsing C
// command-line arguments", after checking its splitter against the six examples
// on that page, and leaves what it found in files for the test. The structure
// is declared here from the documented field list, not taken from a header.
//
// Build with tests/compat_fixtures/build-startup-info.sh. The test that runs
// it is crates/loader/tests/startup_info.rs.

typedef unsigned long long u64;
typedef unsigned int u32;
typedef unsigned short u16;
typedef unsigned char u8;

typedef struct {
    u32 cb;
    void *reserved;
    void *desktop;
    void *title;
    u32 x, y, x_size, y_size, x_count_chars, y_count_chars, fill_attribute, flags;
    u16 show_window;
    u16 reserved_size;
    u8 *reserved_bytes;
    void *std_input, *std_output, *std_error;
} STARTUP_INFO;

#define IMPORT __declspec(dllimport)
IMPORT void GetStartupInfoA(STARTUP_INFO *info);
IMPORT void GetStartupInfoW(STARTUP_INFO *info);
IMPORT char *GetCommandLineA(void);
IMPORT u16 *GetCommandLineW(void);
IMPORT void *CreateFileW(const u16 *name, u32 access, u32 share, void *security,
                         u32 disposition, u32 flags, void *template_file);
IMPORT int WriteFile(void *file, const void *data, u32 size, u32 *written, void *overlapped);
IMPORT int CloseHandle(void *object);
IMPORT void ExitProcess(u32 status);

// The status of a run in which every check passed.
#define STATUS 0x5157u
#ifdef STARTUP_BREAK
// Control: the wrong size, so the first check must fail.
#define EXPECTED_SIZE 103
#else
#define EXPECTED_SIZE 104
#endif

static u32 checks;

static void check(int holds) {
    checks++;
    if (!holds) ExitProcess(checks);
}

static void save(const u16 *name, const void *data, u64 size) {
    u32 written = 0;
    void *file = CreateFileW(name, 0x40000000u, 0, 0, 2, 0, 0);
    check(file != (void *)-1);
    check(WriteFile(file, data, (u32)size, &written, 0) && written == size);
    check(CloseHandle(file));
}

// Split a command line. Each argument is stored with a terminating zero.
static u64 split(const u16 *line, int with_program, u16 *out, u64 *count) {
    u64 at = 0;
    *count = 0;
    if (with_program) {
        int quoted = 0;
        for (; *line && (quoted || (*line != ' ' && *line != '\t')); line++) {
            if (*line == '"') quoted = !quoted;
            else out[at++] = *line;
        }
        out[at++] = 0;
        ++*count;
    }
    for (;;) {
        int quoted = 0;
        while (*line == ' ' || *line == '\t') line++;
        if (!*line) return at;
        while (*line && (quoted || (*line != ' ' && *line != '\t'))) {
            u64 slashes = 0;
            while (*line == '\\') {
                slashes++;
                line++;
            }
            if (*line != '"') {
                while (slashes--) out[at++] = '\\';
                if (*line && (quoted || (*line != ' ' && *line != '\t'))) out[at++] = *line++;
                continue;
            }
            for (u64 i = 0; i < slashes / 2; i++) out[at++] = '\\';
            line++;
            if (slashes & 1) {
                out[at++] = '"';
            } else if (quoted && *line == '"') {
                out[at++] = '"';
                line++;
            } else {
                quoted = !quoted;
            }
        }
        out[at++] = 0;
        ++*count;
    }
}

static int same(const u16 *left, const u16 *right, u64 count) {
    for (u64 i = 0; i < count; i++)
        if (left[i] != right[i]) return 0;
    return 1;
}

static u16 buffer[0x8000];
static struct {
    STARTUP_INFO info;
    u8 guard[16];
} wide, again, narrow;

static void fill(void *block, u64 size) {
    volatile u8 *bytes = block;
    for (u64 i = 0; i < size; i++) bytes[i] = 0xCC;
}

#define EXAMPLE(input, count_, expected)                                            \
    do {                                                                            \
        static const u16 in[] = input, want[] = expected;                           \
        u64 count;                                                                  \
        u64 used = split(in, 0, buffer, &count);                                    \
        check(count == (count_) && used == sizeof want / 2 && same(buffer, want, used)); \
    } while (0)

static u64 length_wide(const u16 *text) {
    u64 n = 0;
    while (text[n]) n++;
    return n + 1;
}

static u64 length_narrow(const char *text) {
    u64 n = 0;
    while (text[n]) n++;
    return n + 1;
}

void start(void) {
    fill(&wide, sizeof wide);
    fill(&again, sizeof again);
    fill(&narrow, sizeof narrow);
    GetStartupInfoW(&wide.info);
    check(sizeof wide.info == EXPECTED_SIZE);
    check(wide.info.cb == EXPECTED_SIZE);
    check(!wide.info.reserved && !wide.info.reserved_size && !wide.info.reserved_bytes);
    GetStartupInfoW(&again.info);
    GetStartupInfoA(&narrow.info);
    check(narrow.info.cb == EXPECTED_SIZE);
    for (int i = 0; i < 16; i++)
        check(wide.guard[i] == 0xCC && narrow.guard[i] == 0xCC);
    for (u64 i = 0; i < sizeof wide.info; i++)
        check(((u8 *)&wide.info)[i] == ((u8 *)&again.info)[i]);

    EXAMPLE(u"\"a b c\" d e", 3, u"a b c\0d\0e");
    EXAMPLE(u"\"ab\\\"c\" \"\\\\\" d", 3, u"ab\"c\0\\\0d");
    EXAMPLE(u"a\\\\\\b d\"e f\"g h", 3, u"a\\\\\\b\0de fg\0h");
    EXAMPLE(u"a\\\\\\\"b c d", 3, u"a\\\"b\0c\0d");
    EXAMPLE(u"a\\\\\\\\\"b c\" d e", 3, u"a\\\\b c\0d\0e");
    EXAMPLE(u"a\"b\"\" c d", 1, u"ab\" c d");

    u16 *line = GetCommandLineW();
    char *bytes = GetCommandLineA();
    check(line == GetCommandLineW());
    check(bytes == GetCommandLineA());
    save(u"C:\\line-w.bin", line, 2 * length_wide(line));
    save(u"C:\\line-a.bin", bytes, length_narrow(bytes));
    u64 count;
    u64 used = split(line, 1, buffer, &count);
    save(u"C:\\arguments.bin", buffer, 2 * used);
    save(u"C:\\info-w.bin", &wide.info, sizeof wide.info);
    save(u"C:\\info-a.bin", &narrow.info, sizeof narrow.info);
    if (wide.info.desktop) {
        save(u"C:\\desktop-w.bin", wide.info.desktop, 2 * length_wide(wide.info.desktop));
        save(u"C:\\desktop-a.bin", narrow.info.desktop, length_narrow(narrow.info.desktop));
    }
    if (wide.info.title) {
        save(u"C:\\title-w.bin", wide.info.title, 2 * length_wide(wide.info.title));
        save(u"C:\\title-a.bin", narrow.info.title, length_narrow(narrow.info.title));
    }
    ExitProcess(STATUS);
}
