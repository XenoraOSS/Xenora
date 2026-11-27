# Middleware compatibility coverage

This inventory distinguishes what Xenora does today from what public-interface
research merely makes possible. “Implemented” means guest- or host-visible code
exists now. “Locally verified” means the project has tests or probe evidence on
native Apple Silicon; it does not mean independent acceptance, Windows comparison,
Xbox hardware agreement or game compatibility.

## Networking and HTTP

| Area | Current status | Evidence and limits |
| --- | --- | --- |
| Winsock loopback | Implemented, partial | TCP loopback streams, datagram primitives, select, shutdown, limited options, localhost-only name forms and AMD64 ordinal aliases for implemented named services exist. External routing, DNS truth, TLS and Internet reachability do not. See the Winsock entries in `CHANGELOG.md` and the service inventory. |
| WinHTTP object layer | Implemented, partial | Session, connection, request and close-handle ownership exist with bounded UTF-16 copying and honest refusal of unsupported async/proxy/transport flags. This layer alone does not perform HTTP. |
| WinHTTP synchronous cleartext transport | Implemented, partial | One direct HTTP/1.1 exchange with `Connection: close`, bounded response parsing, selected header queries, body reads, chunked decoding and timeout handling exist. TLS, reuse, redirects, authentication, proxying, callbacks, cancellation, streaming writes and most transfer-coding combinations remain unsupported. Evidence is controlled loopback testing on Apple Silicon only. |
| XNetworking connectivity hint | Implemented, very narrow | `XNetworkingGetConnectivityHint` honestly reports process-local loopback capability with no external route. It must not be read as endpoint reachability or Xbox network compatibility. |
| libcurl-compatible interfaces | Unsupported | No libcurl guest ABI or compatibility shim is implemented. Public concepts may inform design, but no surface is counted as present. |

## Upscaling and frame generation

| Area | Current status | Evidence and limits |
| --- | --- | --- |
| AMD FidelityFX public SDK research | Research only | `docs/research/fidelityfx-metal.md` records a pinned public-revision license and Metal-equivalence reading. No AMD implementation source, shader or asset was copied. |
| FidelityFX temporal upscaling or frame generation | Unsupported | No Xenora implementation exists. The default binary-only SDK license and exact-file MIT-list requirements mean every future use requires file-level license review before copying or adaptation. |

## Ray tracing support technology

| Area | Current status | Evidence and limits |
| --- | --- | --- |
| Metal acceleration-structure research | Research only | `docs/research/metal-acceleration-structures.md` documents public descriptor, sizing, scratch ownership and ordering constraints. |
| Host acceleration-structure probe | Implemented, very narrow | A local probe can detect runtime support and repeatedly build one original single-triangle primitive acceleration structure through public classic Metal APIs. It does not perform intersections, rendering, refit/compaction, performance validation or console translation. |
| Xbox ray-tracing ABI compatibility | Unsupported | No console-specific acceleration-structure ABI or game-facing translation exists. |

## Video and media

| Area | Current status | Evidence and limits |
| --- | --- | --- |
| Native video playback foundation | Implemented, partial | `docs/video-playback.md` documents AVFoundation-backed single-track decode, lifecycle state, serialized seek/reader replacement, timestamps and terminal cleanup, with local generated H.264 evidence. |
| Metal texture video upload | Partial foundation | A bounded copy-based upload path exists for decoded 32-bit BGRA buffers into shader-read Metal textures. Zero-copy import, presentation, color conversion and real-time scheduling remain unsupported. |
| Bink or console video ABI | Unsupported | No Bink decoder, RAD Game Tools integration, console-specific video ABI, DRM handling or game video compatibility exists. |
| Audio/video synchronization | Unsupported | No synchronized presentation pipeline exists. XAudio2 work is tracked separately and is not evidence of media playback. |

## Licensing and clean-room boundaries

- Use only public documentation, public SDK text with verified file-level licenses,
  and authorized materials.
- Do not copy AMD FidelityFX implementation files unless the exact file license
  permits it and the provenance record names the repository revision and file.
- Do not use leaked console SDKs, restricted console tools, proprietary game
  binaries, shader collections, firmware, keys or confidential documentation.
- Public API compatibility research is not permission to copy implementation
  code. Xenora's emulator implementation remains original.
- A loopback, synthetic or host-probe result is local evidence only. It is not
  independent acceptance, Windows comparison, Xbox hardware evidence or game
  compatibility.

## Immediate documentation rule

When any middleware surface changes, update the detailed interface document, the
changelog and the relevant row here in the same change. Do not add placeholder
APIs to this inventory as if they were verified coverage.
