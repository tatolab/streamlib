# Audio subsystem: sensor-fusion-first, PipeWire-dlopen, samples in the bag

Rationale for the `[audio-subsystem]` entries in `docs/plan/ARCHITECTURE.md` §Media I/O,
decided 2026-08-26. Evidence: four research memos from the 2026-08-26 exploration
(`docs/research/2026-08-26-*.md`), whose load-bearing claims were independently
re-verified in-session.

## Trigger

Read this before building any audio capture, playback, resampling, windowing, or
conditioning path; before adding an audio dependency to any crate the wheel links;
before proposing plugin hosting; or before giving audio any surface-id-shaped
machinery.

## The reframe this rests on

StreamLib is for physical AI. The audio subsystem exists so timestamped audio can be
fused with other sensors and fed to ML processors — perception (VAD, wake word, ASR,
sound events) and interaction (TTS playback, barge-in), not audio production. Owner,
2026-08-26: "the goal is an audio subsystem processors can use that maintains realtime
performance"; the one-wheel `pip install streamlib` experience is load-bearing. The
prior framing (converting `packages/audio` / `packages/clap`) was retired: those
packages depend on deleted crates and cannot compile; their logic is reference
material, their form is the pre-pivot model.

## Decision rationale

**Backend chain (PipeWire → ALSA → null, all dlopen).** The wheel's `DT_NEEDED` gate
permits nine host libraries; every *system* audio library — one the host machine may
or may not supply, `libpipewire`, `libasound` — must therefore bind at runtime.
Vendored code the wheel compiles in (WebRTC APM below, like shaderc before it) is
the other side of the same portability rule, not an exception to it: static linking
adds nothing to `DT_NEEDED`. The
supposed blocker — PipeWire's SPA layer being macro/inline-heavy — is a build-time
concern only: SPA is header-only with no shared object, so it compiles into the wheel
while the ~33 `pw_*` symbols bind via dlopen. SDL3 ships exactly this split as its
default Linux configuration; cubeb proves the ALSA arm (~40 symbols). CPAL links
`libasound` into `DT_NEEDED` and its new PipeWire host links too — unusable, no dlopen
path planned. Going through ALSA-the-API alone was rejected even though stock Ubuntu
routes it into PipeWire (verified: `pcm.!default` is `type pipewire`), because the
compat plugin synthesizes timestamps (measured: `htstamp` ≈ "now", `audio_htstamp` =
0) and forfeits device sharing on Debian/headless plus PipeWire-only devices
(Bluetooth). The null backend is not politeness: headless GPU containers carry zero
audio libraries (verified in `ubuntu:24.04` and `manylinux_2_28`), and the wheel must
import and run there.

**Device paces; the timerfd clock serves deviceless graphs.** A free-running clock's
tick time is meaningless once a real device exists — the deferred
`FIXME(audio-backend)` in `linux/audio_clock.rs` said exactly this, and PipeWire's own
model is driver-node-paced. Timestamps derive from the backend's timing
(status/`pw_time` minus reported delay) in `CLOCK_MONOTONIC` — the same epoch V4L2
stamps carry, which is what makes fusion join-by-timestamp. Hardware DMA timestamps
are opportunistic, not foundational (PipeWire itself ships `api.alsa.htimestamp =
false` for driver-trust reasons).

**Samples in the bag; no surface machinery.** Audio is ~4,000× smaller than the video
the engine moves (a 512-sample f32 window is 2 KB against a 64 MB link ceiling; 16 kHz
mono f32 is 64 KB/s). Every surveyed stack hands audio around as CPU bytes/float32;
no DLPack-for-audio practice exists anywhere; the CPU-bound perception models (VAD,
wake word) are designed for CPU, and where GPU inference matters the ML framework's
own ~80 µs H2D copy is noise. GPU-resident audio buffers were rejected as pure loss.
With samples inline, audio needs none of the surface registry, claim protocol, or
lifetime contract — the asymmetry with `VideoFrame` is deliberate, not an oversight.
`f32` default with `i16` legal matches the model split (float models vs the int16
C-API generation: WebRTC VAD, Porcupine).

**The window contract is the API centerpiece.** Devices deliver quantum-sized blocks
(PipeWire default 1024 samples); models demand exact windows (Silero asserts on
non-512; WebRTC VAD on non-10/20/30 ms; wake word wants ~1 s rolling). Every
framework re-solves this privately (openWakeWord buffers internally, Pipecat chunks in
transports); none puts window/hop in the port contract. Declaring rate / channels /
dtype / window / hop on the input port — engine-inserted resample, mixdown, framing —
is the game-engine abstraction the owner asked for and composes with the decided
port-local channel-policy grammar.

> ~~the existing `lossless` delivery profile covers audio's no-silent-drops requirement
> (drops corrupt ASR silently; `latest` is wrong for audio).~~ — Superseded 2026-08-28 by
> `delivery-profile-vocabulary.md`. `lossless` never delivered that guarantee and is
> retired; audio declares `ordered`, and the no-silent-drops requirement is met by
> counting every drop and surfacing it, not by a profile that promises none.

Order still matters for audio and `newest` is still wrong for it. Feature extraction stays out because every model ships its own extractor tuned
to its training statistics (Whisper's `(log_mel+4)/4`, AST's AudioSet mean/std) — a
generic mel block would be subtly wrong for each.

**Conditioning on the built-ins, engine-internal.** AEC requires the mic signal and
the speaker reference sample-aligned with drift compensation; the engine owns both
ends, so the chain runs between device and published block — a graph hop in between
would make alignment the user's wiring problem. WebRTC APM is BSD-3-Clause with an
explicit patent grant (the component PipeWire's own echo-cancel wraps), statically
linkable in a BUSL-1.1 wheel. Bypassable because XMOS-class array mics (ReSpeaker
Lite, HA Voice PE) condition in hardware before the host sees audio. `SpeakerSink`'s
immediate cancel + played-up-to timestamps serve barge-in (~200 ms human turn-gap
target) and feed the AEC reference — one mechanism. Soft realtime is the calibrated
target: capture sits at ~3% of a conversational budget (LLM inference dominates), so
the invariants are no silent drops (every drop counted and surfaced, per
`delivery-profile-vocabulary.md`) and AEC alignment, not sub-5 ms heroics.

**Audio plugins: OPEN with direction, strict admission test.** No surveyed physical-AI
or voice system hosts audio plugins; the DSP they need ships as permissive libraries
(APM; DeepFilterNet is MIT/Apache and Rust). But the door is provably cheap to keep
open: `clack-host` (MIT/Apache, crates.io) and coupler-rs `vst3` add zero `DT_NEEDED`
(pure Rust, dlopen), Steinberg relicensed the VST3 SDK to MIT with SDK 3.8 (Oct
2025), and Spotify's `pedalboard` ships 15 Linux wheels on PyPI hosting VST3 —
precedent that the one-wheel constraint survives. When built: **out-of-process only**
— an in-process plugin segfault kills CPython, the engine, the Vulkan device, and the
pipeline; an SHM round trip costs ~20–100 µs against a 5–11 ms block, and the shape is
the helper-process doctrine applied to another foreign binary. **Project-local
declaration** is the owner's shader-precedent ruling: a plugin ships in or is
referenced by the app's own project so packaging the project makes it run elsewhere;
machine-global scan paths (`~/.clap`, `/usr/lib/clap`) — every DAW's shape — are at
most a convenience, never the model. CLAP first because its Rust host tooling is the
only mature one and the repo's dead macOS host logic transfers; LV2 last because its
published crates dynamically link system `lilv` and would need a vendored ISC static
build.

## Rejected alternatives

- **CPAL (any backend)** — links audio libraries into `DT_NEEDED`; fails the wheel
  gate by construction.
- **ALSA-API-only backend** — synthesized timestamps through the compat plugin,
  exclusive device grabs where `pipewire-alsa` is absent, no PipeWire-only devices.
  Survives as the fallback arm, not the model.
- **Backend outside the wheel** — contradicts the decided built-ins-in-the-wheel
  entry and reintroduces lag-by-design for audio alone. (After 2026-09-04 the rejection
  rests on the criterion in `extension-model.md` — a device audio callback is a deadline
  the helper hop cannot meet, and the audio built-ins had a named consumer — not on a
  built-ins-by-default rule.)
- **Engine-clock-paced devices** — fights every backend's design and re-creates the
  clock-vs-device drift the FIXME documents.
- **GPU-resident audio / DLPack audio handoff** — no consumer wants it at audio's
  sizes; a GPU round trip exceeds the block budget it would serve.
- **Surface-id-shaped audio lifetime machinery** — solves a problem audio does not
  have; pool-depth transit already bounds an inline bag.
- **Native mel/MFCC blocks** — per-model extractor statistics make a generic one
  wrong for each; the contract ends at windowed raw samples.
- **A separate conditioning graph block** — breaks mic/reference alignment by
  inserting a hop the engine cannot order; may return later as an *additional* door,
  never the blessed path.
- **In-process plugin hosting** — pedalboard's shape, and its documented caveat
  (plugins "may even crash the Python interpreter without warning"); unacceptable
  blast radius for a robot's sensor pipeline.
- **Native beamforming / DoA** — arrays increasingly condition on-chip; ODAS (MIT) is
  wrappable as an ordinary processor if a consumer ever needs it.
- **Piper TTS integration** — relicensed GPL-3 (`piper1-gpl`); never linked, helper
  isolation only if ever wanted.
