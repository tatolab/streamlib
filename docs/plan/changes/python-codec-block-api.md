# python-codec-block-api

The second rung of §Media I/O's codec half, the one the first rung deliberately
did not build: the four video codec blocks reach Python. `rt.add(H264Encoder)`,
`rt.add(H265Decoder, config={...})`, a stub entry per block, and one cast —
`EncodedVideoFrame` — so a Python processor can read the encoded-domain link the
engine half opened. The plan's own words for this rung
(`docs/plan/ARCHITECTURE.md:1140-1143`): "the Python surface — marker classes,
stub entries, `rt.add` reach — is the next rung the proof below gates". The proof
shipped (`archive/2026-09-01-codec-roundtrip-reproof.md`), so the gate is open.

**Scale gate — this skill, plus the ADR.** New behavior on the Python API's public
contract: four marker classes and a cast type, the same trigger the audio pair
fired (`archive/2026-08-28-dlopen-audio-backend-and-audio-blocks.md:11-16`). The
codec rationale already lives in `docs/decisions/codec-blocks.md`; this change
extends it with one section on the surface's shape rather than opening a parallel
record.

**Precondition.** Every §Media I/O bullet this delta touches is DECIDED: codec
blocks (`ARCHITECTURE.md:1124-1146`), the encoded-frame bag convention
(`:1147-1167`), the display window (`:1168-1182`), proof-precedes-surface and its
per-block ship bar (`:1183-1203`), the chroma floor (`:1204-1232`) and the
per-codec vivid lock (`:1233-1245`). §Media I/O is IN-FLIGHT only on the
audio-plugins OPEN (`:1287`) and the unbuilt later rungs, none of which this
touches. Verified against the tree 2026-09-01: all four processors are registered
on `PROCESSOR_REGISTRY` (`runtime/streamlib-media-builtins/src/lib.rs:86-102`)
and already linked into `_engine.abi3.so` (`sdk/streamlib-python-wheel/Cargo.toml:52`);
the wheel's marker seam names five built-ins and no codec
(`sdk/streamlib-python-wheel/src/python_native_builtin_blocks.rs:60-102`); the
cast pattern is live (`python/streamlib/audio_block.py`); the wheel's test
harness decodes `bin` payloads (`python_test_harness_endpoints.rs`, fixed by the
audio rung). Nothing below needs an engine change.

**Approved by the owner, 2026-09-01**, in their own words, with no bullet struck —
the showcase example included — and both findings left parked as written. Next is
`/derive-tickets`.

Two §Processor model OPENs are coupled and stay OPEN, untouched: the two
uncounted losses (`:355-366`) and producer-pressure reflection (`:397-404`). A
Python consumer of an encoded link inherits the discard-to-sync-point doctrine the
engine half already implements; this change resolves nothing about where the
count lives.

---

## ADDED: §Media I/O — the four video blocks reach Python

- **DECIDED** — `H264Encoder`, `H264Decoder`, `H265Encoder` and `H265Decoder` are
  surfaced to Python as marker classes beside `CameraSource`, through the five
  touchpoints a native built-in owns and no sixth: a
  `#[pyclass(name = "H264Encoder", module = "streamlib", frozen)]` unit struct
  with no constructor in `sdk/streamlib-python-wheel/src/python_native_builtin_blocks.rs`
  (the `PythonMicrophoneSourceBlock` shape, `:46-47`); an `is()` arm in
  `native_builtin_class_import_path` returning
  `streamlib_media_builtins::H264Encoder::Processor::processor_class_import_path()`,
  behind the same `#[cfg(target_os = "linux")]` split `CameraSource` carries with
  the unsupported-platform `PyRuntimeError` on the other arm (`:69-79`), because
  the codec modules are Linux-only (`h264_encoder.rs:4`); an `add_class` line in
  `src/lib.rs` (`:40-44`); a `from ._engine import H264Encoder as H264Encoder`
  re-export plus `__all__` entry in `python/streamlib/__init__.py`; and an
  `@final class` entry in `python/streamlib/_engine.pyi`, gated by stubtest with
  no allowlist. Configured the one way a built-in is configured —
  `rt.add(H264Encoder)`, `rt.add(H264Encoder, config={"keyframe_interval_seconds": 2})`
  — and identified in `graph` by the import path the macro minted:
  `streamlib_media_builtins::h264_encoder::H264Encoder` and its three siblings,
  with `H264Encoder` as the default display name (`Processor::NAME`). No new
  engine registration: `register_media_builtin_processor_types()` already
  registers all four, and the wheel already calls it at import
  (`python_native_builtin_blocks.rs:106-108`).
- **DECIDED** — The stub docstring is where a block's config keys and port names
  are written down, as it is for every built-in (`_engine.pyi:95-110` is the
  model), and this rung writes them from the tree rather than inventing them.
  Encoder: input `video` (`ordered`), output `encoded_video`; config keys
  `width`, `height`, `fps`, `bitrate_bps`, `keyframe_interval_seconds`,
  `effort_level`, every one an optional non-negative integer
  (`published_surface_to_encoded_frame_encoder.rs:46-69`), with the semantics the
  engine half fixed: `width`/`height` are guardrails and a mismatching frame wins
  with a warning (`:339-358`); `fps` resolves frame → config → 60 (`:34`);
  `keyframe_interval_seconds` defaults to 2 (`:37`); an absent `bitrate_bps` means
  constant-QP at the medium preset (`:392-393`); the session mints from the first
  frame and re-mints on an extent change (`:182-194`). Decoder: input
  `encoded_video` (`ordered`), output `video`; config keys `max_width` and
  `max_height`, both optional, both or neither — a half-specified pair warns and
  auto-detects both from the first SPS (`encoded_frame_to_published_surface_decoder.rs:420-437`).
  `{}` deserializes to defaults on both, which is what makes the bare
  `rt.add(H265Encoder)` spelling legal (`:498-502`, `:519-524`). Rate-control and
  GOP knobs beyond these keys stay ticket-level per the plan and are not added
  here.
- **DECIDED** — What a Python app can wire, stated so the docstrings can say it:
  the encoder's `video` input takes any published `VideoFrame` — buffer-backed
  (`CameraSource`, `TestPatternSource`) or texture-backed (a kernel output) — since
  its one resolver walks all three backings
  (`runtime/streamlib-engine/src/core/context/gpu_context.rs:1317-1414`); the
  decoder's `video` output is an ordinary `VideoFrame` on a pooled RGBA
  pixel-buffer surface carrying the conformance-windowed extent, the encoded
  frame's own timestamp, and `color_info`, with `fps`, `texture_layout` and the
  HDR sidecars absent (`encoded_frame_to_published_surface_decoder.rs:374-386`),
  so `DisplayWindow`, the cast object's `__dlpack__` / `cpu()`, and a Python
  processor reading `into=VideoFrame` all consume it unchanged. Worth stating in
  the decoder's docstring because it is the camera's own gap and not a new one: a
  decoded frame is buffer-backed, so it reaches a Python kernel through a DLPack
  landing copy, never by bare surface id. A frame the encoder cannot consume is
  logged and dropped and the processor keeps running (`thread_runner.rs:304-306`);
  a session that fails to mint latches and discards every later frame with one
  `error!` line (`published_surface_to_encoded_frame_encoder.rs:206-213`) — the
  engine half's decided behavior, carried into the docstring so a Python author
  on a device without Vulkan Video knows where to look.

## ADDED: §Media I/O — the `EncodedVideoFrame` cast

- **DECIDED** — `streamlib.EncodedVideoFrame` is the Python cast over the eight
  encoded-frame wire keys the plan fixed (`ARCHITECTURE.md:1150-1155`), pure
  Python in `sdk/streamlib-python-wheel/python/streamlib/encoded_video_frame.py`
  beside `audio_block.py`, read with `ctx.inputs.read("encoded_video", into=EncodedVideoFrame)`,
  re-exported from `__init__.py`, owing no `.pyi` entry — pyright checks it from
  source, as it does `AudioBlock` and `VideoFrame`. It composes nothing
  surface-shaped: the payload rides inline as msgpack `bin` and arrives as
  `bytes`, exactly `AudioBlock`'s reasoning (`audio_block.py:11-20`), so there is
  no surface, no claim and no lifetime contract. Construction is the validation
  and the wire keys are the constructor keywords, because
  `cast_decoded_bag_into_read_target` calls the class with the bag's entries
  (`python_bag_conversion.rs:124-125`): `codec` (`Literal["h264", "h265"]`),
  `bitstream` (`bytes`, one Annex-B access unit, stored under the name the Rust
  struct uses, `annex_b_access_unit_bytes`, off the repr), `is_sync_point`
  (`bool`), `group_index`, `sequence_index`, `width`, `height` (plain ints;
  `bool` refused as an int subclass, as `AudioBlock` does), and `color`
  (`video_frame.ColorInfo | None`, read field by field the way `VideoFrame`
  reads `color_info` — the same H.273 four-tuple under a different key, which
  the docstring says). `width`/`height` are the coded extent before crop, and the
  docstring says so, because a consumer handed 1088 for a 1080 stream must know
  which number it holds. Extra keys are read past, never refused
  (`**keys_this_cast_does_not_read`). Refused by name: a missing key (`from_bag`
  names it), a mistyped field, a `bitstream` that is not `bytes`, and a `codec`
  string naming neither elementary stream — the Rust reader's own three refusals
  (`encoded_video_frame.rs:101-139`), spelled in Python. No numpy property: an
  access unit is opaque bytes, and a consumer that wants a container or a socket
  writes `bytes`.
- **DECIDED** — The cast is read-side; producing an encoded bag from Python is
  spelling the eight keys as a bag literal against the plan's wire contract and
  writing it with the timestamped write, `ctx.outputs.write(port, bag, timestamp_ns=...)`
  (`_engine.pyi:406-411`), because the decoder stamps its output with the frame
  header's timestamp and the implicit write would stamp the moment of
  publication. The cast offers no to-bag helper: `dataclasses.asdict` would emit
  the stored field name rather than the wire key, and a second spelling of the
  contract is a second thing that can drift. This is the doctrine CLAUDE.md
  already states for every bag — spell a literal against the contract, never from
  memory — applied, not extended.

## ADDED: the wheel proves the surface

- **DECIDED** — CI-run, GPU-free, in `sdk/streamlib-python-wheel/tests/`: each
  marker refuses instantiation and defaults its display name to its type
  (`test_native_builtin_blocks.py:31,150` shape, four times); the four connect
  without an adapter — `pattern.output("video") → encoder.input("video")`,
  `encoder.output("encoded_video") → decoder.input("encoded_video")`,
  `decoder.output("video") → window.input("video")` — the
  `test_speaker_sink.py:61` shape; stubtest and pyright stay green with the four
  entries in; and `test_encoded_video_frame_cast.py` mirrors
  `test_audio_block_cast.py` — every wire key locked, `bitstream` crossing real
  wired ports as `bytes` through `streamlib.testing`'s feeder and collector,
  each refusal naming its key or its codec, an unknown key read past, `bool`
  refused for every integer field, and the cast holding no surface and no claim.
- **DECIDED** — Rig-only, `requires_gpu`, `test_video_codec_blocks.py` over a
  `video_codec_blocks_app.py` and its probes, parameterized over both codecs:
  `TestPatternSource → <codec>Encoder → EncodedFrameProbe` asserts every bag
  casts, the first admitted bag is a sync point, `sequence_index` advances by
  exactly one, `group_index` steps only at a sync point, `bitstream` opens with
  an Annex-B start code, and the coded extent is the source's padded up to the
  codec's block (a 320×180 pattern codes at 320×192 under both codecs — the
  16-sample macroblock and the 64-sample CTU agree on it). `TestPatternSource →
  <codec>Encoder → <codec>Decoder → VideoFrameProbe` asserts the decoded frames
  arrive at the source extent — the conformance crop, proven from Python for both
  codecs — with each `timestamp_ns` equal to the encoded bag's and `color_info`
  present. These carry the marker like every graph test here and run nowhere in
  CI; the module docstring says so, as `test_microphone_source.py:5-8` does.
- **DECIDED** — The per-block ship bar the plan states (`ARCHITECTURE.md:1190-1192`)
  is met by agreement, not by a second rig: the codec path below the marker is
  byte-identical to the one the Rust rig scored, so the live proof for the
  Python surface is that a Python-authored round trip locks to the same per-codec
  vivid baseline the Rust rig locks to. `runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh`
  gains a `PIPELINE=python` arm (default `rust`, everything after launch
  unchanged — tap, `exchange`, `xtask psnr channel-means`, the per-codec TSV,
  ±0.05) that launches `runtime/streamlib-engine/tests/fixtures/codec_roundtrip_node.py`
  beside `audio_loopback_node.py`: a Python app of four `rt.add` calls —
  `CameraSource → <codec>Encoder → <codec>Decoder → DisplayWindow` — taking
  `--codec`, `--camera` and `--control-plane-port` the way the Rust rig does.
  Ship bar: both codecs PASS through the Python arm via `/verify-live` (wheel
  rebuilt first), log gates at zero, clean SIGTERM exit. The fixture app is
  engine-owned and no test reaches into a consumer for it. The reference-PNG rig
  gets no Python arm: it would need a Python PNG-replay source and prove nothing
  the vivid arm does not, since nothing Python-specific sits on the colour path.

## ADDED: §Consumers — the showcase, by convention

- `examples/camera-codec-roundtrip/` — a converted example (`app.py` +
  `pyproject.toml` from the `streamlib new` scaffold, per `ARCHITECTURE.md:232-243`):
  camera → encoder → decoder → window, codec chosen by `STREAMLIB_CODEC`
  (`h264` default) and the device by `STREAMLIB_CAMERA_DEVICE` as
  `examples/camera-display/app.py:16-35` does. A showcase with no CI presence
  and not the proof's fixture (`:277-287`); the plan decides nothing new for it.
  The owner may strike this bullet without touching the rest — the surface is
  proven without it.

## MODIFIED: `docs/decisions/codec-blocks.md`

- Gains one section, "Why the Python surface is four markers and one cast": a
  block is configured and never instantiated, so a marker with no constructor
  (the built-in precedent); one cast whose payload is bytes composes no surface
  machinery (the audio precedent); no Python-side PSNR rig, because agreement
  with the Rust rig's own baseline is the stronger proof; the decoded frame's
  buffer backing is the camera's existing gap, carried not created. Written with
  this proposal, as every ADR here was written before its approval.

## MODIFIED: plan text at fold

- `ARCHITECTURE.md:1140-1143` — the codec bullet's "the Python surface … is the
  next rung the proof below gates" folds to SHIPPED with this change's tickets;
  the four verify lines above gain the cast test and the marker tests.
- `docs/plan/diagrams/system.mmd` — the media node's codec line already names the
  blocks; the Python-reach edge is what the fold checks.

## REMOVED:

Nothing. The rung is additive, and every held codec consumer stays on the rung
that mines it: `packages/{jpeg,opus,mp4}`, `examples/jpeg-psnr` and
`examples/camera-audio-recorder` wait for their blocks (`ARCHITECTURE.md:1270-1286`),
and `examples/h264-opus-validator` — which the align said deletes outright —
waits with `Mp4Sink`, because its `videotoolbox_encoder.rs` is now the tree's
only AVCC↔Annex-B reference and that rung re-derives exactly that conversion.
A bullet with no artifact to prove gone is not written.

## Not in scope

- `JpegDecoder`, the Opus pair, `Mp4Sink`; the `camera-audio-recorder`
  conversion (the recording showcase waits for encode + mux + audio); the
  `/verify-video` rewrite that waits with it.
- No rate-control or GOP config surface beyond the six encoder keys and two
  decoder keys the engine half already carries.
- No producer-pressure reflection; the two uncounted losses stay OPEN.
- No MoQ / WebRTC; no encoded-bag carriage other than inline `bin`.
- `packages/` and the held examples lag by design.

## Findings, not in this change

Two engine-wide facts the recon surfaced, recorded here for the owner rather than
folded into a codec rung:

- **No built-in refuses an unknown config key.** No `Config` struct in the
  media built-ins carries `#[serde(deny_unknown_fields)]`, so
  `rt.add(H264Encoder, config={"keyframe_interval_secs": 1})` is accepted and
  runs at the 2-second default, and a mistyped value surfaces at `rt.run()`
  from the registry's constructor closure
  (`processor_instance_factory.rs:286-299`), never at `rt.add()`. It is the
  write-side gap CLAUDE.md names for bags, on config. Recommendation: one
  engine-layer change refusing unknown keys for every registered `Config`, its
  own change, not this one — it touches five shipped built-ins' contracts.
- **A device without Vulkan Video runs the app with an empty channel.** The
  lazy mint is decided, and a failed mint is one `error!` line and a latch; no
  exception reaches the Python author, unlike a missing `/dev/video*` or an
  unopenable audio device, both of which raise at `setup()`. Recommendation:
  a `setup()`-time queue-capability probe on the encoder and decoder — refusing
  by name where the device has no encode or decode queue for the codec — keeps
  the lazy mint and gives the codec blocks the same refuse-at-setup shape the
  other device classes have. An engine-half ticket if the owner wants it; the
  stub docstring states today's behavior either way.

## Validation

- **Pure Python, CI-run**: four markers refuse instantiation and default their
  display names; the round trip wires with no adapter; stubtest and pyright green
  with the four stub entries and the cast in; `test_encoded_video_frame_cast.py`
  locks all eight keys, carries `bitstream` as `bytes` across real ports, names
  every refusal, reads past an unknown key, and holds no surface and no claim.
- **Rig, `requires_gpu`**: encoded bags cast and are Annex-B, sync-point-first,
  sequence-contiguous, block-padded; decoded frames arrive at the source extent
  with the encoded timestamp, both codecs.
- **Live**: `PIPELINE=python e2e_fixture_psnr_vivid.sh <out> h264` and `h265`
  PASS through `/verify-live` against the per-codec baselines, log gates zero,
  clean exit — on a wheel rebuilt with `maturin develop` first.
- **The wheel still links nothing new**:
  `test_wheel_portability.py::test_the_native_extension_links_nothing_the_host_may_not_supply`
  stays green (the codec code was already in the `.so`).
- Mechanical: `cargo xtask check-all-source-gates`; wheel pytest suite green;
  `cargo check --target aarch64-apple-darwin` for the non-Linux marker arms.
