# media-built-ins-become-modules

> **Approved by the owner, 2026-11-02**, with its six decisions as recommended ("I approve your
> decisions and recommendations").

The runtime ships its media nodes as built-ins: Rust statically linked into `tatolabd`, reaching
Python only as generated classes. The owner retires that shape. In the owner's words
(2026-11-02): built-ins in the engine "disprove the main principle that other people can build
on top of their own custom hardware"; they do not dogfood the product, because "we never solved
how we can act as a consumer and implement a camera"; they hide what is missing in extensibility;
and they assume every machine needs a camera, "when that should be a type of python module we
deliver instead when they need it" — the runtime moves toward a general-purpose engine in the
manner of Holoscan.

**After this change:**
- **The runtime ships no media nodes.** Camera, microphone, speaker and the video and audio
  codecs are first-party extension wheels, built exactly the way a third party would build one:
  a Python node whose per-frame work runs in the wheel's own native code, in that node's
  processor interpreter.
- **A module's native threads reach ports without Python.** The lent `tatolab.runtime`
  implements one small, versioned host table — port read, write and wait, logging, clock, stop
  flag, host capabilities — handed to a wheel's native half as a named capsule. Python never
  runs at frame or sample cadence in a module.
- **Whatever a first-party module needs from the engine becomes public API** that a third-party
  module can use too: surface import by plane layout, colour conversion, the audio device
  interface, the capture-prompting policy.
- **What a stream author sees:** `from tatolab.camera import CameraSource` instead of
  `from tatolab.stream import CameraSource`, a `pip` dependency in the project, and the same
  `stream_builder.add(...)`. A stream with no camera loads no camera code at all.

**Scale gate — this skill plus an ADR.** It changes the processor model's extension surface, the
Python API's public contract (built-in classes leave `tatolab.stream`), and adds a binary
interface between the lent wheel and extension wheels. The ADR is written once the decisions
below are settled.

**Precondition.** Every entry touched is DECIDED and SHIPPED. This change flips §Packages &
extension model and §Media I/O to IN-FLIGHT.

## Current state (recon, 2026-11-02, `main` `ab8bdae05`)

Full reports, with every citation, live in the project's research folder; the load-bearing facts:

**What forces first-party capabilities into the engine today**
- **Ports are unreachable from a wheel's native code.** The helper's data plane lives inside
  the lent `_engine` (`sdk/streamlib-python-wheel/src/python_processor_link_data_access.rs:50`);
  a separately built wheel cannot downcast its objects, so every message crosses Python as
  `bytes`.
- **Logging:** each extension carries its own `tracing` with no subscriber in a helper;
  `packages/streamlib-webrtc/python/tatolab/webrtc/processors.py:57,65` moved a retry loop into
  Python for that reason.
- **Surfaces:** `import_dma_buf` refuses multi-plane formats (`gpu_context.rs:1066`), no
  `import_iosurface` exists, and kernels cannot bind an imported buffer
  (`subprocess_escalate/surface_bound_kernel_binding.rs:225-245`).
- **Codecs:** Vulkan Video sessions are `pub(crate)` (`gpu_context.rs:3444`, `:3470`) and
  `streamlib-consumer-rhi` excludes video queues by design (`consumer-rhi/src/lib.rs:39-44`).
- **Discovery:** an installed extension's nodes reach the node catalog only after a graph
  names them.

**What the built-ins actually need**
- **Microphone and speaker:** the device callback only fills a bounded ring
  (`streamlib-media-builtins/src/microphone_source.rs:258-268`); a separate thread does the
  iceoryx2 write (`:270-364`). The speaker mirrors it (`speaker_sink.rs:347-439`). Every link
  is iceoryx2 shared memory plus a notify, even between two built-ins in one process
  (`iceoryx2/output.rs:416-501`). Moving callback, ring and publisher into a helper keeps the
  deadline inside one process. No audio clock master exists: `MediaClock` is the machine's
  monotonic clock (`core/media_clock.rs`), and a device stamps its own blocks.
- **Camera:** imports V4L2 buffers as DMA-BUFs (`linux/v4l2_video_device_backend.rs:690-726`)
  or IOSurfaces, converts with the engine's colour converter
  (`core/context/captured_video_frame_to_pooled_rgba_conversion_stage.rs:74-247`), and
  publishes a pooled pixel buffer. A helper child inherits the responsible process for macOS
  camera permission (`docs/research/2026-09-19-macos-camera-display-revival.md:465`).
- **Codecs:** decode is already not zero-copy — it reads RGBA back to the CPU
  (`vulkan/video/decode/mod.rs:928`). Opus is libopus on the CPU, bytes in and out.

**Prior art.** JACK, PipeWire, Chromium's audio service, CoreAudio's driver host and Android
all run the real-time device callback in a different process from the producer, over shared
memory with direct peer wakeups. Holoscan keeps its core free of domain operators, which ship in
HoloHub as modules; React Native's JSI and TurboModules hand handles, never serialized frames,
across the language boundary.

## ADDED

- **§Packages & extension model — the module host table.** The lent `tatolab.runtime`
  implements one `#[repr(C)]`, append-only function table, versioned major/minor/size, handed
  to an extension wheel's native half as a named capsule from a node's context. v1 carries: open,
  write, read and wait on a port handed to native code, from any thread, without the GIL; the
  catalog entry; log records into the helper's existing log ring; the monotonic clock; the
  node's stop flag; host capabilities. Python hands named ports to native code, and a later
  Python write to such a port is refused by name. A module built against minor N loads on any
  runtime at minor ≥ N and is refused by name on an older one.
- **§Packages & extension model — `tatolab-module-sdk`.** A published crate carrying the
  table's struct mirror, layout regression tests, safe Rust wrappers and a generated C header,
  with optional `pyo3` and `napi` features. It depends on no engine crate.
- **§Packages & extension model — discovery.** A wheel declares its nodes in a `tatolab.nodes`
  entry-point group; the node catalog lists installed modules' nodes, read in a stream's own
  interpreter, never in `tatolabd`.
- **§Graphics — public surface imports.** `import_dma_buf` takes a plane layout (NV12, YUYV:
  per-plane offset and stride) and marks the surface as written by a foreign device;
  `import_iosurface` is its macOS twin; `convert_surface_into` exposes the engine's colour
  converter to a helper.
- **§Media I/O — the audio device interface reaches modules.** Opening a device stream, its
  format and period, liveness, and start and stop with a native callback, exported through the
  host table, so a module reuses the engine's PipeWire, ALSA and CoreAudio arms instead of
  carrying a second audio device path. `AudioDeviceStreamRequest` gains a requested format.
- **§Media I/O — capture prompting reaches helpers.** `capture_device_access_prompting` is
  readable on a helper's context, so a module refuses a prompt the stream's load did not allow.

## MODIFIED

- **§Packages & extension model, the built-in criterion** (`ARCHITECTURE.md:512-524`) is
  retired: no first-party capability is a built-in by virtue of a deadline, an engine-only
  primitive, or being first-party. Anything a first-party module needs is public engine API.
- **§Packages & extension model, "A Rust-side extension SDK is not owed by the first
  extension"** (`:534-537`): it is owed now, as `tatolab-module-sdk`.
- **§Packages & extension model, "the CPython ABI is the only binary boundary"**
  (`:477-485`, `:505-506`): the host table is a second, narrow boundary — data-plane verbs and
  plain handles only, never an RHI object. See D2.
- **§Processor model, built-in node type** (`:1495-1504`) and the generated classes
  (`:764-770`): media classes leave `tatolab.stream`; a module node's type is its own import
  path, e.g. `tatolab.camera:CameraSource`.
- **§Processor model, failure isolation** (`:1629-1634`): a native crash in a camera, codec
  or audio node ends that node's stream, not every stream on the machine.
- **§Media I/O, first-party camera and audio** (`:2090-2098`), the audio built-ins entry
  (`:2657-2686`) and "device audio callbacks stay native, always" (`:2286-2290`): device and
  codec nodes are extension wheels; a device callback runs on a native thread in the node's
  helper process and never enters Python.
- **§Media I/O, device permission** (`:2941`): "only the runtime process opens devices, never
  a processor interpreter" becomes "a device is opened by the node that uses it, in its own
  process, under the stream's capture-prompting policy".
- **§Media I/O, the virtual camera** (`:2099-2110`): `VirtualCameraSink` is an extension wheel
  like every other device node; installing the runtime alone no longer gives other
  applications a camera (D5).
- **§Media I/O, `match_device`** (`:2643-2656`): a device stream request carries a requested format, so a speaker needs no `match_device`; the sentinel stays engine-internal (D4).

## REMOVED

- REMOVED: runtime/streamlib-media-builtins/src/camera_source.rs
- REMOVED: runtime/streamlib-media-builtins/src/microphone_source.rs
- REMOVED: runtime/streamlib-media-builtins/src/speaker_sink.rs
- REMOVED: runtime/streamlib-media-builtins/src/h264_encoder.rs
- REMOVED: runtime/streamlib-media-builtins/src/h264_decoder.rs
- REMOVED: runtime/streamlib-media-builtins/src/h265_encoder.rs
- REMOVED: runtime/streamlib-media-builtins/src/h265_decoder.rs
- REMOVED: runtime/streamlib-media-builtins/src/opus_encoder.rs
- REMOVED: runtime/streamlib-media-builtins/src/opus_decoder.rs
- REMOVED: runtime/streamlib-media-builtins/src/test_pattern_source.rs
- REMOVED: runtime/streamlib-media-builtins/src/virtual_camera_sink.rs
- REMOVED: tatolab.stream:CameraSource
- REMOVED: tatolab.stream:MicrophoneSource
- REMOVED: tatolab.stream:SpeakerSink
- REMOVED: tatolab.stream:H264Encoder
- REMOVED: tatolab.stream:H264Decoder
- REMOVED: tatolab.stream:H265Encoder
- REMOVED: tatolab.stream:H265Decoder
- REMOVED: tatolab.stream:OpusEncoder
- REMOVED: tatolab.stream:OpusDecoder
- REMOVED: tatolab.stream:TestPatternSource
- REMOVED: tatolab.stream:VirtualCameraSink

`RelayTrackSource` stays in the runtime as transport (D5). The engine's audio device arms and
colour converter stay and are exported (D4); the Vulkan Video port leaves the engine for the
crates D3 names, and its removal bullets are written when that tracer is ticketed.

## Decisions

**Decided D1 — The direction, in five sentences** (owner, 2026-11-02).
1. The runtime ships no media nodes; it is a general-purpose engine.
2. Camera, microphone, speaker and codecs become Python nodes with native Rust halves, built as
   a third party would build them.
3. They ship as optional first-party wheels, installed when a stream needs them.
4. Anything a module needs that the engine does not expose becomes public engine API, never a
   private shortcut.
5. TypeScript, when unpaused, follows the same model with the same native halves.

**Decided D2 — The host table: (a)** (owner, 2026-11-02). The plan rejected a narrow native ABI for
extensions; the plugin ABI was deleted in #1804. Options: (a) the versioned capsule table above,
implemented once in the lent wheel, data-plane only, guarded by a lint that refuses RHI types in
it; (b) no table: native threads take the GIL for every port write, uncontended because one
helper runs one node; (c) a module opens its own iceoryx2 and surface-share connection — a
second data plane matching the runtime's layout byte for byte. **Recommendation: (a).** (b) is
enough for a camera at 60 fps but puts Python's GIL on every audio block; (c) duplicates the
data plane. Unlike the plugin ABI, the engine never calls module code, nothing is dlopen'd into
the runtime, only plain data and generation-checked handles cross, and a version is accepted
by minor, never by exact build.

**Decided D3 — Video codecs run on a device the module owns: (a)** (owner, 2026-11-02). (a) The module's native half owns its own
Vulkan device on the runtime's GPU, matched by UUID, with video queues, and imports frames
through `export_opaque_fd`. The Vulkan Video port moves out of the engine into an owned-device
tier of `streamlib-consumer-rhi` plus a `streamlib-vulkan-video` crate, both published; `rhi.md`'s
"helper Vulkan is import-only" changes. (b) Sessions stay an engine primitive behind the host
table, at one round trip to the parent per frame. **Recommendation: (a),** confirmed by the
first codec tracer's cost table (memory and startup of a second device, timeslicing under 1, 2
and 4 encoders). A driver fault in the video engine then ends one helper instead of the
runtime's device. VideoToolbox and Opus go into their modules under either option.

**Decided D4 — The audio device arms stay in the engine and are exported; a device stream takes a requested format** (owner, 2026-11-02). Keep the PipeWire, ALSA
and CoreAudio arms in the engine and export them through the host table (one device path,
recommended), or move them into the audio module. Separately: give `AudioDeviceStreamRequest` a
requested format so a speaker needs no `match_device` (recommended), or export settling
`match_device` from a helper.

**Decided D5 — Test pattern and virtual camera move last; the relay reader stays** (owner, 2026-11-02). `TestPatternSource` meets no criterion and can
be Python plus GLSL; the scaffold, the codec PSNR rig and about fifteen runtime test streams use
it, so it moves last. `VirtualCameraSink` is a built-in so that installing the runtime alone
gives other applications a camera. `RelayTrackSource` is "the only way a graph reads a relay"
and pins its MoQ crates to `tatolabd`'s publisher. **Recommendation:** move the test pattern
and the virtual camera last, keep the relay reader in the runtime as a transport, not a media
node.

**Decided D6 — Golden graphs are re-recorded with each module's type** (owner, 2026-11-02). Recorded and golden graphs carry
`tatolab.stream:CameraSource`. Pre-1.0 allows no shim. **Recommendation:** re-record every
golden graph with the module's type in the change that moves each node, and let an old
persisted graph refuse by name naming the module to install.

**OPEN, recorded, not built — placement per host.** iOS forbids spawning processes, and a
small embedded host may not afford one interpreter per node. Whether a host profile may run
processors as threads is a reversal of the placement ban for that profile, and is decided only
when such a host is scheduled. Nothing in this change depends on it.

## Tracer bullets

1. **Three proofs with no engine change.** A microphone wheel capturing ALSA into a lock-free
   ring, A/B against the built-in under GIL churn and CPU stress; a Linux camera wheel with CPU
   upload and its own GLSL conversion, run in the scaffold; a `tatolab-opus` wheel.
2. **The host table and `tatolab-module-sdk`**, with `tatolab-webrtc` moved onto it and
   entry-point discovery in the node catalog.
3. **Camera module**, zero-copy on Linux and macOS, deleting `CameraSource` in the same change.
4. **Audio module**, microphone then speaker, deleting both built-ins in the same change.
5. **Codec module**, H.264 encode first under `D3`'s cost table, then decode, H.265,
   VideoToolbox, and deletion of the codec built-ins.
6. **Remaining nodes** per `D5`, and starter wheels shipped in the runtime unit so a new project
   runs offline.

Converted consumers that break — the six `examples/camera-*`, `microphone-reverb-speaker` and
`audio-mixer-demo` — get backlog at each consumer, never a block on the
engine change.
