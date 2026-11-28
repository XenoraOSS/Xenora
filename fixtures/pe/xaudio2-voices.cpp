// SPDX-License-Identifier: GPL-3.0-or-later
// Original guest acceptance program for the bounded XAudio2 runtime (#888,
// PR 1b). Freestanding: no CRT and no SDK headers. The interface layouts are
// written here from the public XAudio2 reference (method order of IXAudio2,
// IXAudio2Voice, IXAudio2SourceVoice and IXAudio2MasteringVoice).
//
// The process exit code is the number of the first failed check, or 0 when
// every check passed. The runner also asserts that every engine, voice and
// buffer is gone afterwards.
//
// Clock contract until the #306 mixer drives it (docs/runtime/xaudio2.md):
// each IXAudio2SourceVoice::GetState call first advances every started source
// voice of the engine by one 10 ms quantum of the mastering rate (480 frames
// at 48 kHz, frequency ratio 1, same source and mastering rate).

typedef unsigned char u8;
typedef unsigned short u16;
typedef unsigned int u32;
typedef unsigned long long u64;
typedef long HRESULT;
typedef u32 ULONG;

extern "C" __declspec(dllimport) void __stdcall ExitProcess(u32 code);

struct GUID { u32 a; u16 b, c; u8 d[8]; };

#pragma pack(push, 2)
struct WAVEFORMATEX { u16 tag, channels; u32 rate, avg; u16 align, bits, cb; };
#pragma pack(pop)
static_assert(sizeof(WAVEFORMATEX) == 18, "WAVEFORMATEX is 18 packed bytes");

// xaudio2.h: "All structures defined in this file use tight field packing".
#pragma pack(push, 1)
struct XAUDIO2_BUFFER {
    u32 Flags, AudioBytes; const u8 *pAudioData;
    u32 PlayBegin, PlayLength, LoopBegin, LoopLength, LoopCount; void *pContext;
};
struct XAUDIO2_VOICE_STATE { void *pCurrentBufferContext; u32 BuffersQueued; u64 SamplesPlayed; };
#pragma pack(pop)
static_assert(sizeof(XAUDIO2_BUFFER) == 44, "packed XAUDIO2_BUFFER is 44 bytes on x64");
static_assert(sizeof(XAUDIO2_VOICE_STATE) == 20, "packed XAUDIO2_VOICE_STATE is 20 bytes on x64");

struct IXAudio2; struct IXAudio2Voice; struct IXAudio2SourceVoice; struct IXAudio2MasteringVoice;
typedef void *Any;

struct IXAudio2Vtbl {
    HRESULT (*QueryInterface)(IXAudio2 *, const GUID *, void **);
    ULONG (*AddRef)(IXAudio2 *);
    ULONG (*Release)(IXAudio2 *);
    HRESULT (*RegisterForCallbacks)(IXAudio2 *, Any);
    void (*UnregisterForCallbacks)(IXAudio2 *, Any);
    HRESULT (*CreateSourceVoice)(IXAudio2 *, IXAudio2SourceVoice **, const WAVEFORMATEX *, u32, float, Any, Any, Any);
    HRESULT (*CreateSubmixVoice)(IXAudio2 *, Any *, u32, u32, u32, u32, Any, Any);
    HRESULT (*CreateMasteringVoice)(IXAudio2 *, IXAudio2MasteringVoice **, u32, u32, u32, const wchar_t *, Any, u32);
    HRESULT (*StartEngine)(IXAudio2 *);
    void (*StopEngine)(IXAudio2 *);
    HRESULT (*CommitChanges)(IXAudio2 *, u32);
    void (*GetPerformanceData)(IXAudio2 *, Any);
    void (*SetDebugConfiguration)(IXAudio2 *, Any, Any);
};
struct IXAudio2 { const IXAudio2Vtbl *v; };

// IXAudio2Voice methods, shared by both voice kinds (19 entries).
#define VOICE_METHODS(Self)                                                     \
    void (*GetVoiceDetails)(Self *, Any);                                       \
    HRESULT (*SetOutputVoices)(Self *, Any);                                    \
    HRESULT (*SetEffectChain)(Self *, Any);                                     \
    HRESULT (*EnableEffect)(Self *, u32, u32);                                  \
    HRESULT (*DisableEffect)(Self *, u32, u32);                                 \
    void (*GetEffectState)(Self *, u32, Any);                                   \
    HRESULT (*SetEffectParameters)(Self *, u32, Any, u32, u32);                 \
    HRESULT (*GetEffectParameters)(Self *, u32, Any, u32);                      \
    HRESULT (*SetFilterParameters)(Self *, Any, u32);                           \
    void (*GetFilterParameters)(Self *, Any);                                   \
    HRESULT (*SetOutputFilterParameters)(Self *, Any, Any, u32);                \
    void (*GetOutputFilterParameters)(Self *, Any, Any);                        \
    HRESULT (*SetVolume)(Self *, float, u32);                                   \
    void (*GetVolume)(Self *, float *);                                         \
    HRESULT (*SetChannelVolumes)(Self *, u32, const float *, u32);              \
    void (*GetChannelVolumes)(Self *, u32, float *);                            \
    HRESULT (*SetOutputMatrix)(Self *, Any, u32, u32, const float *, u32);      \
    void (*GetOutputMatrix)(Self *, Any, u32, u32, float *);                    \
    void (*DestroyVoice)(Self *);

struct IXAudio2SourceVoiceVtbl {
    VOICE_METHODS(IXAudio2SourceVoice)
    HRESULT (*Start)(IXAudio2SourceVoice *, u32, u32);
    HRESULT (*Stop)(IXAudio2SourceVoice *, u32, u32);
    HRESULT (*SubmitSourceBuffer)(IXAudio2SourceVoice *, const XAUDIO2_BUFFER *, Any);
    HRESULT (*FlushSourceBuffers)(IXAudio2SourceVoice *);
    HRESULT (*Discontinuity)(IXAudio2SourceVoice *);
    HRESULT (*ExitLoop)(IXAudio2SourceVoice *, u32);
    void (*GetState)(IXAudio2SourceVoice *, XAUDIO2_VOICE_STATE *, u32);
    HRESULT (*SetFrequencyRatio)(IXAudio2SourceVoice *, float, u32);
    void (*GetFrequencyRatio)(IXAudio2SourceVoice *, float *);
    HRESULT (*SetSourceSampleRate)(IXAudio2SourceVoice *, u32);
};
struct IXAudio2SourceVoice { const IXAudio2SourceVoiceVtbl *v; };
struct IXAudio2MasteringVoiceVtbl {
    VOICE_METHODS(IXAudio2MasteringVoice)
    HRESULT (*GetChannelMask)(IXAudio2MasteringVoice *, u32 *);
};
struct IXAudio2MasteringVoice { const IXAudio2MasteringVoiceVtbl *v; };
static_assert(sizeof(IXAudio2Vtbl) == 13 * 8, "IXAudio2 has 13 methods");
static_assert(sizeof(IXAudio2SourceVoiceVtbl) == 29 * 8, "source voice has 29 methods");
static_assert(sizeof(IXAudio2MasteringVoiceVtbl) == 20 * 8, "mastering voice has 20 methods");

extern "C" __declspec(dllimport) HRESULT __stdcall XAudio2Create(IXAudio2 **, u32, u32);

static const HRESULT S_OK = 0;
static const HRESULT E_NOTIMPL = (HRESULT)0x80004001;
static const HRESULT E_NOINTERFACE = (HRESULT)0x80004002;
static const HRESULT E_POINTER = (HRESULT)0x80004003;
static const HRESULT E_INVALIDARG = (HRESULT)0x80070057;
static const HRESULT INVALID_CALL = (HRESULT)0x88960001;
static const u32 END_OF_STREAM = 0x40;

// IID_IXAudio2 for XAudio2 2.9 (public xaudio2.h interface identifier).
static const GUID IID_IXAudio2 = {0x2B02E3CF, 0x2E0B, 0x4EC3, {0xBE, 0x45, 0x1B, 0x2A, 0x3F, 0xE7, 0x21, 0x0D}};
static const GUID IID_IUnknown = {0x00000000, 0x0000, 0x0000, {0xC0, 0, 0, 0, 0, 0, 0, 0x46}};
static const GUID IID_Other = {0x12345678, 0x9ABC, 0xDEF0, {1, 2, 3, 4, 5, 6, 7, 8}};

// Two stereo PCM16 buffers of 4800 frames (100 ms at 48 kHz) each.
static short pcm[2][4800 * 2];

static u32 check = 0;
static void expect(bool ok) {
    ++check;
    if (!ok) ExitProcess(check);
}

static WAVEFORMATEX pcm16(u16 channels, u32 rate) {
    WAVEFORMATEX f = {1, channels, rate, rate * channels * 2u, (u16)(channels * 2), 16, 0};
    return f;
}

static XAUDIO2_VOICE_STATE state_of(IXAudio2SourceVoice *s) {
    XAUDIO2_VOICE_STATE st = {(void *)1, 0xFFFFFFFFu, 0xFFFFFFFFFFFFFFFFull};
    s->v->GetState(s, &st, 0);
    return st;
}

static void round_trip(bool first) {
    IXAudio2 *x = nullptr;
    expect(XAudio2Create(nullptr, 0, 1) == E_POINTER);
    expect(XAudio2Create(&x, 1, 1) == E_NOTIMPL && x == nullptr);  // debug-engine flag
    expect(XAudio2Create(&x, 0, 1) == S_OK && x != nullptr);

    // IUnknown: QueryInterface adds a reference; Release returns the new count.
    IXAudio2 *same = nullptr;
    expect(x->v->QueryInterface(x, &IID_IXAudio2, (void **)&same) == S_OK && same == x);
    expect(x->v->Release(x) == 1);
    expect(x->v->QueryInterface(x, &IID_IUnknown, (void **)&same) == S_OK && same == x);
    expect(x->v->AddRef(x) == 3 && x->v->Release(x) == 2 && x->v->Release(x) == 1);
    same = x;
    expect(x->v->QueryInterface(x, &IID_Other, (void **)&same) == E_NOINTERFACE && same == nullptr);
    expect(x->v->QueryInterface(x, &IID_IXAudio2, nullptr) == E_POINTER);

    // Explicitly unsupported pieces fail instead of pretending.
    expect(x->v->RegisterForCallbacks(x, (Any)0x1000) == E_NOTIMPL);
    Any submix = nullptr;
    expect(x->v->CreateSubmixVoice(x, &submix, 2, 48000, 0, 0, nullptr, nullptr) == E_NOTIMPL);

    WAVEFORMATEX f = pcm16(2, 48000);
    IXAudio2SourceVoice *s = nullptr;
    expect(x->v->CreateSourceVoice(x, &s, &f, 0, 2.0f, nullptr, nullptr, nullptr) == INVALID_CALL);

    IXAudio2MasteringVoice *m = nullptr;
    expect(x->v->CreateMasteringVoice(x, &m, 2, 48000, 0, nullptr, nullptr, 6) == S_OK && m);
    IXAudio2MasteringVoice *m2 = nullptr;
    expect(x->v->CreateMasteringVoice(x, &m2, 2, 48000, 0, nullptr, nullptr, 6) == INVALID_CALL);
    expect(m->v->SetEffectChain(m, (Any)0x1000) == E_NOTIMPL);

    // Format validation through guest memory.
    WAVEFORMATEX bad = f;
    bad.align = 3;
    expect(x->v->CreateSourceVoice(x, &s, &bad, 0, 2.0f, nullptr, nullptr, nullptr) == E_INVALIDARG);
    WAVEFORMATEX adpcm = f;
    adpcm.tag = 2;
    expect(x->v->CreateSourceVoice(x, &s, &adpcm, 0, 2.0f, nullptr, nullptr, nullptr) == E_NOTIMPL);
    expect(x->v->CreateSourceVoice(x, &s, (const WAVEFORMATEX *)0x10, 0, 2.0f, nullptr, nullptr, nullptr) == E_POINTER);
    expect(x->v->CreateSourceVoice(x, nullptr, &f, 0, 2.0f, nullptr, nullptr, nullptr) == E_POINTER);
    expect(x->v->CreateSourceVoice(x, &s, &f, 0, 2.0f, nullptr, nullptr, nullptr) == S_OK && s);

    // Volume and frequency ratio round-trip through the guest.
    float value = -1.0f;
    expect(s->v->SetVolume(s, 0.5f, 0) == S_OK);
    s->v->GetVolume(s, &value);
    expect(value == 0.5f);
    expect(s->v->SetVolume(s, __builtin_nanf(""), 0) == E_INVALIDARG);
    expect(s->v->SetFrequencyRatio(s, 1.5f, 0) == S_OK);
    s->v->GetFrequencyRatio(s, &value);
    expect(value == 1.5f);
    expect(s->v->SetFrequencyRatio(s, 1.0f, 0) == S_OK);

    // Buffer validation: bad structure and data pointers never fault the host.
    XAUDIO2_BUFFER a = {0, sizeof pcm[0], (const u8 *)pcm[0], 0, 0, 0, 0, 0, (void *)0xA};
    XAUDIO2_BUFFER b = {END_OF_STREAM, sizeof pcm[1], (const u8 *)pcm[1], 0, 0, 0, 0, 0, (void *)0xB};
    XAUDIO2_BUFFER null_data = a;
    null_data.pAudioData = nullptr;
    expect(s->v->SubmitSourceBuffer(s, &null_data, nullptr) == E_POINTER);
    XAUDIO2_BUFFER wild_data = a;
    wild_data.pAudioData = (const u8 *)0x10;
    expect(s->v->SubmitSourceBuffer(s, &wild_data, nullptr) == E_POINTER);
    expect(s->v->SubmitSourceBuffer(s, (const XAUDIO2_BUFFER *)0x10, nullptr) == E_POINTER);
    XAUDIO2_BUFFER odd = a;
    odd.AudioBytes = 6;  // not a whole 4-byte frame
    expect(s->v->SubmitSourceBuffer(s, &odd, nullptr) == E_INVALIDARG);
    expect(s->v->SubmitSourceBuffer(s, &a, nullptr) == S_OK);
    expect(s->v->SubmitSourceBuffer(s, &b, nullptr) == S_OK);

    // A stopped voice does not advance; GetState writes all 24 bytes.
    XAUDIO2_VOICE_STATE st = state_of(s);
    expect(st.pCurrentBufferContext == (void *)0xA && st.BuffersQueued == 2 && st.SamplesPlayed == 0);

    // Started: each GetState advances one 480-frame quantum. 4800 + 4800
    // frames drain in exactly 20 calls; end of stream resets SamplesPlayed.
    expect(s->v->Start(s, 0, 0) == S_OK);
    st = state_of(s);
    expect(st.pCurrentBufferContext == (void *)0xA && st.BuffersQueued == 2 && st.SamplesPlayed == 480);
    for (int i = 2; i <= 10; ++i) st = state_of(s);
    expect(st.pCurrentBufferContext == (void *)0xB && st.BuffersQueued == 1 && st.SamplesPlayed == 4800);
    expect(s->v->Stop(s, 0, 0) == S_OK);
    st = state_of(s);
    expect(st.BuffersQueued == 1 && st.SamplesPlayed == 4800);  // stopped: no advance
    expect(s->v->Start(s, 0, 0) == S_OK);
    for (int i = 11; i <= 19; ++i) st = state_of(s);
    expect(st.BuffersQueued == 1 && st.SamplesPlayed == 9120);
    st = state_of(s);
    expect(st.pCurrentBufferContext == nullptr && st.BuffersQueued == 0 && st.SamplesPlayed == 0);

    // Flush on a stopped voice removes everything queued.
    expect(s->v->Stop(s, 0, 0) == S_OK);
    expect(s->v->SubmitSourceBuffer(s, &a, nullptr) == S_OK);
    expect(s->v->FlushSourceBuffers(s) == S_OK);
    expect(state_of(s).BuffersQueued == 0);

    // The mastering voice cannot go while a source voice targets it.
    m->v->DestroyVoice(m);
    expect(x->v->CreateMasteringVoice(x, &m2, 2, 48000, 0, nullptr, nullptr, 6) == INVALID_CALL);

    // Use after DestroyVoice is refused, not a host fault.
    s->v->DestroyVoice(s);
    expect(s->v->Start(s, 0, 0) == INVALID_CALL);
    expect(s->v->SubmitSourceBuffer(s, &a, nullptr) == INVALID_CALL);
    m->v->DestroyVoice(m);
    expect(x->v->CreateMasteringVoice(x, &m2, 1, 44100, 0, nullptr, nullptr, 6) == S_OK && m2);

    // The final Release tears the engine and its remaining voice down.
    expect(x->v->Release(x) == 0);
    (void)first;
}

extern "C" void entry() {
    for (int round = 0; round < 100; ++round) round_trip(round == 0);
    ExitProcess(0);
}
