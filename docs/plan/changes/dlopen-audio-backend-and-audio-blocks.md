# dlopen-audio-backend-and-audio-blocks

Audio starts existing. A microphone reaches a processor as timestamped samples it can hand
straight to a model, and a processor's samples reach a speaker — over a backend the wheel never
links, on the one monotonic clock, with the samples riding the bag. This is rung one of the
`[audio-subsystem]` ladder the align adopted: the dlopen spike and the capture/playback
data-model slice, with the resampler, the port window contract, conditioning and the plugin lane
all sequenced behind it. Delta against `../ARCHITECTURE.md` §Media I/O only. Tickets land in
milestone 43 (Audio).

**Scale gate — new behavior plus a changed contract, so this skill.** It adds a bag shape to the
IPC wire and two marker classes plus a cast type to the Python API's public contract, so the ADR
trigger fires — and is already satisfied. `docs/decisions/audio-subsystem.md` was written by the
align that decided these entries, over four research memos in `docs/research/2026-08-26-*.md`;
nothing below decides past it. No RHI primitive is touched, no processor-model rule changes, and
no Vulkan call is added.

**Precondition.** Six of the seven `[audio-subsystem]` entries are DECIDED and this change
implements them (`ARCHITECTURE.md:569-612`). The seventh — audio plugins, `:613-619` — is OPEN
and is not on this change's path: nothing here loads, scans for, hosts or names a third-party
plugin binary, and the out-of-process helper that lane would need is not built or prepared for.
`§Media I/O`'s header carries the section-wide IN-FLIGHT status every worked section carries.

---

## ADDED: §Media I/O — the backend chain is one engine primitive, probed once

- **DECIDED** — The audio device seam is an engine primitive beside the audio clock, not
  built-in-private code: `AudioDeviceBackend` opening `AudioCaptureStream` and
  `AudioPlaybackStream`, living in `runtime/streamlib-engine/src/core/context/` with its Linux
  implementations under `runtime/streamlib-engine/src/linux/`, exactly where
  `core/context/audio_clock.rs` and `linux/audio_clock.rs` already sit. `MicrophoneSource` and
  `SpeakerSink` are written against it and reach no engine guts, which is the layering entry
  (`ARCHITECTURE.md:468-472`) applied to a fourth device class. There is no second audio device
  path: the built-ins, the null backend and every test open streams through this one seam.

- **DECIDED** — The chain is probed once per process and logged once:
  `PipeWireAudioDeviceBackend`, else `AlsaAudioDeviceBackend`, else
  `SilentNullAudioDeviceBackend`. No configuration dial selects a backend, no environment
  variable overrides the probe, and a machine with no audio library lands on the null backend
  rather than failing to import — the wheel must run in `manylinux_2_28` and headless GPU
  containers, which carry no audio libraries at all.

- **DECIDED** — Every audio symbol binds at runtime; the wheel's `DT_NEEDED` set does not grow.
  `libpipewire-0.3.so.0` and `libasound.so.2` are resolved through `libloading`, the pattern
  `runtime/streamlib-engine/src/vulkan/rhi/drm_modifier_probe.rs:206-217` already uses for
  `libEGL.so.1` — a `Library` held beside typed `unsafe extern "C" fn` pointers, a missing
  library demoting to the next arm and a missing symbol named. The versioned soname is the
  dlopen target: this rig ships `libpipewire-0.3.so.0` with no `libpipewire-0.3.so` dev symlink,
  so the `libEGL.so.1`-shaped spelling is the required one, not a stylistic echo. Nothing links
  `cpal`, `pipewire-rs`, or any pkg-config audio crate — each puts an audio library straight into
  `DT_NEEDED` and fails the gate by construction.

- **DECIDED** — SPA's header-only layer compiles into the wheel as a shim that calls nothing.
  PipeWire's pod builders and parsers are inline C with no shared object, so a small
  `cc`-compiled shim owns them, and every `pw_*` entry point it needs arrives as a function
  pointer Rust filled by `dlsym` — the shim itself references no external symbol. This is
  `vendor/tatolab-vulkanalia-vma/build.rs` verbatim in shape: VMA compiles with
  `VMA_STATIC_VULKAN_FUNCTIONS=0` and `VMA_DYNAMIC_VULKAN_FUNCTIONS=0` so it calls only pointers
  Rust hands it, and adds no `DT_NEEDED` entry beyond the C++ runtime. The engine's `build.rs`
  gains a Linux-only `cc` invocation beside its existing `glslc` step.

- **DECIDED** — The headers are vendored, not taken from the build machine. `manylinux_2_28`
  carries no PipeWire development package, so a system-header build is not reproducible where the
  wheel is actually built; this is the same reasoning that pins `shaderc` to `build-from-source`
  so the wheel can never link a `libshaderc.so` that happens to sit on the builder
  (`Cargo.toml:120-128`). MIT-licensed PipeWire and SPA headers land under `vendor/`, untouched
  and unreformatted. The licence obligations are met by machinery that already exists: `MIT` is
  an accepted identifier in `about.toml:25` and `deny.toml`, and the vendored project joins
  `VENDORED_CPP_PROJECTS` in `xtask/src/generate_third_party_notices.rs:98-152` and
  `VENDORED_CPP_PROJECT_NAMES` in
  `sdk/streamlib-python-wheel/tests/test_third_party_notices.py:24-33`, which reproduce its own
  licence text out of the vendored tree the way `VulkanMemoryAllocator` and `Vulkan-Headers`
  already do. `LICENSE`, `LICENSES/` and `docs/license/` are not edited. The shim is our code and
  carries the BUSL header; the vendored headers carry theirs.

- **DECIDED** — The portability gate is the spike's pass/fail, unchanged and unweakened.
  `sdk/streamlib-python-wheel/tests/test_wheel_portability.py:26-37` permits nine host libraries;
  the shipped `_engine.abi3.so` names five of them today (`libstdc++.so.6`, `libc.so.6`,
  `ld-linux-x86-64.so.2`, `libgcc_s.so.1`, `libm.so.6`). After this change it names the same
  five. No name is added to `LIBRARIES_THE_HOST_MAY_SUPPLY` — an audio library appearing there is
  the failure this design exists to prevent, not a fix for it.

## ADDED: §Media I/O — MicrophoneSource and SpeakerSink

- **DECIDED** — `MicrophoneSource` and `SpeakerSink` are native built-ins in
  `runtime/streamlib-media-builtins/`, registered in `register_media_builtin_processor_types()`
  (`src/lib.rs:33-39`) and surfaced to Python as marker classes beside `CameraSource` —
  `#[pyclass(name = "MicrophoneSource", module = "streamlib", frozen)]` in
  `sdk/streamlib-python-wheel/src/python_native_builtin_blocks.rs`, an arm in
  `native_builtin_class_import_path`, an `add_class` line, an `__init__.py` re-export and an
  `_engine.pyi` entry. Configured the one way a built-in is configured:
  `rt.add(MicrophoneSource, config={"device_id": "..."})`. Both are `execution = manual`, the
  mode `CameraSource` uses for a device that paces itself (`camera_source.rs:121`), with
  `scheduling = realtime` — an audio device callback is the deadline `ThreadPriority::RealTime`
  exists for (`sdk/streamlib-processor-schema/src/thread_priority.rs:35`), and the engine's
  existing `linux/rtkit.rs` already carries the RealtimeKit hop, degrading to best-effort in a
  container rather than failing the stream.

- **DECIDED** — The device stamps the block, and the engine never re-stamps it. A capture
  block's timestamp is the backend's own timing for its first sample — `pw_time`-derived status
  minus reported delay on the PipeWire arm, `snd_pcm_status_get_htstamp` on the ALSA arm, with
  `SND_PCM_TSTAMP_TYPE_MONOTONIC` set explicitly so the stamp cannot arrive on `CLOCK_REALTIME`.
  It is published through `OutputWriter::write_with_timestamp`
  (`runtime/streamlib-engine/src/iceoryx2/output.rs:452`), never `write`, whose implicit
  `MediaClock::now()` would stamp the moment of publication rather than the instant of capture.
  Both the bag field and the frame header therefore carry the same device-derived value, in the
  same `CLOCK_MONOTONIC` epoch a `VideoFrame.timestamp_ns` carries — which is the whole of
  block-level A/V sync (`ARCHITECTURE.md:584-587`): joining audio to camera frames is subtracting
  two integers, and no cross-modal machinery is built.

- **DECIDED** — A device callback never blocks, and the loss is counted at the edge. Audio's
  input ports declare `delivery_profile = "lossless"`, which resolves to `Overflow::Block`
  (`runtime/streamlib-engine/src/iceoryx2/delivery_profile.rs:67-86`) — correct for the consumer,
  and fatal if a device callback ever waited on it. So a bounded ring sits between the callback
  and the publish: the callback only ever hands off, a source-owned thread drains the ring into
  `outputs.write_with_timestamp`, and when a stalled consumer fills the ring the source drops the
  oldest block at the device edge and increments its own counter. The loss is explicit in both
  directions — the counter is logged the way `CameraSource` logs its own
  (`camera_source.rs:1128-1139`), and the gap is derivable from the timestamps and sample counts
  of the blocks either side of it. Nothing is silently interpolated and no sample is invented.

- **DECIDED** — The null backend runs the graph and produces silence. Under it `MicrophoneSource`
  publishes silent blocks and `SpeakerSink` discards what it receives, both paced by the timerfd
  `AudioClock` — so a pipeline authored on a workstation runs unchanged in a headless container,
  and a test needs no audio hardware. A device that was named and cannot be opened is the
  opposite case and raises at `setup()`, the way `CameraSource` raises on a missing or
  unpermitted `/dev/video*` (`camera_source.rs:173-197`): a machine with no audio is a supported
  environment, a wrong device id is a wiring error.

## ADDED: §Media I/O — the AudioBlock bag and its cast

- **DECIDED** — `AudioBlock` is the wire contract, and the field names are the contract — the
  same shape `VideoFrame` states for video (`runtime/streamlib-media-builtins/src/video_frame.rs:3-11`):
  an optional cast over a self-describing msgpack named map, declared on no port, registered
  nowhere, ignoring keys it does not read. The keys are `samples`, `sample_rate`, `channels`,
  `sample_count`, `dtype`, `first_sample_timestamp_ns`. `sample_count` counts per-channel samples,
  so an interleaved block carries `sample_count × channels` scalars and the next block's expected
  timestamp is `first_sample_timestamp_ns + sample_count × 1e9 / sample_rate`. `dtype` is
  metadata — `"f32"` default, `"i16"` legal — and `samples` is little-endian, which is a wire
  statement rather than an assumption: it is the property a bag decoded by a tap, a CLI, or
  another language depends on.

- **DECIDED** — `samples` is msgpack `bin`, which the Rust side cannot spell today. `serde_bytes`
  is absent from the workspace, and without it a `Vec<f32>` field goes through `serialize_seq`
  and lands on the wire as a msgpack **array** — five bytes per sample, and a shape Python's own
  `bytes` → `bin` path (`sdk/streamlib-python-wheel/src/python_bag_conversion.rs:300-301,406`)
  does not agree with. So `serde_bytes` enters the workspace and the samples field is annotated
  with it, and the wire-key test asserts `rmpv::Value::Binary` specifically — the audio analogue
  of `video_frame_msgpack_wire_is_a_named_map_with_the_documented_keys`
  (`video_frame.rs:429`), which is the one test that can catch an array-for-`bin` mistake.

- **DECIDED** — The Python cast is pure Python and composes nothing surface-shaped.
  `streamlib.AudioBlock` lives in `sdk/streamlib-python-wheel/python/streamlib/audio_block.py`
  beside `video_frame.py`, is read with `ctx.inputs.read("audio", into=AudioBlock)`, and owes no
  `.pyi` entry — pyright checks it from source, as it does `VideoFrame`. It must not compose
  `ClaimedSurfacePixelAccess`: that class demands a surface-id field in `__init_subclass__` and
  takes claims in `__init__`, and audio has no surface, no claim and no lifetime contract
  (`ARCHITECTURE.md:594-596`). Its `samples` property returns
  `numpy.frombuffer(...).reshape(-1, channels)` over the decoded payload, with numpy imported
  lazily so the wheel still declares no numpy dependency — the pattern
  `python_processor_context.rs:621-655` already uses for `as_numpy`.

- **DECIDED** — "Zero-copy" is a claim about the cast, and is stated as exactly that. Between
  shared memory and `process()` the payload is copied four times today — out of the iceoryx2
  sample (`iceoryx2/input.rs:379`), a header-strip memmove (`:436-440`), the msgpack decode into
  an owned `rmpv` value (`python_bag_conversion.rs:52`), and into a Python `bytes`
  (`:406`) — and this change removes none of them; they are the helper hop every bag pays. What
  the cast guarantees is that it adds no fifth: the numpy array is a view over that `bytes`
  object, and `torch.from_numpy` over it is a view again. At audio's sizes this is the right
  trade and the reason audio touches no surface machinery at all — a 512-sample stereo `f32`
  block is 4 096 bytes against a 16 MiB per-link ceiling for any helper-placed processor
  (`runtime/streamlib-ipc-types/src/lib.rs:43`; the 64 MiB constant at `:38` is the trusted tier,
  which a Python link never gets). No doc, test name, or log line may describe the path as
  zero-copy from the device.

- **DECIDED** — The wheel's own test harness must be able to decode an audio bag, so the defect
  that stops it is fixed at the engine layer. `TestBagCollector` — what `streamlib.testing`'s
  `await_bag` is built on (`python/streamlib/testing.py:204`) — decodes bags into
  `serde_json::Value` (`sdk/streamlib-python-wheel/src/python_test_harness_endpoints.rs:192`),
  whose visitor implements no `visit_bytes`, so every `bin` payload fails to decode with an
  `invalid_type` error. It decodes through `rmpv` instead, the way the tap path already does
  (`python_bag_conversion.rs:52`). This is not audio-specific and is not bandaided in the audio
  code: any bag carrying bytes hits it, including one a Python processor writes today.

## MODIFIED: §Media I/O — the timerfd clock is the deviceless cadence source

The `AudioClock` primitive stays, its role narrows, and its deferral closes.
`runtime/streamlib-engine/src/core/runtime/runtime.rs:494-495` starts it for every runtime that
ever runs — roughly 94 wakeups a second in a graph with no audio in it, invoking nothing. Under
the decided entry it paces deviceless graphs only (`ARCHITECTURE.md:577-583`), so it starts when
something needs it — the null backend, or a test — and a graph whose audio is device-paced never
starts it at all. That is what makes "exactly one cadence source" true in the tree rather than
merely stated: device ticks and timer ticks cannot interleave if the timer is not running. The
four existing clock tests (`core/context/audio_clock.rs:353-390`) keep their invariant unchanged
— a tick lands inside a `CLOCK_MONOTONIC` bracket and is directly subtractable from a frame
timestamp — and that invariant is what the device-paced path must also satisfy.

`runtime/streamlib-engine/src/iceoryx2/delivery_profile.rs:38-41` tells audio to use
`EverySample`. The plan now says audio declares `lossless` (`ARCHITECTURE.md:597-602`) — dropped
samples corrupt speech recognition silently, which is precisely what a drop-oldest profile does.
The doc comment is corrected to match the decision it now contradicts; the profile's own
behaviour is unchanged and other sample streams keep it.

---

## REMOVED:

- REMOVED: FIXME(audio-backend)
  `runtime/streamlib-engine/src/linux/audio_clock.rs:275-282`, the only FIXME in the tree outside
  `docs/`. It defers exactly what this change settles — that a free-running clock's tick time is
  meaningless until a device paces it, and that the capture path's driver stamp is discarded —
  and its own text asserts the backend is OPEN, which stopped being true when PR #1987 merged.
- REMOVED: SyncAction
- REMOVED: sync_action
- REMOVED: are_synchronized
- REMOVED: runtime/streamlib-engine/src/core/sync.rs
  A drop-or-duplicate-the-video-frame A/V sync model with zero callers anywhere in the tree; the
  only reference outside its own file is the re-export at `runtime/streamlib-engine/src/lib.rs:131`.
  The plan decided A/V sync is join-by-timestamp and that no cross-modal machinery exists
  (`ARCHITECTURE.md:584-587`) — leaving a second, dead sync system in `core/` makes that entry
  false in the tree and hands the next reader a parallel abstraction to extend.
- REMOVED: SampledAudio
- REMOVED: sample_audio
  `runtime/streamlib-engine/src/core/observability/perception.rs:40-72` — a second audio sample
  type (`samples`, `sample_rate`, `channels`, `duration_ms`) on a trait with no implementor and
  no caller. `AudioBlock` is the one audio data model; shipping it while this sits in `core/` is
  the parallel-abstraction default-wrong move the doctrine names. The `AgentPerception` trait
  itself stays — its other members are a separate concern and are not touched.

**Blast radius, checked by running the gate's own sweep rather than assumed.** All seven bullets
match only their own definition sites under the sweep's exclusions
(`.claude/scripts/ship-change-removed-gate.sh:49-70`: `vendor`, `docs/plan`, `docs/decisions`,
`docs/learnings`, `examples`, `CHANGELOG.md`, and every consumer entry under `packages/`). No
bullet requires an edit outside the files this change already opens.

**A `cpal` bullet is deliberately not written.** `docs/research/**` is *not* excluded from the
content sweep, and the backend memo names cpal four times as the rejected option
(`docs/research/2026-08-26-audio-backend-linux.md:42-56,89`) — correct evidence that must stay.
The bullet would fail forever. cpal's two live claims in swept engine paths die with this change
anyway: the FIXME's `InputCallbackInfo` reference (`linux/audio_clock.rs:282`) goes with the
FIXME, and `docker/pipewire/10-virtual.conf:3` still asserts "StreamLib's Linux audio is cpal ->
ALSA", which this change corrects in place. The remaining mention is
`.claude/agent-knowledge/linux-media-expert-index.md:11`, harness text that moves in its own PR
per `.claude/rules/flow.md`. All three are grep checks in the validation below instead.

---

## Not in scope

- **The port window contract and the resampler** (`ARCHITECTURE.md:597-602`) — the next rung.
  `MicrophoneSource` publishes at the device's native rate and channel count; `SpeakerSink`
  refuses a block whose rate or channel count it cannot play, loudly, rather than resampling
  silently. Until the windower exists a consumer sees device-quantum blocks and frames them
  itself. This also means the "three things and nothing else" port entry
  (`ARCHITECTURE.md:219-227`) is untouched here — no port gains a new declaration.
- **Conditioning** — no WebRTC APM, no AEC, no noise suppression, no AGC, and `SpeakerSink` gains
  neither immediate cancel nor played-up-to timestamps (`ARCHITECTURE.md:603-612`). Those land
  together on the rung after the windower, because cancel and the AEC reference are one mechanism.
- **Audio plugins** stay OPEN and untouched (`ARCHITECTURE.md:613-619`).
- **`packages/audio`, `packages/opus` and `packages/clap` are read, never edited or deleted.**
  They cannot compile at HEAD and are pre-pivot in form, but their logic is the reference for
  rungs two and three — the `rubato` resampler wiring, the rechunker, the Opus 960-sample framing.
  They are deleted as `REMOVED:` facts when the last of it is re-homed, not before.
- **Apple.** `apple/audio_clock.rs` keeps its GCD timer and no CoreAudio backend is written; the
  platform floor is Linux + NVIDIA (`ARCHITECTURE.md:473-475`). The change must still cross-check
  with `cargo check --target aarch64-apple-darwin`.
- **Making the drop counter visible in `graph`.** `ProcessorMetrics`
  (`core/graph/components/processor_metrics.rs`) is never inserted on any node today, so no
  processor reports drops through the control plane. Wiring it is a control-plane surface
  question for its own change; audio's counter is explicit the way the camera's is.

## Validation

- **The spike's own question, answered by the gate**: `readelf -d` on the built
  `_engine.abi3.so` names the same five libraries after the change as before, and
  `test_wheel_portability.py` passes unmodified. It carries no `requires_gpu` marker, so unlike
  most of this repo's hardware evidence it genuinely runs in CI.
- **A known-signal loopback on the rig**: a `SpeakerSink` plays a generated tone into a PipeWire
  null sink, a `MicrophoneSource` captures it from the paired virtual source, and the test
  asserts the recovered frequency and amplitude — the audio analogue of the vivid virtual camera,
  and the fixture (`runtime/streamlib-engine/tests/fixtures/`) does not exist yet. Alongside it:
  block timestamps advance by `sample_count / sample_rate` within tolerance, and no gap appears
  across a run.
- **Timestamps are the device's, provably** — a capture block's timestamp precedes the moment its
  `write` returns by roughly the device quantum, which a `MediaClock::now()`-at-publish stamp
  cannot satisfy. Absent this assertion the regression is invisible.
- **CI-gated without hardware**: the null backend runs a graph end to end and produces silent
  blocks; the `AudioBlock` wire keys are locked including `Value::Binary` for `samples`; the cast
  round-trips through real wired iceoryx2 ports the way `test_read_into_target.py:75-321` does;
  `await_bag` returns an audio bag, which fails today.
- **A Python processor reads a microphone block as a numpy array** and the array is a view over
  the bag's bytes, not a copy of them.
- Mechanical: zero hits for the seven `REMOVED:` bullets under the gate's sweep; no `cpal` claim
  left in `docker/`; no audio library name added to `LIBRARIES_THE_HOST_MAY_SUPPLY`; no
  `CLOCK_REALTIME` spelling anywhere in the backend (`cargo xtask check-clock-usage` scans
  `runtime/`); `cargo deny check licenses` and the third-party-notices test green with the
  vendored headers in the tree.
