// SPDX-License-Identifier: GPL-3.0-or-later

// Original AMD64 guest using the public XAudio2.9 vtable and packed records.
typedef unsigned int u32;
typedef unsigned short u16;
typedef unsigned long long u64;
typedef int hresult;

struct engine;
struct source;

#pragma pack(push, 1)
struct format {
    u16 tag, channels;
    u32 sample_rate, average_bytes;
    u16 align, bits, extra;
};
struct buffer {
    u32 flags, audio_bytes;
    const short *audio;
    u32 play_begin, play_length, loop_begin, loop_length, loop_count;
    void *context;
};
struct voice_state {
    void *context;
    u32 queued;
    u64 samples;
};
#pragma pack(pop)
_Static_assert(sizeof(struct format) == 18, "Win64 WAVEFORMATEX");
_Static_assert(sizeof(struct buffer) == 44, "Win64 XAUDIO2_BUFFER");
_Static_assert(sizeof(struct voice_state) == 20, "Win64 XAUDIO2_VOICE_STATE");

struct engine_vtable {
    void (*query_interface)(void);
    void (*add_ref)(void);
    u32 (*release)(struct engine *);
    void (*callback_registration[2])(void);
    hresult (*create_source)(struct engine *, struct source **, const struct format *,
                             u32, float, void *, void *, void *);
    void (*create_submix)(void);
    hresult (*create_mastering)(struct engine *, void **, u32, u32, u32,
                                const u16 *, void *, u32);
    hresult (*start)(struct engine *);
    void (*stop)(struct engine *);
};

struct source_vtable {
    void (*common_voice_methods[18])(void);
    void (*destroy)(struct source *);
    hresult (*start)(struct source *, u32, u32);
    hresult (*stop)(struct source *, u32, u32);
    hresult (*submit)(struct source *, const struct buffer *, void *);
    hresult (*flush)(struct source *);
    void (*discontinuity)(void);
    void (*exit_loop)(void);
    void (*get_state)(struct source *, struct voice_state *, u32);
    hresult (*set_ratio)(struct source *, float, u32);
};
struct engine { const struct engine_vtable *v; };
struct source { const struct source_vtable *v; };

__declspec(dllimport) hresult XAudio2Create(struct engine **, u32, u32);
__declspec(dllimport) __declspec(noreturn) void ExitProcess(u32);

#define CHECK(condition) do { if (!(condition)) ExitProcess(__LINE__); } while (0)

static const struct format stereo = {1, 2, 48000, 192000, 4, 16, 0};
static const short pcm[2];
static const struct buffer one_frame = {.audio_bytes = sizeof pcm, .audio = pcm};

static void expect_state(struct source *source, u32 queued, u64 samples, u32 line) {
    struct voice_state value;
    source->v->get_state(source, &value, 0);
    if (value.queued != queued || value.samples != samples) ExitProcess(line);
}

#define EXPECT_STATE(source, queued, samples) expect_state(source, queued, samples, __LINE__)

static struct source *create_slow_source(struct engine *engine) {
    struct source *source = 0;
    CHECK(engine->v->create_source(engine, &source, &stereo, 0, 2.0f, 0, 0, 0) == 0);
    CHECK(source->v->set_ratio(source, 1.0f / 1024.0f, 0) == 0);
    CHECK(source->v->submit(source, &one_frame, 0) == 0);
    return source;
}

void entry(void) {
    struct engine *engine = 0;
    void *mastering = 0;
    CHECK(XAudio2Create(&engine, 0, 1) == 0);
    CHECK(engine->v->create_mastering(engine, &mastering, 2, 48000, 0, 0, 0, 6) == 0);
    struct source *first = create_slow_source(engine);
    struct source *second = create_slow_source(engine);

    CHECK(first->v->start(first, 0, 0) == 0);
    EXPECT_STATE(first, 1, 0);  // First voice's first quantum.
    CHECK(first->v->submit(first, &one_frame, 0) == 0);
    CHECK(first->v->flush(first) == 0);  // Keep the front buffer and its progress.
    CHECK(first->v->stop(first, 0, 0) == 0);
    EXPECT_STATE(first, 1, 0);  // Stopped voices do not accrue credit.
    CHECK(first->v->start(first, 0, 0) == 0);
    CHECK(second->v->start(second, 0, 0) == 0);
    engine->v->stop(engine);
    EXPECT_STATE(first, 1, 0);  // A stopped engine freezes both voices.
    CHECK(engine->v->start(engine) == 0);
    EXPECT_STATE(first, 1, 0);  // First voice's second quantum.
    EXPECT_STATE(first, 0, 1);  // First drains on three quanta.

    engine->v->stop(engine);
    EXPECT_STATE(second, 1, 0);  // Second has seen only two quanta.
    CHECK(engine->v->start(engine) == 0);
    EXPECT_STATE(second, 0, 1);
    EXPECT_STATE(first, 0, 1);
    CHECK(first->v->submit(first, &one_frame, 0) == 0);
    EXPECT_STATE(first, 1, 1);
    EXPECT_STATE(first, 1, 1);  // Drain and idle did not preload the new buffer.
    EXPECT_STATE(first, 0, 2);

    CHECK(second->v->submit(second, &one_frame, 0) == 0);
    EXPECT_STATE(second, 1, 1);
    CHECK(second->v->stop(second, 0, 0) == 0);
    CHECK(second->v->flush(second) == 0);
    CHECK(second->v->submit(second, &one_frame, 0) == 0);
    CHECK(second->v->start(second, 0, 0) == 0);
    EXPECT_STATE(second, 1, 1);
    EXPECT_STATE(second, 1, 1);  // Flushing discarded the previous fraction.
    EXPECT_STATE(second, 0, 2);

    second->v->destroy(second);
    first->v->destroy(first);
    CHECK(engine->v->release(engine) == 0);
    ExitProcess(0);
}
