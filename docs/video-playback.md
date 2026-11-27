# Native video playback foundation

Xenora's first video middleware slice is a small native macOS playback/decoder
abstraction in the Swift application layer. It uses only public AVFoundation,
Core Media and Core Video interfaces. It is not a Bink implementation and does
not claim any console-specific or game video compatibility.

## Current behavior

`VideoPlayback` loads one local asset, selects its first video track and exposes
duration, natural size and nominal frame rate. It creates an
`AVAssetReader`/`AVAssetReaderTrackOutput` pair that decodes video samples to
32-bit BGRA pixel buffers. The object is main-actor isolated, which serializes
state transitions and reader replacement rather than relying on unsynchronized
shared mutable state.

The state machine is intentionally explicit:

- `ready`: the asset has a video track and a zero-time decoder is configured;
- `playing`: `nextFrame()` may decode one frame;
- `paused`: decoding is cued but `nextFrame()` is rejected;
- `ended`: end of stream was reached and remains stable until an explicit seek;
- `failed`: the reader reported failure;
- `closed`: terminal cleanup state; further playback operations fail.

`seek(to:)` accepts a valid numeric time in the half-open range `[0, duration)`,
replaces the reader and enters `paused`. Seeking to or beyond the exact end,
negative times, invalid times and nonnumeric times fail with `invalidTime` in
this slice. This conservative range avoids silently restarting from zero when a
caller asks outside the media timeline.

Decoded frames retain their `CMSampleBuffer`, so the pixel-buffer ownership is
tied to the sample object rather than to decoder internals. Presentation
timestamps come from the sample. When AVFoundation returns an invalid sample
duration for a decoded frame, the object reports the nominal frame duration
instead of propagating NaN into caller-visible timing.

## Metal texture upload

`VideoTextureUploader` is a deliberately small copy-based bridge from decoded
32-bit BGRA `CVPixelBuffer` frames to native `MTLTexture` objects. It validates
the pixel format and dimensions, creates a non-mipmapped `bgra8Unorm` texture
with shader-read usage, locks the source buffer read-only and copies rows while
preserving the source row stride. The returned texture owns its uploaded texels
independently of the sample buffer after `replace` completes. This is upload
infrastructure only: it does not sample, present, schedule, color-convert or
synchronize frames.

## Verification

`app/Tests/XenoraTests/VideoPlaybackTests.swift` generates a small H.264 MOV
fixture at test time using AVAssetWriter. The media is independently generated
by the test harness and is not committed or copied from another project. The
tests cover:

- decode order and presentation timestamps for five frames;
- fallback frame durations and pixel-buffer availability;
- seeking to a later timestamp and the explicit paused transition;
- invalid seek-time negative controls;
- rejection of non-video input;
- repeated create/close cycles and terminal use-after-close failure;
- exact row-major texture bytes from an independently generated BGRA control;
- rejection of a non-BGRA pixel buffer before texture creation;
- upload of one decoded fixture frame to a native Metal texture.

The tests run natively on Apple Silicon through Swift's release test target.
They are local AVFoundation integration evidence only. They do not establish
real-time playback behavior, general codec compatibility or console middleware
compatibility.

## Explicit limitations

This slice does not implement zero-copy texture import, a real-time playback clock,
frame dropping, audio decoding, audio synchronization, streaming/network input,
subtitles, multiple video tracks, rotation/transform handling, color-space
conversion policy, DRM-protected media, Bink video or any undocumented console
ABI. H.264 is used only as a locally generated controlled codec fixture; codec
support remains whatever the host AVFoundation installation provides.
