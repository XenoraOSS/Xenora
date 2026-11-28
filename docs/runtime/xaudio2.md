# Bounded XAudio2 core

`crates/xaudio2` (`xenora-xaudio2`) models the part of the XAudio2 audio API
a guest program can observe: the lifetime of engines and voices, the buffer
queue of a source voice, the fixed sample clock, the voice state and the
result codes the API reports. The crate is pure Rust with no dependencies and
`#![forbid(unsafe_code)]`.

The model stops at the boundary of the guest address space. A buffer names a
guest address and a byte count; the crate never reads, copies, mixes or
resamples samples, and it loads no guest code. Mixing belongs to the audio
crate that issue #306 owns.

Nothing in this crate is audible and nothing in it runs guest code. The
evidence is the crate's own tests and the acceptance test in
`crates/xaudio2/tests/boundary.rs`, both run locally on Apple Silicon. No
comparison with Windows and no console hardware is claimed.

Every rule below is marked:

- **Public** means the rule comes from the public XAudio2 documentation on
  learn.microsoft.com. The pages are the
  [XAUDIO2_BUFFER structure](https://learn.microsoft.com/en-us/windows/win32/api/xaudio2/ns-xaudio2-xaudio2_buffer),
  the [XAUDIO2_VOICE_STATE structure](https://learn.microsoft.com/en-us/windows/win32/api/xaudio2/ns-xaudio2-xaudio2_voice_state),
  the [boundary values and flags](https://learn.microsoft.com/en-us/windows/win32/xaudio2/xaudio2-boundary-values-and-flags),
  the [error codes](https://learn.microsoft.com/en-us/windows/win32/xaudio2/xaudio2-error-codes),
  the [operation sets](https://learn.microsoft.com/en-us/windows/win32/xaudio2/xaudio2-operation-sets)
  and the interface pages of IXAudio2, IXAudio2MasteringVoice and
  IXAudio2SourceVoice. The numeric constants come from the public
  `xaudio2.h` header that those pages describe; no header text is copied
  into the repository.
- **Xenora** means the rule is a decision of this project, either because the
  public documentation is silent or because Xenora models a smaller world
  than the API does.

## Guest-visible layouts

The guest lays the structures out itself, so the model reads them in the x64
layout: natural alignment, 8-byte pointers and explicit padding.

### WAVEFORMATEX, 18 bytes

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 2 | Format tag |
| 2 | 2 | Channel count |
| 4 | 4 | Sample rate in hertz |
| 8 | 4 | Average bytes per second |
| 12 | 2 | Block alignment in bytes |
| 14 | 2 | Bits per sample |
| 16 | 2 | Callback size, ignored for both supported tags |

**Public.** `xaudio2.h` packs every structure it defines to one byte
("All structures defined in this file use tight field packing"), so neither
structure below has padding.

### XAUDIO2_BUFFER, 44 bytes

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | Flags |
| 4 | 4 | Audio byte count |
| 8 | 8 | Guest address of the audio data |
| 16 | 4 | Play begin, in frames |
| 20 | 4 | Play length, in frames |
| 24 | 4 | Loop begin, in frames |
| 28 | 4 | Loop length, in frames |
| 32 | 4 | Loop count |
| 36 | 8 | Client context |

### XAUDIO2_VOICE_STATE, 20 bytes

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | Client context of the current buffer |
| 8 | 4 | Number of queued buffers |
| 12 | 8 | Samples played |

## Handles

- **Xenora.** A handle is a slot number and a generation. Destroying a voice
  or releasing an engine frees its slot, and the next object that takes the
  slot gets a new generation. A slot whose generation reaches `u32::MAX` is
  retired rather than reused, so a stale handle never names a live object,
  however many objects a guest creates.
- **Xenora.** Every use of a destroyed voice, or of a voice whose engine was
  released, is refused with `InvalidCall` and the result code
  `XAUDIO2_E_INVALID_CALL`.
- **Xenora.** A source-only operation (submit, start, stop, flush, state,
  consume, frequency ratio) applied to a mastering voice is refused with
  `InvalidCall`. The volume applies to every voice.

## Engine

- **Public.** The engine flags of the public API select the debug engine and
  other engine behaviour. **Xenora** models no engine flag, so a nonzero flag
  is refused with `NotImplemented`.
- **Xenora.** The audio processor is accepted and recorded. The model does not
  process audio, so the processor changes nothing.
- **Xenora.** A runtime holds at most `MAX_ENGINES` (8) live engines. The
  next engine is refused with `OutOfMemory`.
- **Xenora.** Releasing an engine destroys every voice and buffer the engine
  owns, and the handles of those voices become stale.

## Mastering voice

- **Xenora.** An engine has at most one mastering voice. A second one is
  refused with `InvalidCall`.
- **Public.** The channel count must be between 1 and
  `XAUDIO2_MAX_AUDIO_CHANNELS` (64), and a zero count asks for the platform
  default. **Xenora** supports mono and stereo only: a count from 3 to 64 is
  refused with `NotImplemented`, a count above 64 with `InvalidArg`, and a
  zero count becomes the Xenora default of two channels.
- **Public.** The sample rate must be between `XAUDIO2_MIN_SAMPLE_RATE`
  (1000) and `XAUDIO2_MAX_SAMPLE_RATE` (200000) hertz, and a zero rate asks
  for the platform default. **Xenora** supports 44100 and 48000 hertz: a zero
  rate becomes the Xenora default of 48000, another rate in range that is a
  multiple of `XAUDIO2_QUANTUM_DENOMINATOR` (100) is refused with
  `NotImplemented`, and any other rate with `InvalidArg`.
- **Xenora.** The flags, the device id and the effect chain of the public
  call are refused with `NotImplemented` when they are nonzero. The stream
  category is recorded.
- **Xenora.** A mastering voice cannot be destroyed while the engine still
  has source voices, because those voices would lose their destination. The
  call is refused with `InvalidCall`.

## Source voice

- **Xenora.** A source voice needs a live mastering voice on its engine. The
  call is refused with `InvalidCall` without one.
- **Xenora.** The voice flags, the notification callback, the send list and
  the effect chain of the public call are refused with `NotImplemented`
  when they are nonzero. The callback arrives in a later change.
- **Public.** The maximum frequency ratio of a voice must be at least
  `XAUDIO2_MIN_FREQ_RATIO` (1/1024) and at most `XAUDIO2_MAX_FREQ_RATIO`
  (1024). A ratio outside that range, or a ratio that is not finite, is
  refused with `InvalidArg`.
- **Xenora.** An engine holds at most `MAX_SOURCE_VOICES` (256) live source
  voices. The next one is refused with `OutOfMemory`.
- **Xenora.** A new voice is stopped, plays at volume 1.0 and at frequency
  ratio 1.0.

## Wave formats

`WaveFormat::parse` reads a WAVEFORMATEX structure. **Xenora** checks the
fields in this order, and each step refuses with `InvalidArg` or
`NotImplemented` as it says:

1. Fewer than 18 bytes is `InvalidArg`.
2. The format tag must be 1 (PCM) or 3 (IEEE float). Any other tag, including
   2 (ADPCM) and 0xFFFE (extensible), is `NotImplemented`.
3. The channel count must not be zero and must not exceed
   `XAUDIO2_MAX_AUDIO_CHANNELS`, otherwise `InvalidArg`.
4. The sample rate must be between `XAUDIO2_MIN_SAMPLE_RATE` and
   `XAUDIO2_MAX_SAMPLE_RATE`, otherwise `InvalidArg`.
5. A PCM bit depth must be nonzero and a multiple of 8, and an IEEE float
   format must be 32 bits, otherwise `InvalidArg`.
6. The block alignment must equal the channel count times the bit depth
   divided by 8, otherwise `InvalidArg`.
7. The average byte rate must equal the sample rate times the block
   alignment, with checked arithmetic, otherwise `InvalidArg`.
8. The modelled subset is 16-bit PCM or 32-bit float, mono or stereo, at
   44100 or 48000 hertz. Everything else that passed the checks above is
   `NotImplemented`.

**Public.** The callback size of a WAVEFORMATEX is meaningful only for the
extensible tag, so it is ignored for both supported tags. **Xenora** ignores
it in every case.

## Submitting a buffer

`submit_source_buffer` checks in this order:

1. **Public.** WMA buffers are a separate structure and a separate decoder.
   **Xenora** refuses them with `NotImplemented`.
2. **Public.** The buffer flags may be zero or `XAUDIO2_END_OF_STREAM`.
   Any other flag is `InvalidArg`.
3. **Xenora.** A null audio address is `Pointer`, the result code
   `E_POINTER`.
4. **Public.** The byte count must not exceed `XAUDIO2_MAX_BUFFER_BYTES`
   (2147483648). **Xenora** also refuses a zero byte count and a byte count
   that is not a whole number of frames, both as `InvalidArg`.
5. **Xenora.** The address range must not overflow the 64-bit address space,
   which is `InvalidArg`.
6. **Public.** A play length of zero plays the whole buffer, and in that case
   the play begin must be zero as well. Otherwise the play region must end
   inside the buffer. **Xenora** refuses a violation with `InvalidArg` and
   does the arithmetic with checked operations.
7. **Public.** A loop count of zero means no loop region, and then the loop
   begin and the loop length must be zero. The loop count is at most
   `XAUDIO2_MAX_LOOP_COUNT` (254), and `XAUDIO2_LOOP_INFINITE` (255) asks
   for an endless loop. A violation is `InvalidArg`.
8. **Public.** The loop region must satisfy that the loop begin is smaller
   than the end of the play region, that the loop begin may be smaller than
   the play begin, and that the end of the loop region is greater than the
   play begin. **Xenora** also refuses a loop region that ends after the play
   region ends and an empty loop region. A loop length of zero means the
   whole play region is looped, so in that case the end of the loop region
   equals the end of the play region. The public text says the end of the
   loop region must be "less than" the end of the play region; the model
   allows the equal case, because the same paragraph states that a loop
   length of zero loops the entire sample.
9. **Xenora.** A queue holds at most `XAUDIO2_MAX_QUEUED_BUFFERS` (64)
   buffers. A further submission is refused with `InvalidCall`.

## The sample clock

**Xenora.** `consume` advances a fixed sample clock in frames. The clock
only moves while the voice plays, and the call reports the events it reached
in order:

- `BufferStart` is reported once, when the voice first plays a buffer.
- Every frame advances the cursor of the front buffer and the sample count
  of the voice. Frames beyond the last queued buffer are not counted. The
  clock jumps from boundary to boundary (loop end or play end) instead of
  stepping frame by frame, so a call costs time in proportion to its events,
  and slicing the same span into calls of any size gives the same events and
  state (`tests/clock.rs`).
- `LoopEnd` is reported when the cursor reaches the end of the loop region
  and loops remain, either an endless loop or fewer passes than the loop
  count asks for. The count of finished passes grows and the cursor jumps
  back to the loop begin.
- `BufferEnd` is reported when the cursor reaches the end of the play
  region, and the buffer leaves the queue. A buffer that was submitted with
  `XAUDIO2_END_OF_STREAM` is followed by `StreamEnd`, and the sample count
  restarts at zero for the next stream.
- When the end of the loop region and the end of the play region are the same
  frame, the loop end is reported first. A voice with an endless loop over
  its whole play region therefore never reports a buffer end.

**Xenora.** Stopping a voice keeps the buffer it was playing and the sample
count it reached, so a later start resumes where the voice stopped.
**Public.** The public documentation ties the sample count to the stream, so
the model resets it at the end of a stream and nowhere else.

## Voice state

- **Public.** The state flags are zero, or `XAUDIO2_VOICE_NOSAMPLESPLAYED`,
   which asks for the sample count to be reported as zero. Any other flag is
   `InvalidArg`.
- **Public.** The context is the context of the buffer the voice plays, and
   the queue count includes the playing buffer. **Xenora** reports the
   context of the front buffer, and zero when the queue is empty.

## Flushing, volume and frequency ratio

- **Xenora.** Flushing removes the buffers of a voice that are not playing
  and reports a buffer end for each of them, in queue order. A playing voice
  keeps the buffer it plays; a stopped voice loses every buffer.
- **Public.** The volume must be at most `XAUDIO2_MAX_VOLUME_LEVEL`
  (16777216) in absolute value. **Xenora** refuses a volume that is not
  finite or that exceeds that bound with `InvalidArg`.
- **Public.** The frequency ratio must be at least
  `XAUDIO2_MIN_FREQ_RATIO` and at most the maximum ratio the voice was
  created with. A ratio that is not finite or that leaves the range is
  `InvalidArg`.
- **Public.** The operation set argument of a call may be
  `XAUDIO2_COMMIT_NOW` or a later operation set. **Xenora** models only the
  immediate commit, so any other operation set is `NotImplemented`.
- **Xenora.** The start flags must be zero and the stop flags must be zero
  or `XAUDIO2_PLAY_TAILS`. Another flag is `InvalidArg`.

## Guest memory

**Public.** The documentation of the XAUDIO2_BUFFER structure says that the
structure itself may be freed as soon as the submission call returns, and
that the audio data it points to may not be freed until the buffer completes,
as reported by the buffer-end callback, or until the voice is stopped or
destroyed.

**Xenora.** The model follows that ownership rule. The glue that will host
this crate reads guest memory only while a buffer is queued, and it
validates the address range twice: when the buffer is submitted, against the
byte count and the address space of the guest, and again when the buffer is
read for the mixer, because the guest may have moved or unmapped the memory
in between. The model itself keeps the address and the byte count and never
touches the bytes, so a stale or unmapped range cannot reach the mixer
through this crate.

## The guest-visible objects

The loader exposes the bounded model to a guest program. The guest imports
`xaudio2_9.dll`, calls `XAudio2Create`, and calls the engine and its
voices through vtables the loader writes into guest memory.

### The module and its exports

- **Public.** `xaudio2_9.dll` is the name of the XAudio2 library on
  Windows 10 and later. **Xenora** registers two exports in it:
  `XAudio2Create` and `XAudio2CreateWithVersionInfo`. The public header
  declares the first one. The export list of `xaudio2_9.dll` in the Wine
  `xaudio2_9` module specification names the second one with the same
  arguments and a trailing operating-system version, so **Xenora** accepts
  that version argument and records it with the engine. The other exports of
  the real library (`CreateAudioReverb`, `CreateAudioVolumeMeter`,
  `CreateFX`, `X3DAudioCalculate` and `X3DAudioInitialize`) are not
  registered.
- **Xenora.** A null out pointer is `E_POINTER`. A nonzero engine flag is
  `E_NOTIMPL`, because the model implements no engine flag. The out
  pointer is set to null before a call that fails, and an out pointer the
  guest cannot write is `E_POINTER` rather than a fault.

### The interfaces

- **Public.** The vtable of `IXAudio2` holds thirteen entries: the three
  methods of `IUnknown` and the ten methods of `IXAudio2`. The vtable of
  `IXAudio2Voice` holds the nineteen methods the voice interfaces share,
  the vtable of `IXAudio2SourceVoice` holds those nineteen and ten more,
  and the vtable of `IXAudio2MasteringVoice` holds the nineteen and
  `GetChannelMask`. **Xenora** writes the vtables in that order, so a
  guest calls the same method at the same index as the public header
  describes.
- **Public.** The voice interfaces do not inherit `IUnknown` in the public
  header, so they have no `QueryInterface` slot at all. Only the engine
  answers a query, and it accepts `IID_IUnknown` and `IID_IXAudio2` only.
  Anything else is `E_NOINTERFACE` with the out pointer set to null; a
  null out pointer is `E_POINTER`.
- **Xenora.** `AddRef` and `Release` return the new count. The last
  `Release` of an engine releases the engine and every voice it owns.
- **Xenora.** `GetVoiceDetails`, `GetVolume`, `GetFrequencyRatio` and
  `GetChannelMask` are implemented. The channel mask of a stereo mastering
  voice is front left and front right, and the mask of a mono one is front
  center.

### The objects

- **Xenora.** A guest object is one allocation in a private heap the loader
  owns. The first word of the allocation holds the address of the vtable,
  and the vtable follows it in the same allocation.
- **Xenora.** A destroyed voice stays allocated as a tombstone until the
  engine is released. A call through the vtable of a tombstone is refused
  with `XAUDIO2_E_INVALID_CALL` and never touches freed memory. A
  mastering voice that still has source voices cannot be destroyed: the
  call is void, so it writes nothing and records a diagnostic.
- **Xenora.** A call that cannot report an error, such as
  `GetPerformanceData` or `GetChannelVolumes`, writes nothing to its out
  pointer and records a diagnostic.

### Guest pointers

- **Xenora.** Every structure pointer, out pointer and audio range is
  checked before the model sees it. An unreadable or unwritable pointer is
  `E_POINTER`, and the host never faults on a guest pointer. The audio
  range of a submitted buffer must be mapped and readable when the buffer
  is submitted. The bytes are named, never copied.

### The clock until the mixer arrives

- **Xenora.** The mixer of issue #306 owns the real clock. Until it lands,
  every `IXAudio2SourceVoice::GetState` call first advances every started
  source voice of the same engine by one quantum: a hundredth of a second
  of the mastering rate, 480 frames at 48000 hertz, turned into frames of
  the source rate with the frequency ratio of the voice and rounded down.
  A stopped engine does not move its voices. This is a stand-in for the
  mixer and not an audio path: nothing is mixed, resampled or played, and
  no sample is ever read.

## Not implemented

- No mixing and no resampling. The audio path of issue #306 owns those, and
  the engine clock of the loader is a stand-in for it.
- No notification callbacks, no submix voices, no effect chains, no voice
  sends, no reverb or EQ effects, no spatial audio and no WMA or ADPCM
  decoding.
- No device enumeration, no audio endpoint and no operating system audio
  client. The engine has no output device.
- No operation sets beyond the immediate commit, no engine flags, no voice
  flags and no submix or mastering device id.
- No sample rate conversion and no volume ramp. The frequency ratio and the
  volume are recorded and bounded, and nothing else uses them yet.
- The audio processor and the stream category are recorded but are not
  observable through the bounded API.

## Evidence

- The crate's own unit tests and the acceptance test in
  `crates/xaudio2/tests/boundary.rs` pass locally on Apple Silicon.
- The loader runs the original acceptance program of issue #888,
  `fixtures/pe/xaudio2-voices.cpp`, through the ordinary x64 execution
  path on Apple Silicon. The program exits with zero, which means every
  check it makes passed, and the runtime diagnostics report no engine, no
  voice, no queued buffer and no queued byte afterwards. The service-level
  tests in `crates/loader/tests/xaudio2_services.rs` cover the calls the
  program cannot reach.
- No audio is produced, no behaviour was compared with Windows or with
  console hardware. The acceptance program declares the structures
  with the header's one-byte packing.
- Independent review is pending.
