# opus-mp4-recording-rung

The third rung of §Media I/O's codec half: `OpusEncoder`, `OpusDecoder` and `Mp4Sink` —
three of the four blocks the plan names as later rungs (`docs/plan/ARCHITECTURE.md:1145-1147`)
— the encoded-audio bag they share, and the recording showcase the plan holds
`examples/camera-audio-recorder` for (`:1347-1349`). Two engine seams move with them, both
owner-directed on 2026-09-02: an audio window contract follows the source's channel count
unless a consumer declares one, and a reader can learn which inbound link a bag arrived on,
which is what makes the sink a many-track writer rather than a fixed pair of inputs.
`JpegDecoder` stays its own rung; the recon facts that make it one are under Not in scope.

**Scale gate — this skill, plus the ADR.** New behavior on the Python API's public contract
(three markers, one cast, one reader method, one relaxed declaration field), a change to the
processor model's read seam, and the wheel's first statically linked C codec.
`docs/decisions/codec-blocks.md` gains one section rather than a parallel record.

**Precondition.** Every bullet this delta touches is DECIDED: the codec roster
(`ARCHITECTURE.md:1129-1147`), the encoded-frame bag (`:1151-1167`), the proof bar
(`:1187-1205`), `Mp4Sink` (`:1335-1337`), Opus linking statically (`:1338-1341`), the
held-consumer disposition (`:1342-1357`), the window-contract entries this amends
(`:936-951`, `:958-976`), and §Consumers' conversion doctrine and held list (`:232-249`,
`:263-277`). §Media I/O is IN-FLIGHT only on the audio-plugins OPEN (`:1359`), untouched.

**Verified against the tree 2026-09-02 — what "mine" turned out to mean.**

- `packages/opus` bound `opus = "0.3"` and refused anything but 48 kHz stereo `f32` in
  exactly 960-sample frames, pushing framing onto an upstream rechunker
  (`packages/opus/processors/opus_encoder.rs:79-90,144-153`); its decoder hard-coded the
  FEC flag off with no gap handling (`opus_decoder.rs:94-95`); its packet bag was
  `{data (msgpack array), timestamp_ns (string), sample_count}` on the re-stamping implicit
  write (`opus_encoder.rs:169-173,253`). Nothing to carry: the window contract frames now.
- `packages/mp4` holds no muxer: `LinuxMp4Writer` pipes raw RGBA into an ffmpeg subprocess
  beside a synthetic silent AAC track (`mp4_writer_linux.rs:126-151`); the Apple tree is
  `#![cfg(any())]` dead code whose muxer is every-method-TODO
  (`_apple_impl_pending_/apple_muxer.rs:33-66`). One rule is worth taking: the session epoch
  is the first stamp and media stamped before it is dropped (`mp4_writer.rs:180-196,288-295`).
- `examples/camera-audio-recorder/src/main.rs` is a 31-line deferral stub;
  `examples/h264-opus-validator` touches no engine machinery and its
  `videotoolbox_encoder.rs:332-341` "AVCC reference" is a comment over `return None` in a
  file `main.rs` never declares. The last rung's reason to keep it
  (`archive/2026-09-02-python-codec-block-api.md`, REMOVED) was mistaken; it deletes
  outright, as the plan says (`:1347`).
- No encoded-audio bag, no file-writing built-in, no two-input built-in exists. The runtime
  stops on SIGINT/SIGTERM or `shutdown()` (`_engine.pyi:336-353`; no `run_for`);
  `teardown()` runs per processor after its loop exits, in graph-traversal order, never on a
  panicked thread (`execution/thread_runner.rs:82-107`, `core/compiler/compiler.rs:298-302`).
- Fan-in is legal on a port that does not window — a destination holds one subscriber per
  inbound link on one local port (`iceoryx2/input.rs:74-86,662-676`) and only a windowed
  port refuses a second link (`compiler_ops/open_iceoryx2_service_op.rs:577`). Every queued
  frame already carries its inbound link's drop counter so an eviction names the right link
  (`iceoryx2/mailbox.rs:128-140`), but no read returns the link: `read_raw` yields bytes and a
  stamp only (`input.rs:933`). The set of inbound links is readable per destination
  (`input.rs:718`). A port's mailbox exists only once a link is wired into it (`:789-804` of
  the service op), and WIRE precedes `setup()`.
- The window contract's `channels` is a required `u32` on every carrier
  (`sdk/streamlib-processor-schema/src/audio_window_contract.rs:66-86`,
  `python/streamlib/_processor_declaration.py:56-95`, `streamlib-macros/src/grammar.rs:326-360`)
  and the stage converts to it by fixed rule (`audio_window_accumulator.rs:532-539`).
- The video encoder prepends parameter sets to every IDR
  (`published_surface_to_encoded_frame_encoder.rs:394-401`), emits no B-frames
  (`vulkan/video/encode/config.rs:471-505`), and re-mints on an extent change (`:182-194`).
- Ecosystem, 2026-09-02: `opus` 0.4.0 (MIT/Apache-2.0; `Encoder`/`Decoder` for one and two
  channels, `MSEncoder`/`MSDecoder` for multistream) over `opusic-sys` 0.7.x (BSD-3-Clause;
  libopus 1.6.1 and its `COPYING` inside the crate, cmake build, static by default, no
  bindgen at build); `mp4-atom` 0.15.0 (MIT/Apache-2.0, pure Rust; `avc1`/`avcC`,
  `hvc1`/`hvcC`, `Opus`/`dOps`, the full `moof`/`traf`/`tfdt`/`trun` set). The `mp4` crate's
  writer knows no Opus. cmake already runs on the wheel builder
  (`.github/workflows/release-wheel.yml:146`).

The two §Processor model OPENs — uncounted losses (`:360-371`), pressure reflection
(`:402-408`) — stay OPEN; nothing here decides where a count lives.

**Approved by the owner, 2026-09-02**: "I'm in agreement so far" to the reshaped direction
(channels follow the source by default, one track per connection), then `/derive-tickets`
invoked after the section-by-section walkthrough with no bullet struck. No open decision
marker remains in this file.

---

## MODIFIED: §Media I/O — a window contract follows the source's channels by default

- **DECIDED** — `channels` becomes the one optional value in a window contract, and absent
  means *the source's count, whatever it is*: the stage resamples, frames and converts dtype
  exactly as decided, skips channel conversion, and every emitted window carries the count
  the block arrived with, so a consumer reads `channels` off the block rather than assuming
  it. A consumer that genuinely needs a fixed count — a model trained on mono — declares it
  and is converted by the fixed rule as today. Rewrites, in place: `:936-951` names four
  required values and one optional; `:958-976`'s "a missing field" refusal excludes
  `channels`, and the N→M refusal applies only to a declared count. `match_device` is
  unchanged — a device stream resolves a count. On the carriers `channels` is
  `Option<u32>`, `AudioWindowContract(channels=None)` in Python, `channels =` omitted in the
  Rust grammar; `graph` renders `channels: source`. Why the default and not a knob: the
  graph is dynamic and a microphone added later must not require touching every consumer
  downstream of it; a fixed count belongs only where a model asserts on it.
  [opus-mp4-recording-rung]

## ADDED: §Processor model — a read can name the inbound link it drained

- **DECIDED** — Beside `read_raw`, a reader offers a read that returns the bag, its stamp
  and the *inbound link* it arrived on, named by the source channel name the link
  subscribed to — `<lowercased producer processor id>/<output port>`, the name `graph` and
  `tap` already show (`iceoryx2/channel_name.rs:172`). The mailbox already queues each frame
  holding its link's counter; this exposes the identity the counter is keyed by, so no
  frame carries anything it did not carry before and counting is unchanged. In Python,
  `LinkInputDataReader` gains the same read (`read_from_inbound_link(port, into=T)`
  returning the cast and the link name) so a Python-authored many-input sink is possible —
  the parity bar states this rather than deferring it. A destination can also enumerate its
  inbound links at `setup()` (`input.rs:718`), which is how a sink learns how many tracks
  it owes.
  [opus-mp4-recording-rung]

## ADDED: §Media I/O — the encoded-audio bag

- **DECIDED** — An encoded audio packet is an ordinary bag, the encoded-frame convention
  applied to audio: `codec` (`"opus"`), `bitstream` (msgpack `bin`, one Opus packet as RFC
  6716 §3 frames it), `is_sync_point` (`true` on every packet — a decoder enters at any),
  `group_index` and `sequence_index` (each packet its own group), `sample_rate` (`48000`,
  Opus's own clock), `channels`, `sample_count` (per-channel samples the packet spans,
  `960` for 20 ms — `AudioBlock`'s unit), and `pre_skip` (the encoder's lookahead in 48 kHz
  samples, the `OpusHead` PreSkip a container writes and a decoder trims). The stamp rides
  the frame header and names the packet's first sample, carried from the window block the
  encoder consumed with the timestamped write. Refused by name, never reshaped: a missing
  key, a `codec` other than `opus`, a non-`bin` `bitstream`, a bag with none of these keys
  (`encoded_video_frame.rs:102-111`'s three, spelled again). The Rust struct is
  `EncodedAudioPacket` — *packet*, because Opus uses *frame* for a subdivision of one.
- **DECIDED** — `streamlib.EncodedAudioPacket` is the Python cast, pure Python beside
  `encoded_video_frame.py`, read with `into=EncodedAudioPacket`, every rule of the video cast
  verbatim (`:1293-1315`): keys are the constructor keywords, `bool` refused for integers,
  unknown keys read past, payload stored as `opus_packet_bytes` off the repr, no to-bag
  helper, no numpy property.

## ADDED: §Media I/O — the Opus pair

- **DECIDED** — `OpusEncoder` is `execution = reactive`, `scheduling = high` like the video
  blocks (`h264_decoder.rs:32-33`), input `audio` declaring `delivery_profile = "ordered"`
  and `audio_window(sample_rate = 48_000, dtype = "f32", window_size = 960, hop = 960)` —
  no channel count — so the engine resamples and frames and `process()` receives one 20 ms
  Opus frame per dispatch in the source's own channels. The encoder is minted from the
  first block's `channels`, the video encoder's first-frame pattern: one or two channels
  through `Encoder`, three to eight through `MSEncoder` with channel mapping family 1 (the
  standard surround order both MP4 and WebRTC accept), more than eight refused by name; a
  block whose count changes re-mints, as an extent change re-mints video. Output
  `encoded_audio`; `pre_skip` is the minted encoder's reported lookahead. Config, both
  optional so `{}` is legal: `bitrate_bps` (absent → libopus's automatic rate) and
  `application` (`"audio"`, `"voip"`, `"lowdelay"`; absent → `"audio"`). FEC and DTX off.
  It links `opus` 0.4 over `opusic-sys` with its bundled libopus: static, `DT_NEEDED` does
  not grow and `test_wheel_portability.py` stays the pass/fail; libopus's BSD-3-Clause
  notice joins `VENDORED_CPP_PROJECTS` (`xtask/src/generate_third_party_notices.rs:150-224`)
  read from the crate's own `COPYING` in the registry checkout — the `shaderc-sys` shape
  generalised to a second build-script crate — and `test_third_party_notices.py` names it.
- **DECIDED** — `OpusDecoder` is `reactive`/`high`, input `encoded_audio` (`ordered`),
  output `audio` as `AudioBlock` bags: `f32`, `48000`, the packet's `channels` and
  `sample_count`, stamp equal to the packet's, the timestamped write; one or two channels
  through `Decoder`, three to eight through `MSDecoder`. No config. It enters at any packet
  and trims `pre_skip` at entry so its first emitted sample is the stamped instant. A
  `sequence_index` step other than one is a gap: reset, re-enter, log the count, invent
  nothing — no concealment, no FEC decode — the drop-at-the-edge and flush-not-interpolate
  doctrine (`:852-864`, `:1028-1046`): the gap stays derivable from the stamps.

## ADDED: §Media I/O — `Mp4Sink`, many tracks by connection

- **DECIDED** — `Mp4Sink` is `reactive`/`high` with one `ordered` input, `tracks`, and no
  output. Any number of links may enter it and **each inbound link is one track**, named by
  its source channel name, so two cameras are two video tracks and three microphones three
  audio tracks with no configuration. The track's kind is the bag's `codec`: `h264`/`h265`
  a video track, `opus` an audio track, anything else refused by name — the door a caption
  or data bag walks through when its convention exists. At `setup()` the sink enumerates
  its inbound links, refusing by name when there are none; it opens `path` (required,
  created or truncated) and refuses by name a path it cannot open, the named-device shape
  (`:865-878`). Truncating is the call: an app is re-run from the same `app.py`, wall-clock
  file naming is a fifth surface the clock entry bans (`:731-748`), and refusing an existing
  file fails every second run.
- **DECIDED** — The layout is fragmented: `ftyp`, one `moov` with every track's sample entry
  and `trex`, then `moof` + `mdat` per fragment, one `traf` per track. `moov` is written once
  every track has delivered its first sync-point bag, since sample entries need the
  parameter sets and the Opus header; a link still silent is named once a second. A
  fragment closes at the first video track's sync points — each second when no video is
  wired — and carries every track's samples stamped within that span. Why fragmented:
  teardown is not a promise (a panicked thread, SIGKILL, the untrusted tier) and a flat
  file whose trailing `moov` never lands is nothing, while this one plays to its last
  closed fragment; and it is the shape (CMAF) a networking sender emits, so the writer is
  reused there. Pure Rust through `mp4-atom`, no ffmpeg, no new `DT_NEEDED` (`:1335-1337`).
- **DECIDED** — Video sample entries are `avc1`/`avcC` and `hvc1`/`hvcC` from the first
  sync-point access unit's parameter sets: H.264's profile, compatibility and level bytes
  are the SPS payload's first three; H.265's profile-tier-level, chroma and bit depths come
  from the engine's own parser (`nv_video_parser::vulkan_h265_decoder`, pub), never a
  second one. Parameter-set NALs are stripped from samples — ISO/IEC 14496-15 forbids
  in-band sets under `avc1`/`hvc1`, and `hvc1` is what Apple hardware plays, retiring the
  ffmpeg re-tag `/verify-video` shells to today (`.claude/skills/verify-video/SKILL.md:44-46`).
  Every remaining NAL is 4-byte length-prefixed, the walk reusing the engine's byte-stream
  parser (`vulkan/video/mod.rs:35`) rather than a fourth splitter; a sync-point bag is a sync
  sample. A parameter set that changes mid-file, a track whose `codec` changes, and an Opus
  track whose `channels` changes are each refused by name, **per track and never per file**:
  there is no second sample entry to switch to — one lives only in the one `moov`
  (14496-12 §6.1.2) and `dOps` shall carry the identification header's count
  (Opus-in-ISOBMFF §4.3.2) — so the sink says so once naming the link and that track's last
  written stamp, stops writing it, reads and discards every later bag it carries, and every
  other track keeps recording. A `moof` owes a `traf` to no track (§8.8.6), so a track that
  stops appearing is a legal file, and one microphone's format change must not end two
  cameras' recording. The refusal is the built-in's own latch, the shape both encoders
  already use: a `reactive` processor has no `Error` state to reach — the runner logs an
  `Err` from `process()` and carries on (`thread_runner.rs:302-305`), and the macro does not
  forward `has_failed_unrecoverably` to the authoring trait, which the trait doc states as
  the design (`generated_processor_impl.rs:129-133`).
- **DECIDED** — An Opus track is the `Opus` sample entry with `dOps` (version 0, the bag's
  `channels`, PreSkip = `pre_skip`, InputSampleRate 48 000, gain 0; mapping family 0),
  timescale 48 000, each sample's duration its `sample_count`. **PreSkip is the encoder's
  reported lookahead (312 at 48 kHz), deliberately below the 80 ms (3 840) floor
  Opus-in-ISOBMFF §4.3.2 states.** That floor is RFC 7845 §4.2's recommendation for
  *cropping an existing stream* rendered as a `shall`; the spec's own §4.7 example writes
  312, and no shipping muxer writes anything else (FFmpeg `movenc.c` ← `libopusenc.c`
  `OPUS_GET_LOOKAHEAD`, GStreamer `qtmux`, gst-plugins-rs `fmp4mux`, Xiph `libopusenc`).
  The field is not informative in practice — FFmpeg, Chromium, ExoPlayer and Android all
  discard exactly this many decoded samples — so 3 840 would destroy 73.5 ms of real audio
  and lead the video by it. With no edit list (the epoch rule), a player that keeps media
  time after the trim places the first real sample 6.5 ms late: the residual every
  FFmpeg- and GStreamer-authored Opus MP4 carries, below any lip-sync threshold, and
  present in every option.
- **DECIDED** — **Three to eight channels record no Opus track yet.** `mp4-atom` 0.15 —
  0.15.0 is the latest — writes `ChannelMappingFamily` 0 unconditionally and refuses any
  other value on read, so mapping family 1 has no representation in the container writer.
  A track above two channels is refused by name, naming the container rather than the
  codec: `OpusEncoder` still mints such a stream (`opus_stream_layout.rs` places 1–8), and
  only recording it does not follow. Owner ruling 2026-09-03, taken over hand-splicing the
  `dOps` bytes (which is the hand-written box writer this change rejected) and over
  carrying a second vendored fork. The gap is tracked; `camera-audio-recorder` is mono or
  stereo, so the showcase is unaffected.
- **DECIDED** — Time is the plan's subtraction written into the container (`:833-851`): the
  epoch is the earliest first stamp across tracks, each track's first `tfdt` is its own
  offset from it, no edit list, no drift correction. Video timescale is 1 000 000 000 — a
  legal `u32`, so monotonic-nanosecond deltas land exactly — with 64-bit `tfdt`; a video
  sample's duration is the delta to the next, so one frame per track is held back and the
  last takes its predecessor's at teardown. A bag stamped at or before its track's last
  written one is dropped and counted, a producer bug on an `ordered` input, named as such.
- **DECIDED** — `teardown()` closes the open fragment, held-back frames included, and owes
  nothing else.

## ADDED: the three blocks reach Python

- **DECIDED** — `OpusEncoder`, `OpusDecoder` and `Mp4Sink` reach Python through the five
  touchpoints the last rung fixed (`:1267-1292`); no Linux split — nothing here is
  platform-bound, so they register unconditionally beside the audio built-ins
  (`streamlib-media-builtins/src/lib.rs:88-89`). The stub docstrings state the engine's own
  behavior — the encoder's window and first-block mint, the two keys, the decoder's entry
  and gap rule, the sink's track-per-link rule, its `moov` wait, fragment rule and
  truncate-at-setup — written from the tree at implementation.

## ADDED: the proof

- **DECIDED** — CI-run, GPU-free, named in `test.yml` and the xtask mirror: the stage with
  `channels` absent emits the source's count and converts when one is declared, in Rust and
  through the Python declaration; the link-naming read returns the link a synthetic frame
  was pushed on, with counting untouched; the `EncodedAudioPacket` wire and cast; the Opus
  bodies against the real library with no `Runtime` — a tone through encode → decode
  within a stated floor for one, two and six channels, `pre_skip` aligning the first
  sample, a gap resetting; container bytes — the writer body driven with synthetic bags
  over checked-in H.264 SPS/PPS and H.265 VPS/SPS/PPS fixtures, re-parsed with `mp4-atom`:
  brands, one `trak` per link with its name, `avcC`/`hvcC`/`dOps` fields, no parameter set
  inside any sample, every NAL length-prefixed, `tfdt`/`trun` equal to stamps and counts,
  the epoch rule, a truncation at any fragment boundary re-parsing cleanly, a mid-file SPS
  change refused. The same inspection ships as `cargo xtask mp4-inspect <file>` — tracks,
  names, sample entries, fragments, durations as JSON — so nothing downstream needs ffprobe.
- **DECIDED** — Rig-only, `requires_gpu`: `test_opus_blocks.py` — a Python known-signal
  source → `OpusEncoder` → probes → `OpusDecoder` → probes: every bag casts, `sequence_index`
  advances by one, `sample_count` is 960, decoded blocks are 48 kHz `f32` in the source's
  channels with stamps 20 ms apart; `test_mp4_sink.py` — two sources into one sink give a
  file whose `mp4-inspect` names two tracks after their producers, grown by fragments while
  running, closed on stop. Marked, and run nowhere in CI, said in the module docstring.
- **DECIDED** — Live, two arms on engine-owned fixtures beside `audio_loopback_node.py` and
  `codec_roundtrip_node.py`. `opus_roundtrip_node.py`: `KnownAudioSignalSource →
  OpusEncoder → OpusDecoder → CapturedAudioWaveformRecorder`, scored by
  `known_audio_signal.py` — tone identity and the DTMF timing grid intact within its own
  floor — through `/verify-audio`'s workflow, no audio device in the path.
  `recording_node.py`: vivid camera and the known signal → `H264Encoder` and `OpusEncoder`
  → `Mp4Sink`, stopped by SIGTERM, then `mp4-inspect` PASS, then the decode-back:
  `codec_roundtrip_rig --source mp4:<path>` (rig-only demux with `mp4-atom`, length
  prefixes back to start codes, parameter sets re-prepended from the sample entry) replays
  the video track through `H264Decoder` to `xtask psnr channel-means`, locked to the
  per-codec vivid baseline within ±0.05. Ship bar: both arms PASS on a rebuilt wheel, log
  gates zero, clean exit, `h264` and `h265`.

## ADDED: §Consumers — the recording showcase

- `examples/camera-audio-recorder/` is converted from scratch per `:232-249`: `app.py` +
  `pyproject.toml` from `streamlib new`; `CameraSource → H264Encoder → Mp4Sink` and
  `MicrophoneSource → OpusEncoder → Mp4Sink`, the camera also fanned to `DisplayWindow`;
  `STREAMLIB_RECORDING_PATH` (default `recording.mp4`) and `STREAMLIB_CAMERA_DEVICE` as
  `examples/camera-display/app.py:16-35` does; Ctrl-C stops and closes the file. The held
  `Cargo.toml`, `setup.sh` and `src/main.rs` delete in the same PR. No CI presence, not the
  proof's fixture (`:282-292`); the owner may strike this bullet alone.

## MODIFIED: plan text and anchors at fold

- `ARCHITECTURE.md:1145-1147` folds the Opus pair and `Mp4Sink` to SHIPPED, leaving
  `JpegDecoder`; `:1335-1341` gain SHIPPED citations and verify lines; `:1342-1357` and
  §Consumers `:263-277` shrink the held list to `packages/jpeg` and `examples/jpeg-psnr`;
  §Consumers' count becomes eleven converted beside five held; `:936-951` and `:958-976`
  read as amended above.
- `docs/plan/diagrams/system.mmd:13` — "JpegDecoder, Opus, pure-Rust Mp4Sink still to come"
  moves Opus and `Mp4Sink` to shipped, and the audio line gains the optional channel count.
- `docs/decisions/codec-blocks.md` — "Why the recording rung is shaped this way", revised
  with this rewrite.
- `.claude/skills/verify-video/SKILL.md` — the blocker (`:17-21`) goes; the workflow becomes
  `recording_node.py` → `mp4-inspect` → decode-back → Telegram; the ffmpeg re-tag and
  ffprobe read (`:44-54`) are deleted.
- `runtime/streamlib-engine/src/vulkan/_nvjpeg_impl_pending_/README.md:11` — cites
  `packages/mp4/processors/_apple_impl_pending_/` as the pending-tree precedent;
  re-anchored to its own naming.
- `xtask/src/generate_third_party_notices.rs`, `test_third_party_notices.py` — libopus
  joins the vendored-C++ roster and the test proving the notices carry it.

## REMOVED: the mined three, and the held recorder's form

- REMOVED: packages/opus
- REMOVED: packages/mp4
- REMOVED: examples/h264-opus-validator
- REMOVED: examples/camera-audio-recorder/Cargo.toml
- REMOVED: examples/camera-audio-recorder/setup.sh
- REMOVED: examples/camera-audio-recorder/src/main.rs

## Not in scope

- **`JpegDecoder`, its own rung**: nothing in the tree produces a JPEG bag (`CameraSource`
  negotiates NV12 and YUYV only, `camera_source.rs:355-468`); `sdk/vulkan-jpeg` publishes a
  texture-ring surface (`simple_decoder.rs:29-53`) where the video decoder publishes a pooled
  buffer, and its kernel is baseline 4:2:0 only (`kernel.rs:366-420`); the `codec` enumerant
  knows no `jpeg` (`encoded_video_frame.rs:31-37`). `packages/jpeg`, `examples/jpeg-psnr`,
  `e2e_fixture_psnr_jpeg.sh` and the #1212 lock wait there; #1206 and #846 stay parked by
  plan text (`:1133-1136`).
- Caption, timed-text and data bag conventions: the sink's door is open to them; the
  conventions are their own rung.
- No concealment, FEC or DTX; no Opus knobs beyond two; no video rate-control surface (#334).
- No MP4 reader as a built-in — the rig's replay is rig tooling — no flat layout, no MoQ or
  WebRTC; the networking align owns transport and reuses track-per-link.
- No conditioning rung; the two uncounted losses and pressure reflection stay OPEN.

## Findings, not in this change

- **Three Annex-B splitters** — `byte_stream_parser` (pub), `encode/vui_patch.rs:202`,
  `decode/mod.rs:722` (private). The sink uses the public one; folding the two is its own
  DRY change.
- **The encoder capability probe** from the last rung stays owed on the video encoder alone.
- **`examples/screen-recorder`** is the same deferral-stub shape, held on screen capture,
  which has no plan entry (#374 closed noting that); untouched here.

## Validation

- **CI-run**: the stage and declaration tests, the link-naming read, wire and cast tests,
  the Opus bodies, container bytes, stubtest and pyright with three entries, one cast and
  one reader method in, `cargo xtask check-all-source-gates`,
  `test_wheel_portability.py::test_the_native_extension_links_nothing_the_host_may_not_supply`
  and `test_third_party_notices.py` green with libopus and `mp4-atom` in.
- **Rig**: `test_opus_blocks.py`, `test_mp4_sink.py`.
- **Live**: `opus_roundtrip_node.py` through `/verify-audio`; `recording_node.py` through
  `/verify-video` with `mp4-inspect` PASS and the decode-back at the vivid baseline, both
  codecs, wheel rebuilt first.
- **Gate**: `bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/opus-mp4-recording-rung.md`
  clean after the deletions; `cargo check --target aarch64-apple-darwin`.
