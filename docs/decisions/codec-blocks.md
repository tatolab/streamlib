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
  (h265), vivid colorimetry drift 0.0000; the baseline is still checked in at
  `runtime/streamlib-engine/tests/fixtures/psnr_vivid_baseline.tsv`.
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

## Known landmines carried forward (from the audit, for the implementing tickets)

- First-IDR loss over a late-attaching subscriber; driver-defined session-parameter
  header framing (#1077's two hypotheses).
- Cam-Link-only 60 fps race class that vivid and validation layers both hide (#756);
  decoder-lags-encoder shutdown race (#335).
- H.265 CTU padding: 1920×1088 decoded extent needs crop (PR #328).
- `effort_level` (Vulkan encoder-effort index) vs `quality_level` are distinct knobs
  (#306/#329, #330/#333).
- H.273 `ColorInfo` ↔ VUI translation is solved and portable — mine
  `packages/{h264,h265}/processors/color_vui_translate_linux.rs` (PR #828).
- The held Linux MP4 writer was raw-RGBA piped to an ffmpeg subprocess; the direct
  encoded-mux path was abandoned in April 2026 — `Mp4Sink` is new work, not a port.
