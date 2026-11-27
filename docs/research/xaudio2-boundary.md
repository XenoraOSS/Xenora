# Public XAudio2 runtime boundary

This is a design record, not an implementation or compatibility claim. It uses
public Microsoft Win32 XAudio2 documentation only. It does not use leaked or
restricted console material, does not describe proprietary Xbox DSP behavior,
and does not prove that a commercial game can run.

## Relationship to existing work

The native PCM mixer and CoreAudio prototype is separately claimed in #306.
This document must not be read as taking that work over. It only defines the
guest-facing boundary and validation questions that a future implementation
would have to satisfy. COM infrastructure is also active elsewhere, so this
record does not prescribe edits to those files.

## Public object model

Public XAudio2 documentation divides voices into three roles:

1. **Source voices** accept client-owned encoded or PCM buffers.
2. **Submix voices** combine and process audio from upstream voices.
3. **Mastering voices** mix their input and write it to an audio output
   device.

Every voice performs volume adjustment, optional effects, channel volume
adjustment and matrix mixing in the documented order. Source voices can also
perform sample-rate conversion, pitch adjustment and filtering before their
output enters the graph. A mastering voice is therefore not merely a handle to
a speaker: it is the terminal graph node with its own input rate, channel count,
effects and mixing semantics.

Public creation documentation also imposes ordering. An engine object exists
before voices. A mastering voice normally exists before meaningful output.
Source voices are created with an explicit wave format and must eventually send
their output to a mastering voice, directly or through submix voices. Voice
destruction is synchronous from the client's perspective in the public API,
but buffer memory remains client-owned until playback no longer references it.

## Narrow honest implementation shape

A first implementation should not expose the whole object model. The smallest
useful boundary is:

- one engine instance per runtime service state;
- at most one mastering voice using explicit input channels and sample rate;
- PCM mono or stereo source voices whose formats can be validated exactly;
- buffer submission, start, stop and destruction;
- deterministic buffer queue accounting before real-time output;
- explicit unsupported errors for compressed input, effects, callbacks,
  submix voices, device enumeration, spatial audio and virtualization flags.

Even that shape must not silently drop audio. If no output backend exists,
creation can still model graph state honestly, but audible output must not be
claimed. Documentation and tests must distinguish queue correctness from sound
reaching CoreAudio or hardware.

## Format and numeric constraints

Public mastering-voice documentation says input channels are bounded by
`XAUDIO2_MAX_AUDIO_CHANNELS`, and input sample rates must be a multiple of
`XAUDIO2_QUANTUM_DENOMINATOR` and between the documented minimum and maximum.
Those exact values need to be copied from the public headers or documented
public constants before implementation; they are intentionally not guessed
here.

`WAVEFORMATEX` validation must be total and deterministic:

- reject malformed or truncated format structures before creating state;
- reject impossible channel counts, zero or out-of-range sample rates,
  inconsistent block alignment and inconsistent byte rates;
- accept only formats the implementation actually consumes;
- treat compressed formats and extensible-format details as separate slices,
  not silent PCM reinterpretations;
- preserve integer overflow checks for every byte-count conversion.

## Buffer submission contract

`SubmitSourceBuffer` queues buffers in submission order. If the voice is
started and empty, a submitted buffer starts immediately; if stopped, it waits
until start. `PlayBegin`/`PlayLength` select a bounded play region, with zero
length meaning the whole buffer only under the documented precondition.
`LoopBegin`, `LoopLength` and `LoopCount` define an optional non-empty loop
region, including the documented no-loop sentinel and infinite-loop value.
`XAUDIO2_END_OF_STREAM` marks the final buffer of a contiguous sound.

The implementation must copy or retain metadata exactly, but it must not copy
unbounded client memory. Since the API leaves audio storage client-owned until
playback completes, lifetime rules need explicit design. Copying bytes into a
bounded runtime-owned queue simplifies memory safety but changes memory-use
behavior; referencing guest memory avoids copies but requires rigorous lifetime
and mutation semantics. That choice is unresolved and must be resolved before
implementation rather than hidden.

Buffer validation must at least cover:

- null and malformed buffer pointers;
- byte-count overflow and guest-memory range validity;
- play and loop regions inside the declared buffer;
- alignment to the wave format's block alignment;
- loop-count sentinel combinations;
- end-of-stream ordering and behavior after it;
- maximum queued-buffer and queued-byte limits, chosen and documented by
  Xenora rather than invented as Windows behavior.

## State and error honesty

XAudio2 methods return `HRESULT`, not Win32 error codes. The future runtime
must map unsupported or invalid operations to documented public XAudio2 error
codes where one exists and use `E_INVALIDARG`, `E_POINTER` or `E_FAIL` only
where appropriate. Unknown console-specific HRESULT behavior is unknown and
must not be guessed.

State transitions need a table before implementation. At minimum it must
answer what happens when a guest:

- submits before creating a valid source voice;
- starts or stops repeatedly;
- submits after end-of-stream;
- destroys a voice with queued or playing buffers;
- destroys the mastering voice while sources still target it;
- releases the engine before voices;
- uses a voice pointer after release.

If callbacks are unsupported, the API must say so consistently. Implementing
no-op success for callbacks would hide synchronization bugs and is not
acceptable evidence.

## Validation without a mixer

Useful validation is possible before the #306 mixer exists:

- structural voice lifetime tests;
- format validation matrices;
- byte-range and overflow tests;
- queue ordering and end-of-stream state tests;
- deterministic offline consumption tests with a fixed sample clock;
- independent byte-level expected-output comparisons for simple PCM input.

These tests do not prove audible output. Hardware evidence, when eventually
available, must be a separate opt-in test and must state device, format,
sample rate and whether the result was observed by signal capture or only by
API success. Skipped hardware tests must never be counted as audio validation.

## Open questions

- Which exact XAudio2 version surface is required by Xbox-targeted public
  documentation, and how it differs from the Win32 surface?
- Whether Xenora should copy PCM buffers or reference guest memory.
- How buffer completion should be represented if callbacks remain unsupported.
- Whether submix voices are required by realistic workloads before effects.
- Which portions of sample-rate conversion belong in the future mixer rather
  than the guest API boundary.
- How COM identity, reference counting and thread lifetime should interact
  with the active COM infrastructure design.

These questions remain unresolved. Until they are answered and implemented,
Xenora has no XAudio2 compatibility claim.

## Sources

- Microsoft Learn, “XAudio2 Voices”:
  https://learn.microsoft.com/en-us/windows/win32/xaudio2/xaudio2-voices
- Microsoft Learn, `IXAudio2::CreateMasteringVoice`:
  https://learn.microsoft.com/en-us/windows/win32/api/xaudio2/nf-xaudio2-ixaudio2-createmasteringvoice
- Microsoft Learn, `IXAudio2SourceVoice::SubmitSourceBuffer`:
  https://learn.microsoft.com/en-us/windows/win32/api/xaudio2/nf-xaudio2-ixaudio2sourcevoice-submitsourcebuffer
- Microsoft Learn, “How to: Play a sound with XAudio2”:
  https://learn.microsoft.com/en-us/windows/win32/xaudio2/how-to--play-a-sound-with-xaudio2
