/* SPDX-License-Identifier: GPL-3.0-or-later */

/*
 * A hosted C program that calls the 49 math.h functions of issue #468 and the
 * four binary32 functions of issue #986 through the Universal C runtime.
 * Original code written for Xenora.
 *
 * Built with mingw-w64 (UCRT flavour) and -fno-builtin, every function is an
 * import of api-ms-win-crt-math-l1-1-0.dll, and the arguments are volatile so
 * the compiler cannot fold a call.
 *
 * Expected values are not produced by Xenora: the exact functions' are hand
 * derived from IEC 60559, the others' were computed with mpmath at 200 bits and
 * rounded once to the nearest double or float. An exact function must match to
 * the bit; the others may be one unit in the last place away.
 *
 * The program checks every result itself and prints the count. Its exit status
 * is 0 when every check passed and otherwise the number of the first failed
 * check, so a runtime that returns the wrong value cannot report success.
 * CRT_MATH_BREAK expects a wrong value for the first check of the two-argument
 * double table, check 67: a control.
 */
#include <math.h>
#include <stdio.h>
#include <string.h>

typedef unsigned long long u64;
typedef unsigned u32;

static unsigned checks;
static unsigned passed;
static unsigned first_failure;

static double from64(u64 bits) { double x; memcpy(&x, &bits, 8); return x; }
static u64 bits64(double x) { u64 bits; memcpy(&bits, &x, 8); return bits; }
static float from32(u32 bits) { float x; memcpy(&x, &bits, 4); return x; }
static u32 bits32(float x) { u32 bits; memcpy(&bits, &x, 4); return bits; }

static void check(const char *name, int good) {
    checks++;
    passed += good != 0;
    if (!good) {
        printf("check %u failed: %s\n", checks, name);
        if (!first_failure) first_failure = checks;
    }
}

/* Same sign and at most `ulps` representable values apart. */
static int near64(u64 actual, u64 expected, unsigned ulps) {
    if ((actual ^ expected) >> 63) return 0;
    u64 distance = actual > expected ? actual - expected : expected - actual;
    return distance <= ulps;
}

static int near32(u32 actual, u32 expected, unsigned ulps) {
    if ((actual ^ expected) >> 31) return 0;
    u32 distance = actual > expected ? actual - expected : expected - actual;
    return distance <= ulps;
}

struct unary64 { const char *name; double (*f)(double); u64 x, expected; unsigned ulps; };
struct binary64 { const char *name; double (*f)(double, double); u64 x, y, expected; unsigned ulps; };
struct unary32 { const char *name; float (*f)(float); u32 x, expected; unsigned ulps; };
struct binary32 { const char *name; float (*f)(float, float); u32 x, y, expected; unsigned ulps; };

static const struct unary64 unary64[] = {
    {"acos", acos, 0x3FD3333333333333ull, 0x3FF441F5ECBEEF59ull, 1}, /* acos(0.3) */
    {"acos", acos, 0xBFE6666666666666ull, 0x4002C501446CD5F1ull, 1}, /* acos(-0.7) */
    {"acos", acos, 0x3FEFAE147AE147AEull, 0x3FC21DF72882BFD8ull, 1}, /* acos(0.99) */
    {"asin", asin, 0x3FD3333333333333ull, 0x3FD380159E14F6FFull, 1}, /* asin(0.3) */
    {"asin", asin, 0xBFE0000000000000ull, 0xBFE0C152382D7366ull, 1}, /* asin(-0.5) */
    {"asin", asin, 0x3DDB7CDFD9D7BDBBull, 0x3DDB7CDFD9D7BDBBull, 1}, /* asin(1e-10) */
    {"atan", atan, 0x3FE0000000000000ull, 0x3FDDAC670561BB4Full, 1}, /* atan(0.5) */
    {"atan", atan, 0xC008000000000000ull, 0xBFF3FC176B7A8560ull, 1}, /* atan(-3.0) */
    {"atan", atan, 0x412E848000000000ull, 0x3FF921FA47D4B30Dull, 1}, /* atan(1000000.0) */
    {"cbrt", cbrt, 0x4000000000000000ull, 0x3FF428A2F98D728Bull, 1}, /* cbrt(2.0) */
    {"cbrt", cbrt, 0xC03B800000000000ull, 0xC00825B1B6BAC03Bull, 1}, /* cbrt(-27.5) */
    {"cbrt", cbrt, 0x01A56E1FC2F8F359ull, 0x2B2BFF2EE48E0530ull, 1}, /* cbrt(1e-300) */
    {"cos", cos, 0x3FF0000000000000ull, 0x3FE14A280FB5068Cull, 1}, /* cos(1.0) */
    {"cos", cos, 0x4059000000000000ull, 0x3FEB981DBF665FDFull, 1}, /* cos(100.0) */
    {"cos", cos, 0x4480F0CF064DD592ull, 0x3FE0BE2CEF01C8F4ull, 1}, /* cos(1e+22) */
    {"cosh", cosh, 0x3FE0000000000000ull, 0x3FF20AC1862AE8D0ull, 1}, /* cosh(0.5) */
    {"cosh", cosh, 0xC008000000000000ull, 0x402422A497D6185Eull, 1}, /* cosh(-3.0) */
    {"cosh", cosh, 0x4085E00000000000ull, 0x7EFD945DF4F8EC8Eull, 1}, /* cosh(700.0) */
    {"erf", erf, 0x3FB999999999999Aull, 0x3FBCCA5EA24FB334ull, 1}, /* erf(0.1) */
    {"erf", erf, 0xBFF8000000000000ull, 0xBFEEEA5557137AE0ull, 1}, /* erf(-1.5) */
    {"erf", erf, 0x4008000000000000ull, 0x3FEFFFD1AC4135F9ull, 1}, /* erf(3.0) */
    {"exp", exp, 0x3FF0000000000000ull, 0x4005BF0A8B145769ull, 1}, /* exp(1.0) */
    {"exp", exp, 0xC024000000000000ull, 0x3F07CD79B5647C9Bull, 1}, /* exp(-10.0) */
    {"exp", exp, 0x4085E00000000000ull, 0x7F0D945DF4F8EC8Eull, 1}, /* exp(700.0) */
    {"exp2", exp2, 0x3FE0000000000000ull, 0x3FF6A09E667F3BCDull, 1}, /* exp2(0.5) */
    {"exp2", exp2, 0xC090BA0000000000ull, 0x000000000000000Bull, 1}, /* exp2(-1070.5) */
    {"exp2", exp2, 0x402499999999999Aull, 0x4093B2C47BFF832Cull, 1}, /* exp2(10.3) */
    {"expm1", expm1, 0x3DDB7CDFD9D7BDBBull, 0x3DDB7CDFD9DDA4E3ull, 1}, /* expm1(1e-10) */
    {"expm1", expm1, 0x3FE0000000000000ull, 0x3FE4C2531C3C0D38ull, 1}, /* expm1(0.5) */
    {"expm1", expm1, 0xC000000000000000ull, 0xBFEBAB5557101F8Dull, 1}, /* expm1(-2.0) */
    {"log", log, 0x4000000000000000ull, 0x3FE62E42FEFA39EFull, 1}, /* log(2.0) */
    {"log", log, 0x01A56E1FC2F8F359ull, 0xC085963447F87FB5ull, 1}, /* log(1e-300) */
    {"log", log, 0x3FEFF7CED916872Bull, 0xBF5064670D979B73ull, 1}, /* log(0.999) */
    {"log10", log10, 0x4000000000000000ull, 0x3FD34413509F79FFull, 1}, /* log10(2.0) */
    {"log10", log10, 0x3F12599ED7C6FBD2ull, 0xC0109E9E9E90E678ull, 1}, /* log10(7e-05) */
    {"log10", log10, 0x4480F0CF064DD592ull, 0x4036000000000000ull, 1}, /* log10(1e+22) */
    {"log1p", log1p, 0x3CD203AF9EE75616ull, 0x3CD203AF9EE75613ull, 1}, /* log1p(1e-15) */
    {"log1p", log1p, 0x3FE0000000000000ull, 0x3FD9F323ECBF984Cull, 1}, /* log1p(0.5) */
    {"log1p", log1p, 0xBFE0000000000000ull, 0xBFE62E42FEFA39EFull, 1}, /* log1p(-0.5) */
    {"log2", log2, 0x4008000000000000ull, 0x3FF95C01A39FBD68ull, 1}, /* log2(3.0) */
    {"log2", log2, 0x54B249AD2594C37Dull, 0x4074C315BF6707B1ull, 1}, /* log2(1e+100) */
    {"log2", log2, 0x3FB999999999999Aull, 0xC00A934F0979A371ull, 1}, /* log2(0.1) */
    {"sin", sin, 0x3FE0000000000000ull, 0x3FDEAEE8744B05F0ull, 1}, /* sin(0.5) */
    {"sin", sin, 0x400921FB54442D18ull, 0x3CA1A62633145C07ull, 1}, /* sin(3.141592653589793) */
    {"sin", sin, 0x4480F0CF064DD592ull, 0xBFEB453AB76BF397ull, 1}, /* sin(1e+22) */
    {"sinh", sinh, 0x3EE4F8B588E368F1ull, 0x3EE4F8B588E4E940ull, 1}, /* sinh(1e-05) */
    {"sinh", sinh, 0x4000000000000000ull, 0x400D03CF63B6E19Full, 1}, /* sinh(2.0) */
    {"sinh", sinh, 0xC085E00000000000ull, 0xFEFD945DF4F8EC8Eull, 1}, /* sinh(-700.0) */
    {"tan", tan, 0x3FF0000000000000ull, 0x3FF8EB245CBEE3A6ull, 1}, /* tan(1.0) */
    {"tan", tan, 0x3FF921FB54442D18ull, 0x434D02967C31CDB5ull, 1}, /* tan(1.5707963267948966) */
    {"tan", tan, 0xBFE0000000000000ull, 0xBFE17B4F5BF3474Aull, 1}, /* tan(-0.5) */
    {"tanh", tanh, 0x3FE0000000000000ull, 0x3FDD9353D7568AF3ull, 1}, /* tanh(0.5) */
    {"tanh", tanh, 0xC000000000000000ull, 0xBFEED9505E1BC3D4ull, 1}, /* tanh(-2.0) */
    {"tanh", tanh, 0x3E45798EE2308C3Aull, 0x3E45798EE2308C3Aull, 1}, /* tanh(1e-08) */
    {"tgamma", tgamma, 0x3FE0000000000000ull, 0x3FFC5BF891B4EF6Bull, 1}, /* tgamma(0.5) */
    {"tgamma", tgamma, 0x4016000000000000ull, 0x404A2BE0247739F2ull, 1}, /* tgamma(5.5) */
    {"tgamma", tgamma, 0xC004000000000000ull, 0xBFEE3FF812E32183ull, 1}, /* tgamma(-2.5) */
    {"tgamma", tgamma, 0x4065500000000000ull, 0x7F69589F849167A8ull, 1}, /* tgamma(170.5) */
    {"ceil", ceil, 0xBFE0000000000000ull, 0x8000000000000000ull, 0}, /* ceil(-0.5) */
    {"floor", floor, 0xBFE0000000000000ull, 0xBFF0000000000000ull, 0}, /* floor(-0.5) */
    {"trunc", trunc, 0xBFFE666666666666ull, 0xBFF0000000000000ull, 0}, /* trunc(-1.9) */
    {"round", round, 0xC004000000000000ull, 0xC008000000000000ull, 0}, /* round(-2.5) */
    {"round", round, 0x3FDFFFFFFFFFFFFFull, 0x0000000000000000ull, 0}, /* round(0.49999999999999994) */
    {"rint", rint, 0x4004000000000000ull, 0x4000000000000000ull, 0}, /* rint(2.5) */
    {"rint", rint, 0x400C000000000000ull, 0x4010000000000000ull, 0}, /* rint(3.5) */
    {"nearbyint", nearbyint, 0xBFF8000000000000ull, 0xC000000000000000ull, 0}, /* nearbyint(-1.5) */
};

static const struct binary64 binary64[] = {
    {"atan2", atan2, 0x3FF0000000000000ull, 0x4000000000000000ull, 0x3FDDAC670561BB4Full, 1}, /* atan2(1.0, 2.0) */
    {"atan2", atan2, 0xBFF0000000000000ull, 0xC000000000000000ull, 0xC0056C6E7397F5AEull, 1}, /* atan2(-1.0, -2.0) */
    {"atan2", atan2, 0x3FE0000000000000ull, 0xBF1A36E2EB1C432Dull, 0x3FF922CD0B5B5711ull, 1}, /* atan2(0.5, -0.0001) */
    {"hypot", hypot, 0x4008000000000000ull, 0x4010000000000000ull, 0x4014000000000000ull, 1}, /* hypot(3.0, 4.0) */
    {"hypot", hypot, 0x7E37E43C8800759Cull, 0x7E37E43C8800759Cull, 0x7E40E4D50F99B211ull, 1}, /* hypot(1e+300, 1e+300) */
    {"hypot", hypot, 0x000012688B70E62Bull, 0x000024D116E1CC56ull, 0x0000292991FDAE04ull, 1}, /* hypot(1e-310, 2e-310) */
    {"pow", pow, 0x4000000000000000ull, 0x3FE0000000000000ull, 0x3FF6A09E667F3BCDull, 1}, /* pow(2.0, 0.5) */
    {"pow", pow, 0x3FF8000000000000ull, 0x4059000000000000ull, 0x43969194F299CDDAull, 1}, /* pow(1.5, 100.0) */
    {"pow", pow, 0x3FECCCCCCCCCCCCDull, 0xC08F400000000000ull, 0x497008CAC83EB741ull, 1}, /* pow(0.9, -1000.0) */
    {"copysign", copysign, 0x4008000000000000ull, 0x8000000000000000ull, 0xC008000000000000ull, 0}, /* copysign(3.0, -0.0) */
    {"fmod", fmod, 0xC016000000000000ull, 0x4000000000000000ull, 0xBFF8000000000000ull, 0}, /* fmod(-5.5, 2.0) */
    {"remainder", remainder, 0x401C000000000000ull, 0x4000000000000000ull, 0xBFF0000000000000ull, 0}, /* remainder(7.0, 2.0) */
    {"remainder", remainder, 0xC01C000000000000ull, 0x4000000000000000ull, 0x3FF0000000000000ull, 0}, /* remainder(-7.0, 2.0) */
    {"fdim", fdim, 0x3FF0000000000000ull, 0x4008000000000000ull, 0x0000000000000000ull, 0}, /* fdim(1.0, 3.0) */
    {"fdim", fdim, 0x4008000000000000ull, 0x3FF0000000000000ull, 0x4000000000000000ull, 0}, /* fdim(3.0, 1.0) */
    {"fmax", fmax, 0x8000000000000000ull, 0x0000000000000000ull, 0x0000000000000000ull, 0}, /* fmax(-0.0, 0.0) */
    {"fmin", fmin, 0x0000000000000000ull, 0x8000000000000000ull, 0x8000000000000000ull, 0}, /* fmin(0.0, -0.0) */
    {"remainder", remainder, 0x7E37E43C8800759Cull, 0x01D2C05BCA99D4EEull, 0x81ACE4A32878D880ull, 0}, /* remainder(1e300, 7e-300) */
    {"fmod", fmod, 0x3FB999999999999Aull, 0x01A56E1FC2F8F359ull, 0x0177350D4134AEC0ull, 0}, /* fmod(0.1, 1e-300) */
};

static const struct unary32 unary32[] = {
    {"cosf", cosf, 0x3F000000u, 0x3F60A940u, 1}, /* cosf(0.5f) */
    {"cosf", cosf, 0x42C80000u, 0x3F5CC0EEu, 1}, /* cosf(100.0f) */
    {"expf", expf, 0x3F800000u, 0x402DF854u, 1}, /* expf(1.0f) */
    {"expf", expf, 0xC1A00000u, 0x310DA433u, 1}, /* expf(-20.0f) */
    {"logf", logf, 0x40000000u, 0x3F317218u, 1}, /* logf(2.0f) */
    {"logf", logf, 0x0DA24260u, 0xC28A27B5u, 1}, /* logf(1.0000000031710769e-30f) */
    {"sinf", sinf, 0x3F000000u, 0x3EF57744u, 1}, /* sinf(0.5f) */
    {"sinf", sinf, 0x501502F9u, 0xBEF99A64u, 1}, /* sinf(10000000000.0f) */
    {"tanf", tanf, 0x3F800000u, 0x3FC75923u, 1}, /* tanf(1.0f) */
    {"tanf", tanf, 0xBE800000u, 0xBE82BC2Du, 1}, /* tanf(-0.25f) */
    {"ceilf", ceilf, 0x3FC00000u, 0x40000000u, 0}, /* ceilf(1.5f) */
    {"floorf", floorf, 0xBFC00000u, 0xC0000000u, 0}, /* floorf(-1.5f) */
    {"roundf", roundf, 0xBF000000u, 0xBF800000u, 0}, /* roundf(-0.5f) */
};

static const struct binary32 binary32[] = {
    {"atan2f", atan2f, 0x3F800000u, 0x40000000u, 0x3EED6338u, 1}, /* atan2f(1.0f, 2.0f) */
    {"atan2f", atan2f, 0xC0400000u, 0x3F000000u, 0xBFB3EC43u, 1}, /* atan2f(-3.0f, 0.5f) */
    {"powf", powf, 0x40000000u, 0x3F000000u, 0x3FB504F3u, 1}, /* powf(2.0f, 0.5f) */
    {"powf", powf, 0x3F8CCCCDu, 0x42480000u, 0x42EAC82Fu, 1}, /* powf(1.100000023841858f, 50.0f) */
    {"fmodf", fmodf, 0xC0E00000u, 0x3F400000u, 0xBE800000u, 0}, /* fmodf(-7f, 0.75f) */
};

#define LENGTH(table) (sizeof(table) / sizeof((table)[0]))

int main(void) {
    for (size_t i = 0; i < LENGTH(unary64); i++) {
        volatile double x = from64(unary64[i].x);
        check(unary64[i].name, near64(bits64(unary64[i].f(x)), unary64[i].expected, unary64[i].ulps));
    }
    for (size_t i = 0; i < LENGTH(binary64); i++) {
        volatile double x = from64(binary64[i].x), y = from64(binary64[i].y);
        u64 expected = binary64[i].expected;
#ifdef CRT_MATH_BREAK
        if (i == 0) expected ^= 1ull << 52;
#endif
        check(binary64[i].name, near64(bits64(binary64[i].f(x, y)), expected, binary64[i].ulps));
    }
    for (size_t i = 0; i < LENGTH(unary32); i++) {
        volatile float x = from32(unary32[i].x);
        check(unary32[i].name, near32(bits32(unary32[i].f(x)), unary32[i].expected, unary32[i].ulps));
    }
    for (size_t i = 0; i < LENGTH(binary32); i++) {
        volatile float x = from32(binary32[i].x), y = from32(binary32[i].y);
        check(binary32[i].name, near32(bits32(binary32[i].f(x, y)), binary32[i].expected, binary32[i].ulps));
    }

    /* The two that write through a pointer, and the two integer results. */
    volatile double subnormal = from64(1), mixed = -3.75, big = 2147483647.5, edge = 9223372036854774784.0;
    int exponent = 99;
    double whole = 0.0;
    check("frexp", bits64(frexp(subnormal, &exponent)) == bits64(0.5) && exponent == -1073);
    check("frexp", bits64(frexp(-0.0 * mixed, &exponent)) == bits64(0.0) && exponent == 0);
    check("modf", bits64(modf(mixed, &whole)) == bits64(-0.75) && bits64(whole) == bits64(-3.0));
    check("lround", lround(mixed) == -4);
    check("lround", lround(big - 1.0) == 2147483647L);
    check("llround", llround(edge) == 9223372036854774784LL);
    check("llround", llround(-0.5 + 0.0 * mixed) == -1LL);

    volatile float half = -2.5f, long_edge = from32(0x4EFFFFFFu);
    volatile float mixed32 = -3.75f, zero = 0.0f, negative_zero = from32(0x80000000u);
    volatile float seven = 7.0f, two = 2.0f;
    struct {
        volatile u32 before;
        float whole;
        volatile u32 after;
    } split32 = {0x12345678u, 0.0f, 0x9ABCDEF0u};

    check("lroundf ties away", lroundf(half) == -3L);
    check("lroundf below LONG overflow", lroundf(long_edge) == 2147483520L);
    float fraction32 = modff(mixed32, &split32.whole);
    check("modff parts", bits32(fraction32) == 0xBF400000u && bits32(split32.whole) == 0xC0400000u);
    check("modff write width", split32.before == 0x12345678u && split32.after == 0x9ABCDEF0u);
    check("nextafterf equal zeros", bits32(nextafterf(zero, negative_zero)) == 0x80000000u);
    check("nextafterf first subnormal", bits32(nextafterf(negative_zero, two)) == 1u);
    check("remainderf ties even", bits32(remainderf(seven, two)) == 0xBF800000u);

    printf("crt_math: %u of %u checks passed\n", passed, checks);
    return (int)first_failure;
}
