# moq-relays

The sharing step. After it a stream's exposed ports are tracks at relays, forwarded by a
publisher with no logic; a payload is bytes; the encoders write the stack's media frame so a
public port plays in any MoQ player; a graph reads a relay through one built-in source node; the
runtime opens no windows, reads nothing of Tailscale and serves HTTP on loopback only;
`tatolab new` writes the sample stream; `H264Encoder` encodes on the CPU where the device cannot.

**Scale gate — this skill, plus an ADR.** It changes the IPC wire (a header flag; payloads are
bytes), the processor model (`write`/`read` on bytes), the Python public contract (the two link
Protocols, two casts and `remote_output` deleted, one built-in added, one deleted), the RHI (the
present path deleted) and the unit's contents. ADR: `docs/decisions/moq-relays.md` (2026-10-16).

**Precondition.** Every entry touched is DECIDED: §Networking `ARCHITECTURE.md:3685-3949` (the
`[moq-relays]` entries), §Media I/O `:2480-2507` (windows, the sample source, the CPU arm) and the
`amended by moq-relays` pointers at `:2231`, `:2423`, `:2473`, `:3116`, `:3242`, `:3280`, `:3293`,
§Product `:30-50`, `:132-143`, §Processor model `:874-893`, `:901-908`, `:1629`, §Control plane
`:4442`. §Networking's one OPEN (`:3943`) is a list of later changes this one does not build
against.

**Verified against the tree 2026-10-16 (HEAD ac4ca55b5)**, three read-only sweeps. `E` =
`runtime/streamlib-engine/src`, `M` = `runtime/streamlib-media-builtins/src`, `W` =
`sdk/streamlib-python-wheel/src`, `S` = `sdk/tatolab-stream/tatolab/stream`, `A` =
`runtime/streamlib-api-server/src`, `C` = `runtime/tatolab-cli/src`, `D` = `runtime/tatolabd/src`.
- **The wire.** `FrameHeader { port_key, timestamp_ns, len }` is 76 bytes with documented offsets
  (`runtime/streamlib-ipc-types/src/lib.rs:455-459`, `:348`), pinned by
  `frame_header_size_matches_constant` and
  `frame_header_fields_sit_at_their_documented_wire_offsets` (`:773`, `:780`). The per-link sequence
  rides the iceoryx2 user header (`:654`), engine-internal.
- **msgpack is applied behind the call.** `write(port_name, bag, timestamp_ns=None)`
  (`W/python_processor_context/link_data_access.rs:134`) → `encode_bag_to_msgpack`
  (`W/python_processor_link_data_access.rs:705`) → `write_raw`; every read decodes in
  `decode_one_bag_into` (`:255`) and constructs `T(**bag)` in `cast_decoded_bag_into_read_target`
  (`W/python_bag_conversion.rs:123-143`). In Rust `OutputWriter::write<T>` is `rmp_serde::to_vec_named`
  (`E/iceoryx2/output.rs:548-567`), `InputMailboxes::read<T>` is `rmp_serde::from_slice`
  (`E/iceoryx2/input.rs:71`, `:1347`); `write_raw`/`read_raw` exist (`:1357`). The casts
  (`S/video_frame.py:237`, `audio_block.py:85`, `encoded_video_frame.py:97`, `encoded_audio_packet.py:106`)
  are frozen dataclasses with `from_bag` → `cls(**bag)`, none with `from_bytes`. The codec pair is
  runtime-backed (`S/_bag_codec.py:14-30`; `W/lib.rs:73-84`). The audio-window stage decodes
  `AudioBlock` keys itself (`E/iceoryx2/audio_window/audio_block_bag_wire_codec.rs:140`).
- **The encoded frame has no timestamp key**: `codec, bitstream, is_sync_point, group_index,
  sequence_index, width, height, color?` (`M/encoded_video_frame.rs:37-92`); the packet adds
  `sample_rate, channels, sample_count, pre_skip` (`M/encoded_audio_packet.rs:112`). Readers and
  writers: `published_surface_to_encoded_frame_encoder.rs:288`,
  `encoded_frame_to_published_surface_decoder.rs:204`,
  `audio_window_to_encoded_packet_encoder.rs:279`, `encoded_packet_to_audio_block_decoder.rs`,
  `mp4_sink.rs:74-77` → `mp4_fragmented_file_writer.rs:69` (`BagCodecPeek`),
  `E/../examples/codec_roundtrip_rig.rs:89`, and the WebRTC wheel
  (`packages/streamlib-webrtc/python/tatolab/webrtc/processors.py:298-303`, `:553-577`;
  `src/lib.rs:331-399`). `OpusDecoder` detects a gap by `sequence_index`
  (`encoded_packet_to_audio_block_decoder.rs`); the video decoders enter at `sequence_index=0`.
- **The tap slot is the publisher's seat.** `RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL = 1`
  (`ipc-types/src/lib.rs:327`), added in `channel_max_subscribers`
  (`E/core/compiler/compiler_ops/open_iceoryx2_service_op.rs:629-637`); `tap_async`
  (`E/core/runtime/operations_runtime.rs:1038`) → `start_channel_tap` (`E/core/runtime/tap.rs:152`).
- **Exposure is enforced at two edges.** `OutputPortExposureLevel`
  (`E/core/graph/output_port_exposure.rs:23`); `loaded_stream_output_port_exposure.rs:45-250`; the
  cross-stream edges are `connect_from_an_output_port_in_another_stream`
  (`operations_runtime.rs:632-653`) and `open_the_source_side_on_the_source_stream`
  (`reconcile_a_link_reading_another_stream_op.rs:355-366`); `expose_port` (`A/mcp.rs:1057` →
  `stream_actions_of_this_runtime.rs:799`) records `KeptStreamRecord.exposure_rulings`
  (`machine_state_directory.rs:60`, `:135`). Python `Exposure {PRIVATE, PUBLIC}` and `expose`
  (`S/_stream_graph_builder.py:44`, `:377-406`).
- **The cross-stream link.** `remote_output` → `NodeOutputPortReferenceByAddress`
  (`S/_stream_graph_builder.py:344`, `:229-251`, `:448-466`); `LinkPortRefOutput { machine?,
  stream?, node, port }` (`E/core/json_schema.rs:160-207`); `OutputPortInAnotherStreamOnThisMachine`
  (`E/core/graph/edges/output_port_in_another_stream_on_this_machine.rs:16`);
  `LinkState::AwaitingRemote` (`link_state.rs:23`); `wire_a_link_reading_another_stream`
  (`compiler.rs:504-520`); tests
  `tests/stream-on-runtime/test_links_between_streams_on_one_machine.py` and
  `E/../tests/links_between_streams_on_one_machine_test.rs` with
  `tests/support/links_between_streams_harness.rs`.
- **Tailscale.** `MachineName::of_this_machine()` (`E/core/runtime/machine_name.rs:113`) runs
  `tailscale status --json --peers=false` with a 2 s timeout (`:27-40`, `:357`); its one caller is
  `Runner::new_with_construction_options` (`E/core/runtime/runtime.rs:577`).
- **Windows.** `M/display_window.rs` (registered `M/lib.rs:107`; software-device refusal `:356`;
  generated class `S/_built_in_nodes.py:87`, `:122`); `E/core/{window_event_pump,processor_owned_window,display_info}.rs`
  (called from `runtime.rs:1014-1199`, `loaded_stream.rs:550`, `runtime_context.rs:652`);
  `E/apple/{appkit_content_view_of_winit_window,metal_layer_added_as_sublayer_of_window_content_view}.rs`;
  RHI `vulkan_present_{target,compositor}.rs`, `vulkan_swapchain_colorspace.rs`,
  `vulkan_swapchain_dma_buf_allocation_fix_validation_test.rs`, `shaders/display_blit.*` (`build.rs:202-208`),
  `gpu_context.rs:2843` `create_present_target`, `:1455-1463` — `VulkanPresentCompositor` is also
  the PipeWire virtual camera's compositor (`E/linux/pipewire_video_source.rs:36`, `:282`), not a
  window; escalate family `subprocess_escalate/processor_owned_window/`, wire variants
  `escalate_request.rs:31-91`, `escalate_response.rs:61-114`; `W/python_processor_owned_window.rs`,
  `W/python_helper_process_pixel_exchange/processor_owned_window.rs`; Protocols
  `S/_node_owned_window_protocols.py`, `_gpu_protocols.py:193`; `winit = "0.30"`, `raw-window-handle`
  (`E/Cargo.toml:174-175`; root `:130` enables vulkanalia's `window`), `objc2-app-kit`,
  `objc2-quartz-core` (root `:114-115`). Tests: engine `processor_owned_window_*.rs` ×5,
  `window_event_pump_*.rs` ×2 (`E/Cargo.toml:256-270`); media `camera_to_display_window_on_the_macos_window_server.rs`,
  `two_display_windows_*.rs` + harness; `tests/stream-on-runtime/test_{display,processor_owned}_window.py`
  with their `*_streams.py`/`*_probes.py`, and `DisplayWindow` in six more stream fixtures there;
  `D/../tests/tatolabd_hosts_a_stream_on_the_gpu.rs`; rig fixtures `camera_display_stream.py`,
  `codec_roundtrip_stream.py`, `e2e_camera_display.sh`, `e2e_fixture_psnr.sh`, `capture_window.py`;
  golden graphs `000{1,2,3}-*.json`; eight `examples/*/app.py`. Display environment:
  `D/display_server_environment_from_the_user_manager.rs` (`D/main.rs:33-84`, `machine_stream_hosting.rs:28-65`),
  `installer/tatolabd-restart-for-a-display-at-login.service` (`install.sh:29`,
  `build_linux_runtime_release_tarball.sh:96`), marker `tatolabd-started-with-no-display-server`
  (`streamlib_runtime_directory.rs:150-154`).
- **TestPatternSource is CPU bars**: `M/test_pattern_source.rs` fills one `Rgba32` pixel buffer with
  `fill_smpte_bars_rgba` (`:63`) and republishes the same surface id every 33 ms; config `width`,
  `height`. `tatolab new --test-pattern` (`C/main.rs:110-113`, `:272-275`) string-replaces the one
  template `sdk/tatolab-stream/scaffold_template/stream.py` (`CameraSource` + `DisplayWindow`,
  `:14-27`) through `ScaffoldedStreamSource::{Camera, TestPattern}`
  (`C/scaffold_new_stream_project.rs:41-119`).
- **`tap` and `exchange`.** `C/main.rs:195`, `:236`, `C/surface_image_exchange.rs`, `TAP_TOOL_NAME`
  (`C/runtime_observation_verbs.rs:16`); `A/handlers.rs:55` `/api/surfaces/{surface_id}/image`
  (`local_api_wire_contract.rs:24`), `:111` `/ws/tap/{channel}`; `A/mcp.rs:602-712`;
  `CONTROL_TOOL_NAMES` (`:1476-1491`). The PNG conversion is
  `exchange_published_surface_id_for_png_image_bytes_async` (`operations_on_the_streams_loaded_in_this_runtime.rs:42-279`)
  over `E/core/runtime/surface_image_exchange.rs`. Fixtures on them: `e2e_fixture_psnr.sh:275`,
  `e2e_fixture_psnr_vivid.sh:343`, `e2e_fixture_recording.sh:472`,
  `packages/streamlib-webrtc/tests/live/whip_whep_roundtrip.sh`; `.claude/skills/verify-live/SKILL.md:25-59`.
- **No URL listener exists.** The api-server binds a Unix socket alone (`A/local_api_socket.rs:71`;
  `serving_the_router_on_the_local_api_socket_opens_no_tcp_listener` `:464`); no route serves a port
  by address or lists exposed ports.
- **The codec seam.** `VideoCodecBackend` (`E/core/context/video_codec_backend.rs:175`), the walk
  `first_video_codec_backend_arm_that_opens` (`:233`) over `platform_video_codec_backend_arms`
  (`:250-272`: Linux `vulkan-video`, macOS `videotoolbox`, else none) into the refusing backend
  (`refusing_null_video_codec_backend.rs:19`); the encoder opens its session at
  `published_surface_to_encoded_frame_encoder.rs:418-420`. No `openh264` anywhere.
- **The unit.** `xtask/src/build_runtime.rs:167-623` lays `bin/`, `lib/tatolab/lend/`; sibling
  native binaries are fetched by pinned tag and SHA-256 in
  `scripts/stage_{linux,macos}_bundled_vulkan_driver.sh`; the portability gate walks every binary
  (`test_wheel_portability.py:397-590`). `tatolabd` supervises no long-lived child
  (`D/machine_stream_hosting.rs:45-134`); the state directory holds `streams/`, `logs/` and a run
  marker only (`tatolab_state_directory.rs:105-116`) — no machine-level setting. No QUIC or MoQ
  crate in the workspace; `tokio 1.48` (`Cargo.toml:66`); `rustls` only in the WebRTC wheel.

## MODIFIED: the wire — §Processor model `ARCHITECTURE.md:874-893`

`FrameHeader { port_key, timestamp_ns, len, flags: u32 }`, 80 bytes, `FRAME_HEADER_FLAG_STARTS_GROUP
= 1 << 0`; the two layout tests move to 80 and the offsets, a third pins the bit. A payload is
bytes: `write(port_name, payload: bytes, timestamp_ns: int | None = None, starts_group: bool =
True)`; `read(port_name) -> bytes | None`, `read_with_timestamp` and `read_from_inbound_link*`
return bytes where they returned a dict; `read(port_name, into=T)` calls `T.from_bytes(payload)`
inside the typed-read GPU offer (`cast_decoded_bag_into_read_target` → `cast_payload_bytes_into_read_target`,
the TypedDict arm gone). Rust: `OutputWriter::write`/`write_with_timestamp` renamed
`write_msgpack`/`write_msgpack_with_timestamp`, `write_bytes_with_timestamp(port, &[u8], stamp,
starts_group)` the wire call; `InputMailboxes::read::<T>` → `read_msgpack::<T>` beside `read_raw`.
`tatolab.stream` ships `tatolab.stream.msgpack` — `pack(mapping) -> bytes`, `unpack(bytes)` over the
`msgpack` PyPI package — and `MsgpackCast`, a mixin giving a dataclass or TypedDict-shaped class
`from_bytes` (unpack, then the existing keyword construction) and `to_bytes`. `VideoFrame` and
`AudioBlock` take it (`from_bag` deleted); the runtime-backed codec pair is deleted and the
conformance list shrinks by two. The audio-window stage keeps reading `AudioBlock` at its seam.

## ADDED: catalog fragments — §Networking `ARCHITECTURE.md:3872`

`@node.output(name, description, catalog=CatalogFragment(kind="binary", encoding="msgpack",
cast="acme_types:Detection"))`; `output(…, catalog = …)` in the Rust grammar. One optional struct
on `ProcessorPortSchema`, `PortDescriptor`, `PortInfo` as `audio_window` is, rendered under the
port in `graph` and `/api/registry`; a node replaces its own while running through one escalate
op `update_output_port_catalog_fragment {port, fragment}` (stored on the port; no payload read).
`kind` ∈ `video`, `audio`, `binary`; `encoding` is text the publisher copies. Built-ins:
`CameraSource.video` and `TestPatternSource.video` → `binary`/`msgpack`/`tatolab.stream:VideoFrame`;
`MicrophoneSource.audio` → `binary`/`msgpack`/`tatolab.stream:AudioBlock`; `H264Encoder`,
`H265Encoder` → `video`, container `legacy`, `codec` and coded extent after the first frame;
`OpusEncoder` → `audio`, `codec: "opus"`, `channels`, `sample_rate`, `pre_skip` after the first block.

## MODIFIED: the encoded media wire — §Media I/O `:3116`, `:3242`, `:3280`, `:3293`

`H264Encoder`/`H265Encoder` write `varint(timestamp_µs) ++ annex_b_access_unit`, `starts_group =
is_keyframe`; `OpusEncoder` writes `varint(timestamp_µs) ++ opus_packet`, each message its own
group. The decoders, `Mp4Sink` (reading the varint and the envelope stamp, the codec from the
track's fragment through `inbound_link_catalog_fragment(port)` on the reader), the WebRTC
publisher and player, the codec rig and the fixtures read and write that frame.
`EncodedVideoFrame`, `EncodedAudioPacket`, their casts, tests and `_built_in_nodes.py` key lists
are deleted; `encoded_stream_ordering.rs` goes with the ordering pair.

[NEEDS DECISION] How a decoder detects a lost message on an `ordered` link once no payload carries
`sequence_index`. (a) **Recommended:** the reader exposes the count the engine already keeps —
`read_from_inbound_link_with_timestamp` returns `lost_since_last_read` for that link (the mailbox
eviction and ring-gap counts of loss-visibility, read and zeroed per read); a decoder with a nonzero
count discards until the next `starts_group`. No payload key, no new counter. (b) A producer
sequence inside the payload — non-standard, and a stock player stops on it. (c) Codec-level
detection (`frame_num`, Opus TOC) — unreliable with hardware decoders. Spelling and the Rust twin
are the ticket's.

## ADDED: the publisher, the adapters, the local relay — §Networking `:3833-3908`

- `E/core/runtime/exposed_port_publisher.rs`: one `ExposedPortPublisher` per `Runner`, built in
  `new_with_construction_options` beside the surface service. `ExposedOutputPortsComponent::set_level`
  gains a listener; per exposed port it holds one reader on the reserved slot
  (`RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL` renamed `RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL`,
  the tap subscription's reader reused) and hands each `(payload_bytes, timestamp_ns, starts_group)`
  to every `RelayAdapter` whose level set includes the port's. Per stream and adapter one
  `moq_net::Broadcast` `<root>/<machine>/<stream>`, one track per exposed port `<node>/<port>`, and
  `catalog.json` regenerated when a fragment or the exposed set changes — entries by track name
  under `video`, `audio`, `binary`. The selection predicate is one `fn offers(&self, port) -> bool`.
- `trait RelayAdapter { dialect, credential, root, levels_served }`: `LocalLoopbackRelayAdapter`
  (every exposed port), `LocalEveryInterfaceRelayAdapter` (public), `CloudflareRelayAdapter`,
  `MoqProRelayAdapter`; a hosted adapter refreshes its credential and reconnects with backoff.
- The local relay: `moq-relay` 0.17.x built from the pinned crate into the unit as `bin/moq-relay`
  (`RUNTIME_UNIT_BINARY_PACKAGES_AND_TARGETS`; the portability gate walks it), run by `tatolabd`
  as two supervised children from `machine_stream_hosting.rs` — loopback `127.0.0.1:4443` with
  `tls.generate=["localhost"]` and `[web.http]` `127.0.0.1:4444` (`/certificate.sha256`,
  `/announced/`); every interface `[::]:4445`, `--auth-public` on the public root — restarted
  with backoff, ended with the runtime; `moq-relay` runs no code of ours.
- `machine_name.rs` keeps the hostname cast and loses the Tailscale read.
- `moq-net 0.3.x`, `moq-tokio 0.19.x`, `moq-relay 0.17.x`, `hang 0.21.x` (catalog types) pinned
  exact in the workspace; `rustls` enters the engine workspace through `moq-tokio`.

[NEEDS DECISION] How a machine joins a hosted relay. (a) **Recommended:** CLI verbs `tatolab relay
join <url> --credential-file <path>` / `relay leave <url>` / `relays`, each a local-API tool
(`join_relay`, `leave_relay`, `list_relays`), recorded in a machine-level `relays.json` in the state
directory (mode 0600) beside `streams/`, re-joined at every start; the app and a future control
client call the same tools. (b) A settings file the user edits, no verb. (c) The app only, no CLI.
(a) keeps "every stream verb is a local-API call" true for a machine setting too.

## ADDED: the relay source node — §Networking `:3908`

`RelayTrackSource`, a native built-in (`M/relay_track_source.rs`), `execution = manual`, config
`relay_url` (default the local loopback relay) and `track_address`
(`<machine>/<stream>/<node>/<port>`), one output `track`. A processor-owned thread runs a `moq-net`
subscription and writes each object's payload with `write_bytes_with_timestamp`, stamp
`MediaClock::now()`, `starts_group` true on the first object of each group; the track's catalog
fragment is copied onto the output port through the fragment update, so a reader's
`inbound_link_catalog_fragment` sees the far end's. A relay refusing or vanishing is retried with
backoff, never ending the stream; `stop()` closes the session inside the 5 s budget.

## REMOVED: `remote_output` and the cross-stream link — §Networking `:3685`, `:3778`

The builder's `remote_output`, `NodeOutputPortReferenceByAddress`, `_link_source_end`'s machine
and stream keys; `LinkPortRefOutput.{machine,stream}`, `OutputPortInAnotherStreamOnThisMachine`,
`LinkState::AwaitingRemote`, `reconcile_a_link_reading_another_stream_op.rs`,
`wire_a_link_reading_another_stream`, `connect_from_an_output_port_in_another_stream`, the
`<end>_stream` arguments of `connect`; a link end naming a stream or machine is refused at load
naming `RelayTrackSource`. The exposure edge collapses to the publisher's level set:
`register_a_reader_of_an_exposed_output_port` and its refusals go, `set_output_port_exposure_level`
notifies the publisher. Tests: the two cross-stream suites and their harnesses are deleted;
`test_a_link_to_an_internal_port_awaits…` and `…lowering_the_source_to_internal_cuts…`
are rewritten against the publisher (a private port appears at the loopback relay, a public
one at both, lowering withdraws the track and the subscriber ends).

## ADDED: loopback HTTP — §Networking `:3807`, §Control plane `:4442`

`A/loopback_forms_listener.rs`: TCP on `127.0.0.1:4446`, started with the local API: `GET /`
(JSON: machine, streams, each exposed port with level and fragment), `GET /watch/<machine>/<stream>[/<node>/<port>]`
(the preview page), `GET /snapshot/<machine>/<stream>/<node>/<port>` (one PNG). The snapshot asks
the publisher for the port's latest payload and, where the fragment's `cast` is
`tatolab.stream:VideoFrame`, unpacks `surface_id` and runs the kept PNG conversion
(`surface_image_exchange.rs` stays engine-internal; the `exchange` verb, tool and route and the
`tap` verb, tool and WebSocket go); another fragment answers 415 naming it. The page is
`@moq/watch`'s published dist, fetched at `build-runtime` by pinned version and SHA-256 into
`lib/tatolab/preview/` (`scripts/stage_preview_page.sh`, the MoltenVK shape), served static,
reading the loopback relay through its fingerprint endpoint. Fixtures and `verify-live` read
`/snapshot/` where they read `tap` + `exchange`; `capture_stream_evidence` and `tap-live-channel`
are rewritten or deleted with them.

## REMOVED: windows — §Media I/O `:2480`

Every site the sweep lists: `display_window.rs`, its registration, floors row and generated class;
the three `E/core` window files and their call sites; the two Apple files; the four RHI present
files, `display_blit.*`, `create_present_target` and the compositor cache —
`VulkanPresentCompositor` stays as the virtual camera's, renamed `VulkanOffscreenCompositor`; the
processor-owned-window escalate family and wire variants; the wheel's two window files and
`create_window`; the two Protocols and their exports; `winit`, `raw-window-handle`, vulkanalia's
`window` feature, `objc2-app-kit`, `objc2-quartz-core`; every test, fixture and harness named;
the three golden graphs rewritten without `DisplayWindow` beside a fourth holding
`RelayTrackSource`; the eight examples re-authored to expose their output (filed per example,
never blocking); the display-environment file, the restart unit, `install.sh:29`,
`build_linux_runtime_release_tarball.sh:96`, the no-display marker;
`supports_window_system_surface` and the software-device refusal; `check_boundaries.rs:323`.

## MODIFIED: the sample stream — §Product `:30-50`, `:132-143`; §Media I/O `:2497`

`TestPatternSource` renders on the GPU: one compute kernel over `create_compute_kernel` writing the
pooled `Rgba32` frame per tick — SMPTE bars, the potato sprite bouncing corner to corner,
"Tatolab stream setup successfully" and "sample stream" bottom-right — glyph atlas and sprite
baked as `include_bytes!` RGBA, no font library; config and fragment as the camera's.
`scaffold_template/stream.py` becomes `TestPatternSource → InvertingEffect → H264Encoder →
expose(encoder.output("video"), Exposure.PUBLIC)` with the `BrightnessMeter` fan-out and a comment
naming `CameraSource` as the one-line swap; `--test-pattern` and `ScaffoldedStreamSource` go.
`test_the_scaffolded_app_reaches_a_running_graph` asserts the track at the loopback relay through
a `moq-net` client (catalog plus one group); `new_writes_the_camera_stream_project…` becomes
`new_writes_the_sample_stream_project…`. The MVP sentence's window clauses fold.

## ADDED: `H264Encoder`'s CPU arm — §Media I/O `:2497`

`E/core/context/openh264_video_codec_backend.rs`, a third arm after the platform's in
`platform_video_codec_backend_arms` on every floor, opening only when the arms before it refused:
the `openh264` crate (0.9.x, `libloading` feature, no bundled source) over Cisco's
`libopenh264-<ver>-linux64.8.so` / `…-mac-arm64.dylib`, fetched once by `tatolabd` on first use
into `<state>/openh264/<ver>/` from `ciscobinary.openh264.org`, SHA-256 pinned in the binary, the
Cisco notice added to the third-party notices; a fetch refused (offline) refuses the encoder by
name saying so. Encode only. `bitrate_bps` → rate control, `keyframe_interval_seconds` → IDR
period, `effort_level` refused by name.

## Tree consequences, by slice

1. **Bytes on the wire** — header flag, `write`/`read` both sides, `tatolab.stream.msgpack`,
   `MsgpackCast`, codec pair deleted, `from_bytes` on the two casts, the fragment on the
   declaration, in `graph` and its update op. Proof: layout tests; `test_read_into_target.py`
   over bytes; raw bytes written by one node read by another.
2. **The media frame** — the video blocks, the Opus pair, `Mp4Sink`, the codec rig, the WebRTC
   wheel, two casts deleted, the decided loss mechanism. Proof: the PSNR rigs lock to their
   baselines unchanged; a hang-catalog validator over the encoders' fragments.
3. **Publisher, adapters, local relay** — `moq-relay` in the unit, two children, the publisher,
   `tap` and the Tailscale read deleted, the join verbs. Proof: a `moq-net` client reads a public
   track at both listeners and a private one at loopback only, byte-identical to iceoryx2;
   lowering a level ends the subscription; the catalog names every exposed port.
4. **`RelayTrackSource`** and the deletion of `remote_output`. Proof: stream B decodes stream A's
   private port through the loopback relay; two runtimes (rig, or two containers) read a public port.
5. **Loopback HTTP** — listener, listing, snapshot, the staged preview dist; `exchange` deleted;
   fixtures and skills rewritten. Proof: rigs green on `/snapshot/`; the page plays the sample
   stream in Chromium on the rig (Playwright, rig-only).
6. **Windows deleted.** Proof: both floors build with no `winit`; no lane needs a display;
   `tatolabd` starts under a user manager with no display.
7. **The sample stream** — GPU `TestPatternSource`, the scaffold, `tatolab new`. Proof: the
   scaffold on the lavapipe lane reaches a track at the loopback relay with ≥ 60 groups.
8. **The CPU encoder arm.** Proof: on `VK_DRIVER_FILES=lvp` the scaffold plays; the PSNR rig on
   the CPU arm meets the Y floor.
9. **Hosted adapters** — Cloudflare (drafts 14/16), moq.pro; credentials from the environment;
   live proof rig-only, cannot-run without them.

1 and 6 start at once; 2, 3 after 1; 4, 5, 9 after 3; 7 after 2 and 6; 8 any time.

## Assumptions stated, not asked

- `flags: u32`, not `u8`, keeps the header 4-byte aligned; bits 1–31 reserved zero.
- The Rust msgpack write is named `write_msgpack`, so a built-in's encoding is visible at its call.
- `msgpack` from PyPI is the stream package's one new dependency; the stream suite stays runtime-free.
- The hang `legacy` container (varint µs + payload) is the media frame: every player on the line reads it.
- Fixed ports 4443–4446 on loopback, 4445 on every interface; one in use refuses the start by name.
- Two `moq-relay` children, not one, because `moq-relay` binds one QUIC address.
- A hosted relay's root is the credential's; the hostname sits under it.
- `RelayTrackSource` stamps arrival; the writer's stamp rides the object.
- The preview page is `@moq/watch`'s published dist; no node toolchain enters the build.
- Snapshot serves `VideoFrame` records only; an encoded or data port answers 415.
- Cisco's binary is fetched at first use, never at install, so the installer stays offline-safe.

## REMOVED

- REMOVED: remote_output
- REMOVED: AwaitingRemote
- REMOVED: NodeOutputPortReferenceByAddress
- REMOVED: reconcile_a_link_reading_another_stream_op.rs
- REMOVED: RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL
- REMOVED: TAP_TOOL_NAME
- REMOVED: SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE
- REMOVED: encode_bag_to_msgpack_bytes
- REMOVED: decode_msgpack_bytes_to_python_object
- REMOVED: EncodedVideoFrame
- REMOVED: EncodedAudioPacket
- REMOVED: TAILSCALE_STATUS_READ_TIMEOUT
- REMOVED: window_event_pump.rs
- REMOVED: processor_owned_window
- REMOVED: vulkan_present_target.rs
- REMOVED: create_present_target
- REMOVED: NodeOwnedWindow
- REMOVED: winit = "0.30"
- REMOVED: display_server_environment_from_the_user_manager.rs
- REMOVED: tatolabd-restart-for-a-display-at-login.service
- REMOVED: new_with_test_pattern_wires_the_test_pattern_and_names_no_camera
- REMOVED: fill_smpte_bars_rgba
