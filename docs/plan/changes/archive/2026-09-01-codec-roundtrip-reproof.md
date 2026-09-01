# codec-roundtrip-reproof

The first rung of §Media I/O's codec half (decided 2026-08-31, align PR #2081,
`385df4e0`; rationale in `docs/decisions/codec-blocks.md`): proof precedes surface.
This change re-proves camera → encode → decode → display at HEAD for H.264 and
H.265, adjudicates the #1077 decode regression, closes out the #756/#335
real-hardware races, and rebuilds the PSNR rig on the control plane's own
observation surface with PSNR a first-class calculation. It lands the engine half
of the four video codec blocks and the encoded-frame bag convention — and no
Python surface: the block API waits for this proof by decision.

**Scale gate — this skill, no new ADR.** New behavior (the tree's first
encoded-domain link, four engine processors, the rebuilt rig) built entirely on
decided contracts: the existing session surface (`GpuContext::
create_encoder_session` / `create_decoder_session`,
`runtime/streamlib-engine/src/core/context/gpu_context.rs:2585,2617`), the bag
wire, and the built-in registration path. The ADR was written by the align.

**Precondition.** Every §Media I/O codec bullet is DECIDED (merged `385df4e0`),
zero OPEN entries among them. Verified against the tree 2026-08-31: the session
APIs are live and maintained; the PSNR fixtures are engine-owned
(`runtime/streamlib-engine/tests/fixtures/{psnr/,psnr_vivid_baseline.tsv,
e2e_fixture_psnr*.sh}`); built-ins register via `PROCESSOR_REGISTRY.register`
(`runtime/streamlib-media-builtins/src/lib.rs:50-57`); a plain-Rust harness can
build and run a graph (`Runner`, `ProcessorSpec` — the tokio-integration shape).

---

## ADDED: the four video codec processors, engine half

`H264Encoder`, `H264Decoder`, `H265Encoder`, `H265Decoder` in
`runtime/streamlib-media-builtins/`, registered beside camera and display —
native processors whose per-frame paths never enter an interpreter. No Python
marker classes, no stub entries, no `rt.add` reach in this change: that is the
block API the proof gates.

- Encoder: input port takes the camera's published surface; color conversion to
  the codec's NV12 input rides the engine's existing converter path (the
  `rgb_to_nv12` compute stage under `vulkan/video/`, reached like
  `RhiColorConverter` is from `camera_source.rs`) — no new RHI primitive. The
  session mints lazily from the first frame's dimensions
  (`create_encoder_session(SimpleEncoderConfig, prepare_gpu_input)`), the mined
  shape: config is a guardrail, dimensions track upstream. H.273 `ColorInfo` ↔
  VUI translation is mined from
  `packages/{h264,h265}/processors/color_vui_translate_linux.rs` into the
  built-ins crate.
- Decoder: `create_decoder_session(SimpleDecoderConfig)` with `max_width` /
  `max_height` `0` — coded dimensions auto-detected from the first SPS, DPB
  auto-sized. Output publishes as ordinary surfaces a display or tap consumer
  reads. The H.265 CTU-padding crop (1920×1088 → 1920×1080, PR #328's lesson)
  is applied at the decoder's publish edge, never left to consumers.
  > Located 2026-08-31 by #2086. "The decoder's publish edge" is the engine's
  > decode session, not the built-in: the H.264 SPS handler already applied
  > its frame crop while the H.265 one recorded `pic_*_in_luma_samples`
  > verbatim, so the padding reached everything holding a `SimpleDecoder` —
  > an engine-layer gap, fixed at the engine layer. Both codecs now derive the
  > window through one helper
  > (`vulkan/video/decode/decoded_picture_display_window.rs`), and the session
  > keeps the coded extent (parameter sets, DPB) separate from what it
  > publishes. Consequence worth knowing: `SimpleDecodedFrame` is cropped on
  > the RGBA path and stays coded on the raw NV12 path, which is a direct DPB
  > readback — the built-ins use RGBA only.
- Both codecs, both directions, because the recorded proof (PRs #328, #827)
  covered both and they share every seam. AV1/VP9 stay unexposed per the plan.
  > Delivered 2026-08-31 by #2086 as one encode body and one decode body
  > specialised by a codec identity, rather than four processors: "share every
  > seam" turned out to be literal — the pairs differ in a `Codec` enumerant,
  > a bag `codec` string and a name. Each built-in is its port surface, its
  > registration and its identity. The H.265 VUI needed no mining either:
  > `packages/h265`'s translation file is byte-identical to `packages/h264`'s,
  > already mined by #2083 into the codec-agnostic
  > `h273_color_vui_translation`.

## ADDED: the encoded-frame bag convention

The tree's first encoded-domain link. An encoded frame is an ordinary bag; the
proposed spelling, honoring the delivery-profile decision's fields
(`docs/decisions/delivery-profile-vocabulary.md:143-152`):

- `codec` — `"h264"` / `"h265"` (elementary-stream identity)
- `bitstream` — msgpack `bin`, one Annex-B access unit
- `is_sync_point` — bool; IDR/CRA, the group boundary
- `group_index`, `sequence_index` — u64; the MoQ-mappable ordering pair, and
  ~~the PSNR rig's frame-pairing key (replacing the old
  `frame_number → frame_index` threading)~~

  > Superseded 2026-08-31 by #2085 (PR #2093). These are encoded-frame keys and
  > never reach a decoded bag, so they cannot pair a decoded frame to its
  > reference; see §the rig, rebuilt on tap + exchange.
- `width`, `height` — coded extent before crop
- `color` — the H.273 tuple (primaries, transfer, matrix, range)

Timestamp rides the frame header like every bag. The decoder reads exactly this
convention back; a bag it cannot read is refused by name, never reshaped — the
audio wire codec's doctrine. A consumer seeing a `sequence_index` gap discards
to the next `is_sync_point`, per the decided loss doctrine
(`ARCHITECTURE.md:377-393`).

## ADDED: the rig, rebuilt on tap + exchange

- The proof harness is an engine-owned Rust fixture app —
  `cargo run -p streamlib-engine --example codec_roundtrip_rig` — building
  fixture-source/camera → encoder → decoder → display with `Runner` +
  `ProcessorSpec`. Engine-owned means CI compiles it (rot protection) while
  running stays rig-only, and no test reaches into a consumer for fixtures.
  The fixture source replays the checked-in `psnr/` reference PNGs (the old
  `BgraFileSource` role, mined); the vivid arm uses `CameraSource` unchanged.
- Scoring rides observation, not display side effects: the rig tooling taps the
  decoded channel for bags, exchanges each sampled surface id for the exact
  frame bytes over the control plane's bytes route (the plan's own "evidence
  and PSNR path", `ARCHITECTURE.md:1305-1316`), and ~~pairs against references
  by `sequence_index`~~. The display-writes-PNGs mechanism retires with the old
  examples. Budget taps per the 500 ms sampling window; read `received` vs
  `requested`.
  > Superseded 2026-08-31 by #2085 (PR #2093). `sequence_index` is an
  > encoded-frame bag key and never reaches a decoded bag — a decoded frame is
  > an ordinary `VideoFrame` — so the join the proposal assumed does not exist
  > on the decoded side. Best-match pairing was rejected as vacuous: it
  > satisfies `swap-channels` by re-pairing a swapped red onto
  > `solid_blue.png`, the regression that mode exists to catch. Delivered
  > instead as one rig run per reference (`--fixtures` holding a single PNG),
  > with the scorer pairing on the `<reference_stem>__<n>.png` filename
  > contract (`xtask/src/psnr.rs`). **#2086 builds on this rig — read this, not
  > the struck clause.**
- PSNR becomes first-class in the proof tooling: `cargo xtask psnr` — per-frame
  and per-plane Y/U/V against a reference set, the Y ≥ 35 dB pass / 30–35 warn
  / < 30 fail classification, and the three injection modes preserved
  (`swap-channels`, `bt601-bt709`, `range-swap`) so the gate stays provably
  non-vacuous. Pure math, GPU-free: its unit tests are CI-named. ffmpeg leaves
  the scoring path.
  > Extended 2026-08-31 by #2094. The classification is no longer luma-only:
  > either chroma plane under 30 dB fails a frame outright, one floor for
  > every reference and no warn band. Derived from six cold rig runs (three
  > per codec, 108 samples) whose lowest finite clean chroma figure is
  > `complex_pattern` at 32.23 dB, reproducing to 0.02 dB run-to-run and
  > 0.13 dB across codecs. A fourth injection mode `swap-chroma` (Cb↔Cr
  > transposition) lands with it, because the three above are all caught by
  > luma as well — without it the new floor would gate nothing. Worth knowing
  > for a rig arm: `swap-chroma` is not luma-invariant as measured either.
  > The transposition leaves Y untouched on the wire, but the inverse
  > transform puts `complex_pattern` and `solid_blue` out of gamut and the
  > clamp moves their Y, so a whole-set run fails on luma too. `solid_red`
  > (Y 42.11) and `solid_green` (Y 38.59) are the two that pass luma and
  > fail on chroma alone, and they are what makes the floor non-vacuous. What that floor measures is worth knowing: a lossless codec
  > pushed through the engine's own `rgb_to_nv12` and `nv12_to_rgb` scores
  > `complex_pattern` within 0.2 dB of a real one, so the chroma columns are
  > the round trip's colour path — the two converters and the 8-bit TV-range
  > wire — and carry no codec-quality signal. Every regression class the
  > gate is for (plane order, plane offset, subsampling filter, matrix,
  > range) still reaches it, because all of them reach the decoded RGB.
- `e2e_fixture_psnr.sh` and `e2e_fixture_psnr_vivid.sh` re-point from the dead
  examples to the fixture app + `xtask psnr`; the vivid baseline
  (`psnr_vivid_baseline.tsv`) and ~~its drift lock carry over unchanged~~ its
  drift lock carry over.
  > Superseded 2026-08-31 by #2085 (PR #2093). The drift lock does carry over
  > unchanged — tolerance ±0.05 and comparison semantics are untouched — but
  > its numbers could not: they were sampled off the display's composited
  > output, which is the thing this change removed from the measurement path,
  > so the old TSV fails outright against the new one
  > (|0.9792 − 0.9180| = 0.0612 > 0.05). Re-measured r 0.9180/g 0.0575/b 0.0536
  > → r 0.9792/g 0.0029/b 0.0068, identical to four decimals across three cold
  > runs, and the gate gained headroom: the bt601/bt709 green rise now reads
  > 0.0965 off a 0.0029 floor instead of a 0.0575 one.
  > Amended 2026-08-31 by #2086: the lock is per codec, not per rig — the
  > H.265 arm locks against `psnr_vivid_baseline_h265.tsv` and h264 keeps the
  > unsuffixed file it was captured under. Measured, the two agree to 0.0001
  > on every channel (r 0.9792 / g 0.0029 / b 0.0068 vs 0.0067) against a
  > ±0.05 tolerance, so one shared file would in fact have passed both arms
  > here. The split is headroom for a codec that does reconstruct a saturated
  > primary differently, so that it cannot be read as a colour regression.

## ADDED: the adjudications

- #1077: reproduce the decode regression against HEAD's wire with the rebuilt
  rig, adjudicate its two recorded hypotheses — first-IDR loss at a
  late-attaching subscriber (possibly transformed by today's `ordered`
  depth-16 wiring) vs implementation-defined
  `vkGetEncodedVideoSessionParametersKHR` header framing — and fix at the
  engine layer. The ticket closes with the cause named, or closes as obsolete
  with the HEAD-wire run that proves it gone.
- #756 (h264 Cam Link `DEVICE_LOST`) and #335 (h265 shutdown race): re-run on
  the rig's real capture hardware with the fixture app, 3+ cold clean runs per
  #756's own bar; vivid alone is recorded as hiding this class. Close or
  re-scope each on the evidence.
- Ship bar per the plan: the change is complete only with the rig round-trip at
  the PSNR floor via `/verify-live` for both codecs, plus the CI-named GPU-free
  tests below.

## ADDED: CI-named GPU-free tests

Named individually in `test.yml` and the xtask mirror, or they run nowhere:
encoded-frame bag codec round-trip (write → read, refusal naming, gap →
sync-point discard), NAL/bitstream parse units for the seams #1077 implicates,
`ColorInfo` ↔ VUI translation, encoder/decoder config resolution, and the
`xtask psnr` math (including injection-mode trip tests).

## MODIFIED: stale anchors the deletions expose

- `docs/architecture/texture-ring.md:323-324` — cites
  `packages/{h264,h265}/processors/decoder_linux.rs`; ~~re-anchored to the
  built-ins~~.

  > Superseded 2026-08-31 by #2087. The bullet those lines sat under claimed
  > the two files were the ring's *first consumers*, and they were not:
  > `decoder_linux.rs:19-20` disclaimed the ring outright ("No engine-only
  > `TextureRing` / `copy_pixel_buffer_to_slot` reach from the cdylib"), and
  > the built-ins that replaced them hold no `create_texture_ring` either — so
  > re-anchoring "to the built-ins" would have restated a false claim against
  > a new target. Re-anchored instead to the one shipped ring consumer,
  > `sdk/vulkan-jpeg/src/vulkan_compute_backend.rs`, with the label corrected
  > from CPU-upload to the compute-kernel fill it actually uses.
- `runtime/streamlib-engine/src/apple/mod.rs:11` and
  `runtime/streamlib-engine/src/vulkan/_nvjpeg_impl_pending_/README.md:11` —
  comments citing `packages/h264` / `packages/mp4` paths; ~~re-anchored~~.

  > Amended 2026-08-31 by #2087. The README was re-anchored; `apple/mod.rs`
  > was deleted instead. Its comment was the moved-to breadcrumb shape
  > `.claude/rules/comments.md` bans by name — a where plus a carve-out ticket
  > number, no why — and trimming it to its surviving `packages/mp4` clause
  > would only re-arm the same rot, since `ARCHITECTURE.md:1157` deletes
  > `packages/mp4` when `Mp4Sink` ships.
- `.claude/skills/verify-live/SKILL.md:80` and
  `.claude/skills/verify-video/SKILL.md:30` — both name
  `vulkan-video-roundtrip` as the runnable; re-pointed at the fixture app
  (verify-video's fuller rewrite waits for the recording showcase).
- `.claude/scripts/tests/rig-brake.test.sh:189-237` — brake tests use
  `vulkan-video-roundtrip` paths as their example commands; re-pointed.
- `.claude/scripts/tests/ship-change-removed-gate.test.sh:201` — plants a
  synthetic `packages/h264/src/lib.rs` in its scratch repo; renamed to a
  neutral synthetic path so the literal string stops shadowing a real bullet.

## REMOVED: the mined four

Per §Consumers and the codec disposition bullet: mined for logic in this change,
deleted in this change. ~~The residue enumerated above is the entire engine-side
reference surface (gate-style search run 2026-08-31).~~

> Superseded 2026-08-31 by #2087. It was not the entire surface.
> `runtime/streamlib-engine/src/core/mod.rs:19-22` named the same tree as bare
> `h264` under "their domain packages'", which no `git grep -F packages/h264`
> can see — the gate-style search that produced the enumeration is exactly the
> search that missed it. That comment is deleted too (same reasoning as
> `apple/mod.rs`), along with its third clause, `audio (audio_codec)`, which
> had been dangling since the consumer-tree sweep took `packages/audio`.
> The lesson for the next disposition: a path-literal sweep proves nothing
> about a prose citation that spells the package without its directory.

- REMOVED: examples/vulkan-video-roundtrip
- REMOVED: examples/vulkan-video-psnr
- REMOVED: packages/h264
- REMOVED: packages/h265

## Not in this change

- No Python surface: marker classes, stub entries, `rt.add` reach for codec
  blocks — the next rung, gated on this proof.
- No Opus, no `Mp4Sink`, no `JpegDecoder` rung (#1212 rides the JPEG rung);
  `packages/{jpeg,opus,mp4}` and `examples/jpeg-psnr` stay held.
- No `examples/camera-audio-recorder` conversion (the recording showcase waits
  for encode + mux + audio).
- No MoQ/WebRTC work; no producer-pressure reflection (stays OPEN); no rate-
  control/GOP config surface beyond what the mined guardrail config already
  carries.
