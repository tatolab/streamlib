# Codec blocks

Rationale for the `[codec-blocks]` entries in `docs/plan/ARCHITECTURE.md` §Media I/O
(decided 2026-08-31). Decisions live in the plan; this records why.

## The starting point is proven machinery, not a greenfield

The ~52k-line `runtime/streamlib-engine/src/vulkan/video/` tree (Rust port of NVIDIA's
vk_video_samples: H.264/H.265/AV1 encode, H.264/H.265/AV1/VP9 decode, NV bitstream
parser, DPB/POC/MMCO, GOP, rate control, PSNR) was proven end-to-end with recorded
numbers before the SDK pivots stranded its consumers:

- PR #328 (2026-04-19): fixture PSNR rig, 8/9 references ≥ 35 dB for both codecs.
- PR #827 (2026-05-16): all 9 references PASS, lowest-Y 43.16 dB (h264) / 43.07 dB
  (h265), vivid colorimetry drift 0.0000; the drift lock is still checked in at
  `runtime/streamlib-engine/tests/fixtures/psnr_vivid_baseline.tsv`, re-measured
  under #2085 when the rig stopped sampling the display's composited output.
- Real-hardware runs on a Cam Link 4K surfaced #756/#335 (shutdown/queue races) —
  failure modes only running code produces.
- The encoder's bitstream fed a verified MoQ A/V round-trip through Cloudflare's
  public relay and verified WebRTC WHIP publishing to Cloudflare Stream.

Consumers reached it through three successive APIs (typed structs → cdylib packages →
plugin-SDK sessions), all deleted by the pivots. `GpuContext::create_encoder_session`
/ `create_decoder_session` survive, maintained (correctness fixes through Aug
2026: #1895, #1894, #1920) — with zero live callers. The state was "dormant since mid-July,
deliberately held", not "unproven". Hence: extend the existing machinery through
built-ins; re-deriving DPB/reference-list logic would be the parallel-system mistake.

## Why both directions, and which codecs

Encode-only was considered and rejected: the round-trip is the proven shape, the PSNR
gate needs decode, and every downstream consumer of the networking domain (MoQ
subscribe, WHEP playback) is a decode consumer. H.264 + H.265 both carry recorded
proof; AV1/VP9 are ported but have never been proven, so they stay unexposed until a
consumer demands them. JPEG decode is a different, working backend (`sdk/vulkan-jpeg`:
CPU parse + Huffman + fused Vulkan compute; nvJPEG parked in-engine) and rides as its
own decoder block. Opus is the audio sibling — the held `packages/opus` logic and the
`camera-audio-recorder` use case both want it.

## Why built-in blocks

A codec is a native per-frame path the user configures but does not author — the
exact rationale that made camera/display/microphone/speaker built-ins. The session
API shape is mined from the era that worked (lazy encoder mint from the first frame's
dimensions; decoder DPB auto-sized from SPS), not re-invented. Kernels-as-objects is
for compute the user writes; a codec is not that.

## Why encoded frames ride inline in bags

The historical blocker was the wire, in three eras: a fixed 64 KiB slot that silently
truncated (`FramePayload`, #127-era); then schema-declared budgets where a 16 MiB
worst-case IDR forced ~256 MiB of pre-allocated shared memory per encoder publisher,
over-budget payloads were a crash class (`ExceedsMaxLoanSize`), and JTD's missing
binary type wire-expanded bitstream ~1.5× as a msgpack array (#859 capped the JPEG
rig at quality 70); then a growth hint (#1421/#1482) the schema-free pivot deleted as
initial-allocation priming only. Today's wire is variable-size by construction:
slice loans with `AllocationStrategy::PowerOfTwo`, grow-and-retry reads that drop
nothing, ceilings of 16 MiB (helper-touching links) / 64 MiB (app-process). A 500 KB
IDR is 3% of the untrusted ceiling; msgpack `bin` is 1× footprint. The delivery
-profile ADR already chose producer-written sync-point/group/sequence bag fields
(mapping onto MoQ group/object ids), and the plan's loss doctrine was written
presuming encoded frames are bags. Pooled-buffer or surface-id carriage would be new
core machinery bought before any measured need.

The coupling named in the plan entry: an uncounted iceoryx2 ring-overwrite (the
§Processor model OPEN) corrupts an encoded stream until the next sync point. The
discard-to-sync-point doctrine makes that survivable, so it is not a prerequisite —
but the codec entry names it so shipping codecs is never read as resolving it.

## Why proof precedes surface

Decode is unverified at HEAD: last-known-good 2026-05-16 (fixture-fed, all-PASS),
last-known-bad 2026-05-26 (#1077 — camera-fed roundtrip, first IDR missed on both
the cdylib and baseline arms, so engine-tier; never fixed, never re-verified). Its
two unadjudicated hypotheses — iceoryx2 zero-history subscriber missing the first
IDR, vs implementation-defined `vkGetEncodedVideoSessionParametersKHR` header
framing — predate the current wire, which may have transformed the first. Building a
Python surface on an unverified decode path would gild an unproven core. The PSNR
rigs, reference PNGs, injection modes and vivid baseline are engine-owned and
survive; `/verify-live` already names the codec round-trip as its scenario 3.

GPU tests never run in CI (rig-only), so the ship bar is split by necessity: rig
round-trip + PSNR floor through `/verify-live`, plus CI-named GPU-free tests for
everything that parses, translates or resolves without a device.

> Outcome, 2026-09-01 (`codec-roundtrip-reproof`, tickets #2083-#2087 and #2094).
> The premise above is resolved and the paragraph opening it is history, not the
> tree's state: decode is verified at HEAD for both codecs. #1077 closed **obsolete**
> — the decoder enters at `sequence_index=0` with nothing discarded, and the wire its
> first hypothesis was recorded against no longer exists (`ordered` at depth 16,
> parameter sets on every IDR rather than only the first). Its second hypothesis was
> never the cause either, though two latent silent paths in the encoder's header route
> were hardened on the way past. #756 closed on 18 clean Cam Link runs; #335 closed
> **not reproducible** — the pre-RHI-coupling teardown that produced it is gone, and
> the decoder-lag condition that triggered it went with the decode path's 21-25 fps
> cap (now 3.75 ms/frame). The four video blocks, the rebuilt rig and the chroma floor
> are plan text in §Media I/O.

## Why the rig rebuilds on tap + exchange

The historical PSNR examples scored by side effect: a display processor wrote decoded
frames to disk and a script paired PNGs by frame index. That coupling is why the rigs
lived in examples. The control plane now carries the purpose-built path — tap a
channel for its bags, exchange a surface id for the exact frame bytes (the plan's own
"evidence and PSNR path") — so PSNR becomes a first-class calculation in the proof
tooling fed by observation, not a display hack. PSNR is load-bearing for codec work:
it is what repeatedly uncovered encode/decode defects (the R↔B injection gate, the
quality-70 wire cap, the colorimetry drift lock).

## Why Opus links statically

libopus is BSD-3-Clause and royalty-free. The wheel already discharges
reproduce-the-notice terms through PEP 639 `license-files`
(`xtask generate_third_party_notices` + `test_third_party_notices.py`), the same
surface shaderc and VMA use. The dlopen arm exists for system audio servers whose
libraries belong to the host; a codec the wheel can carry adds no `DT_NEEDED` entry
by linking statically, which is the portability gate's whole demand.

## Held-consumer disposition

§Consumers holds the codec consumers "until the align covering that domain mines
them" — this is that align. The packages are logic mines, not forms to keep; the
PSNR/roundtrip examples existed to be the proof and are superseded by the engine-owned
rig; `h264-opus-validator` orchestrates ffmpeg and touches no engine machinery;
`camera-audio-recorder` is the one with a future — it becomes the recording showcase
and finally gives Linux the audio-in-MP4 path the old writer never had.

> Executed in part, 2026-09-01 (#2087): `packages/h264`, `packages/h265`,
> `examples/vulkan-video-roundtrip` and `examples/vulkan-video-psnr` are mined and
> deleted. `packages/{jpeg,opus,mp4}`, `examples/jpeg-psnr`,
> `examples/h264-opus-validator` and `examples/camera-audio-recorder` stay held on
> their own rungs.

## Why the Python surface is four markers and one cast

A codec block is configured and never instantiated, so its Python spelling is the one
every built-in already has: a marker class with no constructor, passed by type to
`rt.add`, resolved to the processor's import path by type identity and nothing else.
Four markers rather than one parameterised class, because each built-in is its own
port surface, registration and identity — the same reason the engine half is four
processors over two bodies. A string-import-path door on `rt.add` was rejected: it
would put an unchecked spelling beside a checked one, and the registry miss it
invites lands a node in `Error` state rather than raising.

The encoded frame gets one cast, pure Python, composing no surface machinery. Its
payload is an access unit of bytes riding inline as msgpack `bin`, so it has no
surface, no claim and no lifetime contract — the audio block's reasoning verbatim. The
constructor keywords are the wire keys because the read path calls the class with the
bag's entries; the stored bitstream field takes the Rust struct's name so a reader of
either language sees one vocabulary. No to-bag helper and no numpy property: an access
unit is opaque to everything but a decoder, a container or a socket, and a second
spelling of the wire contract is a second thing that can drift.

No Python-side PSNR rig. Below the marker the path is byte-identical to the one the
engine-owned rig scored, so the proof that the surface changed nothing is agreement —
a Python-authored round trip locking to the same per-codec vivid baseline within the
same tolerance. A separate rig could only measure the same converters twice.

The decoded frame is buffer-backed, which puts it on the camera's side of the
kernel-bindability line: a Python kernel reaches it through a DLPack landing copy, not
by bare surface id. Carried, not created — the decoder publishes through the same
pooled pixel-buffer hand-off the camera's non-DMA-BUF path uses, and moving it to a
texture backing is an engine question no Python surface should answer in passing.

Two things a Python author will meet are engine-wide and deliberately not fixed under
a codec rung: no built-in refuses an unknown config key, and a device without Vulkan
Video runs the app with an empty channel rather than refusing at `setup()`. Both are
recorded as findings on the change; the stub docstrings state today's behavior.

> ~~a device without Vulkan Video runs the app with an empty channel rather than
> refusing at `setup()`~~ — Superseded 2026-09-01 by #2105's read of
> `encoded_frame_to_published_surface_decoder.rs:129-153`. It is the encoder's shape
> alone: the encoder mints its session lazily from the first frame, so a failed mint
> is one `error!` line and a latch, while the decoder mints eagerly at `setup()` and
> a device with no decode queue already refuses by name — the processor reaches
> `Error` and the readiness wait raises. The capability probe the finding recommends
> is owed on the encoder only.

## Known landmines carried forward (from the audit, for the implementing tickets)

- First-IDR loss over a late-attaching subscriber; driver-defined session-parameter
  header framing (#1077's two hypotheses). **Discharged** — neither was the cause; see
  the outcome note above.
- Cam-Link-only 60 fps race class that vivid and validation layers both hide (#756);
  decoder-lags-encoder shutdown race (#335). **Discharged** — both closed on rig
  evidence.
- H.265 CTU padding: 1920×1088 decoded extent needs crop (PR #328). **Discharged** —
  and it was an engine-layer gap, not a built-in's: both codecs now derive the display
  window through one helper in the decode session, and the H.264 arm had been cropping
  correctly all along while H.265 published its padding to everything holding a
  `SimpleDecoder`.
- `effort_level` (Vulkan encoder-effort index) vs `quality_level` are distinct knobs
  (#306/#329, #330/#333). Still live: no rate-control or GOP config surface has landed.
- H.273 `ColorInfo` ↔ VUI translation is solved and portable (PR #828). **Mined** into
  `runtime/streamlib-media-builtins/src/h273_color_vui_translation.rs`, codec-agnostic
  — the two packages' translation files were byte-identical, so H.265 needed no mining
  of its own — and both source packages are deleted.
- The held Linux MP4 writer was raw-RGBA piped to an ffmpeg subprocess; the direct
  encoded-mux path was abandoned in April 2026 — `Mp4Sink` is new work, not a port.

## Why the recording rung is shaped this way

Written with `opus-mp4-recording-rung` (proposed 2026-09-02), before its approval.

**The encoded-audio bag is the video convention applied, not a second one.** `EncodedAudioPacket`
carries `codec`, `bitstream`, the ordering pair and the sync-point flag exactly as
`EncodedVideoFrame` does, plus the three numbers a container and a decoder need that a video bag
carries as extent and colour — `sample_rate`, `channels`, `sample_count` — and `pre_skip`,
because `OpusHead` needs it and the decoder must trim it. Every packet is a sync point and its
own group because an Opus decoder enters at any packet. "Packet" rather than "frame" because Opus
uses *frame* for a subdivision of one, and a name that means two things at the seam it crosses
is the wrong name.

**Framing is the window contract, not the encoder — and the contract follows the source's
channels.** The held `packages/opus` refused any input that was not already 48 kHz stereo `f32`
in 960-sample frames and told the author to add a rechunker; the plan put resampling and framing
into the read-side stage precisely so no processor does that. The encoder declares a 960/960
window at 48 kHz and reads one Opus frame per dispatch. The first draft fixed a channel count in
that declaration because the contract required one; the owner rejected the hard-coding the same
day — a graph is dynamic, a microphone added later must not require touching its consumers, and
a fixed count belongs only where a model asserts on it. So `channels` became the contract's one
optional value, absent meaning the source's count, and the encoder mints from the first block it
sees exactly as the video encoder mints from its first frame, multistream for surround.

**Many tracks by connection, not two fixed inputs.** The first draft gave the sink one video and
one audio input, the shape every held consumer had, and the shape the owner named as what bit
the tree before: no second camera, no second microphone, no data track. A link is already the
engine's unit of a stream, every queued frame already carries its inbound link for drop
attribution, and MP4, CMAF, MoQ and WebRTC all model a stream as a track — so the sink takes any
number of links on one input and each becomes a track named after its producer, and the read
seam gains the one thing it lacked: telling the reader which link a bag arrived on. Caption and
data tracks then need only a bag convention, not a sink change.

**No concealment.** The audio doctrine is that a device edge drops and counts and the stage
flushes rather than interpolates: no sample is ever invented. A decoder that concealed a lost
packet would invent 20 ms of audio, so it does not; it resets, re-enters, logs the count and
leaves the gap derivable from the stamps. FEC and DTX are networking-rung questions with no
consumer today.

**Fragmented MP4 because teardown is not a promise.** `teardown()` runs after a processor's loop
exits, in graph-traversal order, and never on a panicked thread; the runtime's stop is Ctrl-C.
A flat file whose trailing `moov` is never written is nothing. A fragmented file plays to its
last closed fragment whatever happened to the process, and the fragment closes at the video
sync point the discard-to-sync-point doctrine already made a meaningful boundary. It is also the
shape (CMAF) a MoQ or WHEP sender would emit, so the writer is not a dead end when networking
lands.

**`avc1`/`hvc1` with parameter sets stripped from samples.** The encoder prepends the parameter
sets to every IDR so a live subscriber can join mid-stream; a container has a sample entry for
exactly that, and ISO/IEC 14496-15 forbids in-band parameter sets under `avc1`/`hvc1`, which is
what Apple hardware plays. Stripping them is what makes the ffmpeg re-tag in `/verify-video`
unnecessary. The H.265 sample entry's profile-tier-level fields come from the engine's own
parameter-set parser; a second parser for the same bytes would be the parallel-system mistake.

**Nanosecond video timescale.** `u32` holds 1 000 000 000, so the monotonic stamps the whole
data plane already shares land in the container exactly, with no 90 kHz rounding carry across a
long recording. Audio stays at 48 000 with the packet's own sample count as its duration. A/V
sync is then the plan's own subtraction: one epoch, two offsets, no edit list.

**Static libopus through `opusic-sys`, `mp4-atom` for the boxes.** `opusic-sys` carries libopus
1.6.1 and its BSD-3-Clause `COPYING` inside the crate and builds it with cmake, which the wheel
builder already has for shaderc; static by default means `DT_NEEDED` does not grow, which is the
portability gate's whole demand. `audiopus_sys` was rejected as a 2021 crate carrying an old
libopus. `mp4-atom` is pure Rust and encodes every box the sink needs including `dOps`, which the
`mp4` crate's writer lacks; a hand-written box writer would be a maintenance burden and no
capability.

**Nothing was a port.** The MP4 package was an ffmpeg subprocess transcoder, the recorder a
deferral stub, the validator an ffmpeg orchestrator whose AVCC "reference" was a comment over
`None`. All three delete in the change; the rule that the session epoch is the first stamp and
earlier audio is dropped is the one line carried from the dead Apple writer.
