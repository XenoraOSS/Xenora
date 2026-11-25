/* SPDX-License-Identifier: GPL-3.0-or-later */

/*
 * A hosted C++ program for Xenora (issue #437). Original code written for
 * Xenora.
 *
 * It is built with a real C++ runtime's startup code, which runs before main,
 * and it uses the parts of the C++ runtime Xenora supports: objects with
 * constructors and destructors, a function-local static that is built on
 * first use, virtual dispatch through a base class pointer, the standard
 * containers and algorithms, and thread-local storage. It also uses the
 * guest runtime's threads, files and standard streams.
 *
 * The program prints ten lines to standard output, in this order:
 *
 *   guest cpp: ctor 1      a namespace scope object was constructed
 *   guest cpp: lazy 1      a function-local static was built on first use
 *   guest cpp: areas 9 25   virtual dispatch through a base class pointer
 *   guest cpp: sorted 1 2 3 4 5   a vector sorted with a comparator
 *   guest cpp: len 5 cmp 0  a string's length and a comparison of two strings
 *   guest cpp: fn 7 8       two calls through a table of function pointers
 *   guest cpp: tls 7        a thread-local variable read in the main thread
 *   guest cpp: worker 7     a second thread, and its exit code
 *   guest cpp: file alpha   a line read back from a file the program wrote
 *   guest cpp: dtor 1       the namespace scope object was destroyed
 *
 * Every result is checked by the program itself. A failed check writes one
 * line to standard error and returns a nonzero status, so a runtime that
 * computes something wrongly cannot report success. Standard error is empty
 * on a successful run.
 *
 * The program needs no floating point and no input. It does not use
 * exceptions, RTTI, std::thread or the array allocation operators: the guest
 * runtime provides only the ordinary ones.
 */
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX

#include <windows.h>

#include <algorithm>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

namespace {

/* The number of constructions of the namespace scope object. */
int g_constructions = 0;

/* The number of constructions of the function-local static. */
int g_lazy_constructions = 0;

/*
 * A namespace scope object. Its constructor runs before main and its
 * destructor runs at shutdown, after main has returned.
 */
class Global {
public:
    Global() {
        g_constructions++;
        std::printf("guest cpp: ctor %d\n", g_constructions);
        std::fflush(stdout);
    }

    ~Global() {
        std::printf("guest cpp: dtor %d\n", g_constructions);
        std::fflush(stdout);
    }
};

/* The namespace scope object. It is constructed before main. */
Global g_global;

/*
 * A class whose constructor counts its own constructions, so a program can
 * tell a static that was built once from one that was built twice.
 */
class Lazy {
public:
    Lazy() { g_lazy_constructions++; }
};

/*
 * The function-local static. It is built when the function is called for the
 * first time, and not before.
 */
const Lazy &lazy() {
    static Lazy instance;
    return instance;
}

/* A shape with an area, dispatched virtually. */
class Shape {
public:
    virtual ~Shape() {}
    virtual int area() const = 0;
};

/* A square, a shape with a side. */
class Square : public Shape {
public:
    explicit Square(int side) : side_(side) {}
    int area() const { return side_ * side_; }

private:
    int side_;
};

/* The comparator the vector is sorted with. */
struct Less {
    bool operator()(int left, int right) const { return left < right; }
};

/* Two functions with the same shape, called through a table. */
int seven(int) { return 7; }
int eight(int) { return 8; }

typedef int (*Operation)(int);

/* The table of function pointers. */
const Operation g_table[2] = { seven, eight };

/*
 * A thread-local variable. Every thread has its own copy: the main thread's
 * copy is set before the worker starts, and the worker's copy starts at
 * zero.
 */
thread_local int g_tls = 0;

/* The file the program writes and reads back. */
const char *const g_file_name = "C:\\guest-cpp.txt";

/*
 * A service no runtime service provides. The program references the symbol
 * and never calls it: the control executable links it through an import
 * library, so its import table holds a module the loader cannot bind, and the
 * loader refuses that image before the entry point runs. The four running
 * executables link a local definition of the same symbol instead.
 */
extern "C" void MissingService(void);

/* A check that names itself when it fails. */
bool check(int code, bool condition) {
    if (condition) {
        return true;
    }
    std::fprintf(stderr, "guest cpp: check %d failed\n", code);
    return false;
}

/*
 * The worker thread. It returns a nonzero exit code if its thread-local
 * copy of g_tls is not its own, which would mean the storage is shared, and
 * otherwise sets its own copy and returns it.
 */
DWORD WINAPI worker_main(LPVOID) {
    if (g_tls != 0) {
        /* The thread-local storage was shared with the main thread. */
        return 9;
    }
    g_tls = 7;
    return (DWORD)g_tls;
}

} /* namespace */

int main(void) {
    /* The reference to the service no runtime provides. It is never called. */
    void (*volatile reference)() = MissingService;
    (void)reference;

    /* The function-local static is built here, on first use. */
    lazy();
    if (!check(10, g_lazy_constructions == 1)) {
        return 10;
    }
    std::printf("guest cpp: lazy %d\n", g_lazy_constructions);
    std::fflush(stdout);

    /* Virtual dispatch through a base class pointer. */
    Shape *square3 = new Square(3);
    Shape *square5 = new Square(5);
    int area3 = square3->area();
    int area5 = square5->area();
    delete square3;
    delete square5;
    if (!check(20, area3 == 9 && area5 == 25)) {
        return 20;
    }
    std::printf("guest cpp: areas %d %d\n", area3, area5);
    std::fflush(stdout);

    /* A vector, sorted with a comparator. */
    std::vector<int> values;
    values.push_back(5);
    values.push_back(3);
    values.push_back(1);
    values.push_back(4);
    values.push_back(2);
    std::sort(values.begin(), values.end(), Less());
    if (!check(30, values.size() == 5)) {
        return 30;
    }
    for (size_t index = 0; index < values.size(); index++) {
        if (!check(31, values[index] == (int)index + 1)) {
            return 31;
        }
    }
    std::printf("guest cpp: sorted");
    for (size_t index = 0; index < values.size(); index++) {
        std::printf(" %d", values[index]);
    }
    std::printf("\n");
    std::fflush(stdout);

    /* A string, and a comparison of two equal strings. */
    std::string text("hello");
    size_t length = text.length();
    int compared = std::memcmp(text.c_str(), "hello", length);
    if (!check(40, length == 5)) {
        return 40;
    }
    if (!check(41, compared == 0)) {
        return 41;
    }
    std::printf("guest cpp: len %lu cmp %d\n", (unsigned long)length, compared);
    std::fflush(stdout);

    /* Two calls through a table of function pointers. */
    int first = g_table[0](1);
    int second = g_table[1](1);
    if (!check(50, first == 7 && second == 8)) {
        return 50;
    }
    std::printf("guest cpp: fn %d %d\n", first, second);
    std::fflush(stdout);

    /* A thread-local variable, in the main thread. */
    g_tls = 7;
    if (!check(60, g_tls == 7)) {
        return 60;
    }
    std::printf("guest cpp: tls %d\n", g_tls);
    std::fflush(stdout);

    /* A second thread, waited for, with its exit code read back. */
    HANDLE worker = CreateThread(NULL, 0, worker_main, NULL, 0, NULL);
    if (!check(70, worker != NULL)) {
        return 70;
    }
    DWORD wait = WaitForSingleObject(worker, INFINITE);
    if (!check(71, wait == WAIT_OBJECT_0)) {
        return 71;
    }
    DWORD exit_code = 0;
    if (!check(72, GetExitCodeThread(worker, &exit_code) != 0)) {
        return 72;
    }
    if (!check(73, CloseHandle(worker) != 0)) {
        return 73;
    }
    if (!check(74, exit_code == 7)) {
        return 74;
    }
    std::printf("guest cpp: worker %lu\n", (unsigned long)exit_code);
    std::fflush(stdout);

    /* A file written, closed, opened again and read back line by line. */
    FILE *file = std::fopen(g_file_name, "w");
    if (!check(80, file != NULL)) {
        return 80;
    }
    if (!check(81, std::fprintf(file, "alpha\nbeta\n") > 0)) {
        return 81;
    }
    if (!check(82, std::fclose(file) == 0)) {
        return 82;
    }
    file = std::fopen(g_file_name, "r");
    if (!check(83, file != NULL)) {
        return 83;
    }
    char line[64];
    if (!check(84, std::fgets(line, (int)sizeof line, file) != NULL)) {
        return 84;
    }
    if (!check(85, std::strcmp(line, "alpha\n") == 0)) {
        return 85;
    }
    if (!check(86, std::fgets(line, (int)sizeof line, file) != NULL)) {
        return 86;
    }
    if (!check(87, std::strcmp(line, "beta\n") == 0)) {
        return 87;
    }
    if (!check(88, std::fgets(line, (int)sizeof line, file) == NULL)) {
        return 88;
    }
    if (!check(89, std::fclose(file) == 0)) {
        return 89;
    }
    std::printf("guest cpp: file %s\n", "alpha");
    std::fflush(stdout);

    /* The namespace scope object's destructor prints the last line. */
    return 0;
}
