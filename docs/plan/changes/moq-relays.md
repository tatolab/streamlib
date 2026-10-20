# moq-relays

The sharing step, re-issued after the owner's realignment of 2026-10-17. After it a stream's
exposed ports are tracks at relays, forwarded by a publisher in `tatolabd` with no logic; a port
describes itself in the players' own catalog layout; the encoders write the stack's media frame;
a graph reads a relay through one built-in source node; relays are adapters, one crate each; the
runtime opens no windows, serves HTTP on loopback only, keeps no side door for verification.

**Scale gate — this skill, plus an ADR.** It changes the processor model (an output port carries
a catalog entry; a reader sees the group flag and a loss count), the Python public contract (one
read added, `remote_output` and `Mp4Sink` deleted, one built-in added), the RHI (the present path)
and the unit's contents. ADR: `docs/decisions/moq-relays.md` (2026-10-16, amended 2026-10-17).
It supersedes the 2026-10-16 issue of this file in full.

**Precondition.** Every entry touched is DECIDED on the realigned plan: §Networking
`ARCHITECTURE.md:3836-4028` (its one OPEN `:4029` lists later changes), §Processor model
`:877-918`, §Media I/O `:2483-2514`, `:3047`, `:3129`, `:3297`, `:3361-3530` (the `Mp4Sink`
entries, pointer-tagged), `:3744`; §Product `:30-50`; §Control plane `:4301`, `:4384`.

**Verified against the tree 2026-10-17 (HEAD c6a7d316d)**, three read-only sweeps. `E` =
`runtime/streamlib-engine/src`, `M` = `runtime/streamlib-media-builtins/src`, `W` =
`sdk/streamlib-python-wheel/src`, `S` = `sdk/tatolab-stream/tatolab/stream`, `A` =
`runtime/streamlib-api-server/src`, `C` = `runtime/tatolab-cli/src`, `D` = `runtime/tatolabd/src`,
`I` = `runtime/streamlib-ipc-types/src`, `P` = `sdk/streamlib-processor-schema/src`,
`T` = `runtime/streamlib-engine/tests/fixtures`, `X` = `xtask/src`.
- **Bytes on the wire shipped.** `FrameHeader { port_key, timestamp_ns, len, flags }`, 80 bytes,
  `FRAME_HEADER_FLAG_STARTS_GROUP = 1 << 0`, `MessageGroupBoundary` (`I/lib.rs:348-386`, `:488-525`;
  tests `:845-920`); `write_bytes_with_timestamp(port, &[u8], ts, MessageGroupBoundary)`
  (`E/iceoryx2/output.rs:365-411`; a ceiling refusal counts at `:390`); `write_msgpack_with_timestamp`
  always stamps `StartsGroup` (`:568-582`); `MessageReadFromInputPort { payload_bytes, timestamp_ns,
  group_boundary }` (`E/iceoryx2/input.rs:66-73`); `read_raw[_from_inbound_link]`, `read_msgpack*`
  (`:1342-1380`). Python: `write(port_name, payload: bytes, timestamp_ns=None, starts_group=True)`
  (`S/_node_context_protocols.py:360-377`); `read`, `read_with_timestamp`, `read_from_inbound_link[_with_timestamp]`,
  `inbound_link_names` (`:245-346`; wheel `W/python_processor_context/link_data_access.rs:16-152`) —
  **none exposes the group flag or a loss count**. `S/msgpack.py:24-30` exports `pack`, `unpack`,
  `MsgpackCast`, `MsgpackTypedDictCast`, `msgpack_typed_dict_cast`. Conformance:
  `sdk/streamlib-python-wheel/tests/runtime_backed_protocol_conformance.py`, stub `python/tatolab/runtime/_engine.pyi`.
- **Loss counts are cumulative, never per read.** `DroppedBagCountsByInboundLink` / `RefusedBagCountsByOutputPort`
  (`E/iceoryx2/loss_counters.rs:142`, `:262`); ring gaps charged in `receive_pending` (`input.rs:908-935`),
  eviction `:127-128`, helper mirror `:683`; `graph` keys `dropped_bags_by_link`, `refused_bags_by_output_port`
  (`E/core/graph/components/processor_metrics.rs:60`, `:126`). No `lost_since_last_read` exists.
- **A port declares name, description, delivery profile, audio window.** `ProcessorPortSchema.audio_window`
  (`P/processor_schema.rs:396`), `PortDescriptor.audio_window` (`P/descriptors.rs:40`), `PortInfo.audio_window`
  (`E/core/graph/nodes/port_info.rs:31`); `graph` renders `ProcessorNodePorts` (`E/core/graph/nodes/processor_node_ports.rs:9`),
  the registry `PortDescriptorOutput` (`E/core/json_schema.rs:359-373`, `A/handlers.rs:394-410`); `@node.output(name=None, *, description="")`
  (`S/_node_declaration.py:389-411`); the macro's `output("<name>"[, description = …])` rejects every
  other keyword (`sdk/streamlib-macros/src/grammar.rs:347-350`, `:404`). Escalate-op pattern: `EscalateRequest::WaitDeviceIdle`
  (`…/escalate_request.rs:95`, `subprocess_escalate/mod.rs:135`; wheel `W/python_helper_process_pixel_exchange/mod.rs:804-809`).
- **The reserved slot and exposure.** `RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL = 1` (`I/lib.rs:327`;
  `open_iceoryx2_service_op.rs:629-638`); `start_channel_tap` → `TapSubscription { receiver, dropped_bags }`,
  dropping the newest on a full forwarder (`E/core/runtime/tap.rs:152-157`, `:270-287`);
  `ExposedOutputPortsComponent::set_level` returns the readers it cuts (`exposed_output_ports_component.rs:167-197`);
  `LoadedStreamInThisRuntime::{set_output_port_exposure_level, register_a_reader_of_an_exposed_output_port}`
  (`loaded_stream_output_port_exposure.rs:45-135`; one caller `stream_actions_of_this_runtime.rs:2717`);
  `RuntimeOperations` (`E/core/runtime/operations.rs:47-172`) carries `tap_async` (`:109`) and no
  exposure operation. `tatolabd` holds `Arc<Runner>` directly (`D/machine_stream_hosting.rs:52-57`, `:91`;
  deps `runtime/tatolabd/Cargo.toml:34-43`), spawns no child and supervises nothing (`:28-134`).
- **The encoders.** `HardwareVideoEncoderConfig { width, height, fps, bitrate_bps,
  keyframe_interval_seconds (2), effort_level }` (`M/published_surface_to_encoded_frame_encoder.rs:51-75`);
  session minted from the first frame (`:386-447`), VUI from `color_info` (`:401-413`); the bag built
  `:277-300`, the ordering counter `:283-285` (`M/encoded_stream_ordering.rs:32-55`); `h264_encoder.rs:79-83`
  writes `write_msgpack_with_timestamp`, so the keyframe never reaches the header flag. Vulkan: streaming
  Annex-B, H.264 HIGH autoselected (`E/vulkan/video/vulkan_video_codec_backend.rs:88-143`, `:477`);
  VideoToolbox: `H264_High_AutoLevel`, AVCC → Annex-B on sync samples (`…/videotoolbox_encode_session.rs:316-319`,
  `:610-640`). The H.265 SPS parser is engine-side (`E/core/h265_sequence_parameter_set.rs`); the H.264
  one lives only in the WebRTC wheel (`packages/streamlib-webrtc/src/h264_sequence_parameter_set.rs:14-52`).
- **The decoders** gate on `sequence_index`/`is_sync_point` (`M/encoded_frame_to_published_surface_decoder.rs:212-220`;
  `encoded_stream_ordering.rs:91-175`) and rank the SPS VUI over the bag colour (`:243-246`).
  `OpusEncoder` writes `pre_skip = lookahead` (`M/audio_window_to_encoded_packet_encoder.rs:277-288`,
  `opus_encoder.rs:83`); `OpusDecoder` trims by it (`encoded_packet_to_audio_block_decoder.rs:213-225`).
- **Sources write msgpack records**: `camera_source.rs:183` (`VideoFrame`), `microphone_source.rs:353-357`
  (`AudioBlock`), `h264_decoder.rs:90-94`, `opus_decoder.rs:85`; `test_pattern_source.rs:107-188` fills one
  buffer on the CPU (`fill_smpte_bars_rgba` `:64`) and republishes one `surface_id` per tick; `create_compute_kernel`
  (`E/core/context/gpu_context.rs:2723`). Generated classes: `X/generate_built_in_node_classes.rs:69-355`, gate `ci.yml:139`.
- **CI, the unit, the control plane.** No URL listener exists (`A/local_api_socket.rs:71`, Unix only);
  `CONTROL_TOOL_NAMES` (`A/mcp.rs:1476-1491`, test `:1848`); CLI verbs `C/main.rs:99-366`, help test
  `tatolab_new_and_help.rs:66-98`; ffmpeg already serves the rig (`T/virtual_camera.sh:21-39`,
  `T/psnr/generate_fixtures.sh:23`), never CI. `RUNTIME_UNIT_BINARY_PACKAGES_AND_TARGETS = [tatolabd, tatolab]`,
  built with the `machine-directories-under-a-test-root` feature and copied to `bin/` (`X/build_runtime.rs:69-82`,
  `:269-400`); siblings staged by pinned tag + SHA-256 (`scripts/stage_{linux,macos}_bundled_vulkan_driver.sh:20-44`);
  the portability gate walks `bin/` and the lend (`test_wheel_portability.py:111`, `:397-461`); notices
  `test_third_party_notices.py:75-146`. The macOS gate runs `tatolab new --test-pattern` and asserts
  `DisplayWindow: stopped (N frames)` ≥ 60 (`ci.yml:852-918`; `reusable-macos-runtime-unit.yml:189-253`);
  `reusable-distributions.yml:158` runs `--test-pattern`; no workflow installs node or bun but `control-plane-client.yml:55`.
  Scaffold: `sdk/tatolab-stream/scaffold_template/stream.py:14-33` (`CameraSource → InvertingEffect → DisplayWindow`
  + `BrightnessMeter`), `pyproject.toml:5` (`tatolab-stream>=0.41`; workspace 0.46.14); `C/main.rs:112-113`,
  `C/scaffold_new_stream_project.rs:41-66`; tests `tatolab_new_and_help.rs:101`, `:189`, `:221`. Tailscale:
  `machine_name.rs:30-35`, `:329`, `:358-362`, one caller `runtime.rs:577`. No MoQ, QUIC, `rustls`, `keyring` or `secrecy` crate.

## MODIFIED: the reader sees the group and its losses — §Processor model `:877`, `:3129`

`MessageReadFromInputPort` gains `lost_since_last_read: u64`: the inbound link's dropped-bag
count minus its value at this reader's previous message from that link, so a cumulative counter
answers "since my last read" with no zeroing. Python gains one read: `read_message(port_name, *,
into=None) -> Message[bytes] | Message[T] | None`, a frozen dataclass `Message(payload,
timestamp_ns, starts_group, lost_since_last_read, inbound_link)` in `tatolab.stream`, with its
Protocol row, `_engine.pyi` entry and conformance row; the four shipped reads stay. **Verify, not
assume** (owner, 2026-10-16): the proof drops messages on a running ordered link (the shipped
`a_stalled_windowed_consumer…` shape) and asserts the count reaches the next read, in a helper too.

## ADDED: catalog entries — §Networking `:3938`, §Processor model `:903`

An output port's entry is a hang entry: `CatalogEntry { section: CatalogSection, entry: Value }`
on `ProcessorPortSchema`, `PortDescriptor` and `PortInfo` beside `audio_window`, rendered as
`catalog_entry` under the port in `graph` and `/api/registry`. A declared or updated entry is
parsed with the `hang` crate's own types (`VideoConfig`, `AudioConfig`, `TextConfig`, `JsonConfig`,
`BinaryConfig`); one that does not parse is refused by name at declaration — the one check, on the
description and never on a payload. Python: `@node.output(name, description,
catalog=VideoEntry(codec=…, codedWidth=…, codedHeight=…))`, with `AudioEntry`, `TextEntry`,
`JsonEntry(mode=…)`, `BinaryEntry(mode=…, mime=…)` in `tatolab.stream.catalog`, dataclasses whose
`to_dict()` spells hang's field names; Rust `output("video", catalog = VideoEntry { … })`. A node
replaces its own entry while running: Python `ctx.outputs.set_catalog_entry(port_name, entry)` over
one escalate op `set_output_port_catalog_entry {port, section, entry}` (the `WaitDeviceIdle` shape),
Rust `OutputWriter::set_catalog_entry`; a reader asks `inbound_link_catalog_entry(port_name) -> dict | None`
(Rust `Option<CatalogEntry>`) for the upstream port's current entry. Built-ins: `CameraSource.video`,
`TestPatternSource.video`, `H264Decoder.video`, `H265Decoder.video`, `MicrophoneSource.audio`,
`OpusDecoder.audio` → `BinaryEntry(mode="snapshot", mime="application/msgpack")`;
`H264Encoder`/`H265Encoder` → `VideoEntry` after the first frame, `codec` from the SPS the arm
emitted (`avc1.PPCCLL` / `hvc1.…`), `codedWidth`/`codedHeight` the coded extent, no `description`,
`container` the default; `OpusEncoder` → `AudioEntry(codec="opus", sampleRate=48000,
numberOfChannels, description=<OpusHead hex>)` after the first block. `cargo xtask
generate-built-in-node-classes` renders each entry in the class doc; `--check` stays.

## MODIFIED: the encoded media wire — §Media I/O `:3129`, `:3297`, `:3047`

`H264Encoder`/`H265Encoder` write `varint(timestamp_µs) ++ annex_b_access_unit` with
`write_bytes_with_timestamp(…, if is_keyframe { StartsGroup } else { ContinuesGroup })`;
`OpusEncoder` writes `varint(timestamp_µs) ++ opus_packet`, `StartsGroup` every packet. The
codec string comes from a new engine-side `E/core/h264_sequence_parameter_set.rs` (profile,
constraints, level) beside the H.265 one; colour stays in the VUI the arm already patches. A
write refused at the channel ceiling sets the encoder's force-keyframe for its next frame
(Vulkan: a new encode-session request flag; VideoToolbox: `kVTEncodeFrameOptionKey_ForceKeyFrame`),
counted where refusals already count. The decoders enter at `starts_group` and, on a message
whose `lost_since_last_read > 0`, discard until the next `starts_group`; `OpusDecoder` reads
channels and `pre_skip` from the upstream `AudioEntry.description` (OpusHead), falling back to
the packet's TOC for channels. `EncodedVideoFrame`, `EncodedAudioPacket`, their casts, tests,
key lists and `encoded_stream_ordering.rs` are deleted; `color` leaves the wire. The codec rig's
fixture arm writes the media frame. The WebRTC wheel is a consumer: its breakage is filed at
`packages/streamlib-webrtc`, never fixed in-change.
Rejected: a producer sequence inside the payload — a stock player stops on it (owner, 2026-10-16).
Rejected: codec-level gap detection — unreliable with hardware decoders (owner, 2026-10-16).

## REMOVED: `Mp4Sink` — §Media I/O `:3361-3530`, `:3047`

`M/mp4_sink.rs`, `mp4_fragmented_file_writer.rs`, `mp4_track_sample_entry.rs`, `M/lib.rs:35-37`,
`:75`, `:105`, `mp4-atom` (`M/Cargo.toml:28-30`, `:45`; `runtime/streamlib-engine/Cargo.toml:215-218`);
`X/mp4_inspect.rs`, `X/main.rs:29`, `:654-658`, `:751`, `xtask/Cargo.toml:41`; the rig's `mp4:` arm
and `RecordedMp4TrackReplaySource` (`E/../examples/codec_roundtrip_rig.rs:12`, `:50`, `:161-164`,
`:413-720`); `T/e2e_fixture_recording.sh`, `T/recording_stream.py`; `tests/stream-on-runtime/test_mp4_sink.py`,
`block_wiring_streams.py:14`, `:45-46`, `test_stream_graph_load_on_tatolabd.py:54`, `:78`; the
golden graphs `runtime/streamlib-media-builtins/tests/golden_graphs/000{1,2,3}-*.json:217`
rewritten; `S/_built_in_nodes.py:31-32`, `:383-434`, `S/__init__.py:38-39`, `:177-178`,
`test_built_in_node_classes.py:53-54`; `.claude/skills/verify-video/`; `README.md:33`, `:599`.
`h265_sequence_parameter_set.rs` and `nal_unit_raw_byte_sequence_payload.rs` stay (the H.265 decoder
reads them). Consumers (`examples/camera-audio-recorder`, `camera-webrtc-publish/README.md:22`,
`packages/streamlib-webrtc/python/tatolab/webrtc/processors.py:187`, `:232`): backlog filed.

## ADDED: the publisher, the adapter crates, the local relay — §Networking `:3880-3984`

- **The engine seam.** `RuntimeOperations` (one loaded stream's) gains
  `subscribe_to_an_exposed_output_port(node, port) -> ExposedOutputPortSubscription` — the reserved
  slot's reader (the tap's forwarder, shared; `RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL` →
  `RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL`) yielding `(payload_bytes, timestamp_ns,
  group_boundary, lost_since_last_read)` — and `Runner::exposure_changes() -> ExposureChangeReceiver`,
  which reads every loaded stream's exposed ports, each with its level and catalog entry, again on
  each notice on the event bus: a level set, an entry replaced, a stream compiled, a stream unloaded.
  A port's channel exists only while something reads it, so the subscription holds the port's
  source side open — native and helper sources alike — whether or not a link out of it exists. A
  channel the publisher is the first to open is made `WINDOWED_PORT_SUBSCRIBER_RING_DEPTH` deep, so
  a windowed reader a live graph change connects later fits it; the publisher's own reader takes
  `DeliveryProfile::ORDERED_DEPTH` (owner, 2026-10-18).
- **The publisher** is `D/exposed_port_publisher.rs`, one per `tatolabd`, holding one subscription per
  exposed port of every loaded stream and, per adapter instance whose levels include the port's,
  one `moq_net::broadcast::Producer` per stream named `<root>/<machine>/<stream>.hang`, one track
  `<node>/<port>` on a nanosecond timescale, each object written with
  `group.write_frame(Timestamp::from_nanos(timestamp_ns), payload)` (a stamp before zero reads as
  zero), a new group at `StartsGroup`. Under backpressure (the forwarder full) it abandons the
  rest of the current group and resumes at the next `StartsGroup`. The catalog is a
  `hang::catalog::Catalog` assembled from the ports' entries by section under their track names,
  re-published as `catalog.json` and `catalog.json.z` when an entry or the exposed set changes,
  and written again whole as a fresh group on the period a relay's adapter names (owner,
  2026-10-20);
  a port with no entry is a track the catalog does not list. Every exposed port is forwarded; no predicate chooses which.
- **The adapter SDK** `runtime/streamlib-relay-adapter-sdk`: `trait RelayAdapter { fn kind(); fn
  settings_schema(); fn join(&mut self, settings) -> Result<Joined>; fn levels_served(); fn
  broadcast_root(&self, machine) -> BroadcastPath; fn credential_for_dial(&self) -> …; fn events()
  }`, `RelayInstanceSettings` (a `schemars::JsonSchema` struct per adapter through the processor
  schema crate's blanket impl, re-exported as `RelaySettingsJsonSchema`), connection events and
  the credential handle. One crate each: `streamlib-relay-adapter-local`, `-cloudflare`, `-moqpro`,
  all linked into `tatolabd`; `<state>/relays.json` (0600, `[{relay_id, adapter, settings}]`, no
  secret) re-joined at every start; the two local instances `local-loopback` and `local-every-interface`
  present by default, their bind never a setting, `remove_relay` refused on them by name.
- **Relays are configured like built-ins (owner, 2026-10-16).** `GET /api/relay-adapters` and the
  MCP resource `streamlib://relay-adapter-catalog` list each adapter's kind, doc and settings
  schema, secrets marked `writeOnly`; tools and verbs `add_relay {adapter, settings}` →
  `{relay_id}` (`tatolab relay add <adapter> --settings-file`), `configure_relay {relay_id,
  settings}` (`tatolab relay set <id> key=value`; a secret only by `--secret <field> --from-file |
  --from-stdin`), `remove_relay`, `list_relays` (`tatolab relays`: id, adapter, settings with each
  secret shown set or unset, connection state). `CONTROL_TOOL_NAMES` and the help test gain the
  four and lose `tap` and `exchange`. A secret lives in the OS secret store through the `keyring`
  crate, one item per `{relay_id, field}` under `tatolab`, falling back to `<state>/relay-credentials.json`
  (0600) where no store answers, said once at start; held as a `secrecy::SecretString`, redacted in
  `list_relays`, `graph` and every log line — a URL carrying a token is redacted whole — and placed
  in no child's environment. An adapter holds whatever its provider's SDK needs; `configure_relay`
  rotates by replacing.
- **The local adapter** runs `moq-relay` 0.17.x, built from the pinned crate into `bin/moq-relay`
  (a new staging arm of `build_runtime.rs` beside the driver scripts, since it is no workspace
  member; the portability gate walks it), as two supervised children: loopback bound
  `127.0.0.1:4443` and every-interface `[::]:4445`, each with `[web.http]` on its own address
  (fingerprint, WebSocket, `/fetch`). Admission: each child's `--auth-url` is a unix socket in the
  runtime directory that `tatolabd` answers in-process with the `moq-auth` request shape —
  publish and subscribe for the token `tatolabd` minted for itself, subscribe alone for anyone
  else, on that listener's root. `tatolabd` writes each child's certificate and key as files
  (ECDSA, 14 days, regenerated at 10), which the relay hot-reloads; the children get
  `PR_SET_PDEATHSIG` on Linux, a pidfile reaped at start on both floors, restart with backoff,
  and end with the runtime. `machine_name.rs` keeps the hostname cast and loses the Tailscale read.
  `moq-net 0.3.x`, `moq-tokio 0.19.x`, `hang 0.21.x`, `moq-auth 0.2.x`, `moq-relay 0.17.x` pinned exact.
- **Hosted adapters.** `-cloudflare`: IETF drafts 14/16 through `moq-net`, a per-relay token in the
  settings (it rides the URL path, so the whole URL is a secret), no root — the adapter names
  `<machine>/<stream>.hang` under the relay, its catalogs written again whole every second
  since a drafts subscriber receives only objects written after it subscribed (owner,
  2026-10-20); `-moqpro`: the project's signing key in the store, JWTs minted with `root`,
  `publish`, `exp`. Both ship; the live proof is rig-only, cannot-run without credentials.
Rejected: a hand-edited settings file as the only way to join; the app as the only way; a
plaintext credentials file where a store answers (owner, 2026-10-16); a secret as a CLI argument
(owner, 2026-10-17, from the 2026-10-16 review).

## ADDED: the relay source node — §Networking `:3779`

`RelayTrackSource` (`M/relay_track_source.rs`), `execution = manual`, config `relay_url`
(default `http://127.0.0.1:4443`), `broadcast` and `track` — MoQ's own names, both required and
each read exactly as written; a stream's exposed port is the track `<node>/<port>` of the broadcast
`<machine>/<stream>.hang` (owner, 2026-10-18), one output `track`. A processor-owned thread subscribes with `moq-net`, sets `track`'s entry to the
track's own from the broadcast's catalog, and writes each object with `write_bytes_with_timestamp`:
the stamp is the object's timestamp (the hang varint for media, the transport timestamp otherwise)
plus one offset fixed at the first object, re-anchored when the track jumps; no timestamp means
arrival; `StartsGroup` on a group's first object. A relay refusing or vanishing is retried with
backoff; `stop()` closes the session inside the 5 s budget.

## REMOVED: `remote_output` and the cross-stream link — §Networking `:3700-3744`

The builder's `remote_output`, `NodeOutputPortReferenceByAddress`, `_link_source_end`'s machine
and stream keys; `LinkPortRefOutput.{machine,stream}`, `OutputPortInAnotherStreamOnThisMachine`,
`LinkState::AwaitingRemote`, `reconcile_a_link_reading_another_stream_op.rs`,
`wire_a_link_reading_another_stream`, `connect_from_an_output_port_in_another_stream`, `connect`'s
`<end>_stream`; a link end naming a stream or machine is refused at load naming `RelayTrackSource`.
`register_a_reader_of_an_exposed_output_port` and its refusals go; the two cross-stream suites are
deleted, their internal-port and lowering cases rewritten against the publisher (private at
loopback, public at both; lowering withdraws the track and ends the subscriber).

## ADDED: loopback HTTP and the preview page — §Networking `:3841`, `:4006`

`A/loopback_http_listener.rs`: TCP on `127.0.0.1:4446`, started with the local API: `GET /` (JSON:
machine, streams, each exposed port with level and entry) and `GET /watch/<machine>/<stream>[/<node>/<port>]`
serving the preview page, static. The page is `runtime/tatolab-watch/` — `package.json` pinning
`@moq/watch` 0.6.x, `index.html`, `main.ts` constructing the Player on `http://127.0.0.1:4443` with
`target.name` from the URL — built by `bun build` in `build-runtime` and the release workflows (a
`setup-bun` step, pinned) into `lib/tatolab/watch/`; its licence closure enters the third-party notices.

## REMOVED: `tap`, `exchange`, and every verification door — §Networking `:3856`, §Control plane

`C/main.rs:339`, `:354`, `C/surface_image_exchange.rs`, `TAP_TOOL_NAME` (`C/runtime_observation_verbs.rs:16`);
`A/handlers.rs:55`, `:111`, `A/mcp.rs:602-712`, `A/mcp_prompts.rs:686-707`; the PNG conversion
(`operations_on_the_streams_loaded_in_this_runtime.rs:42-279`, `E/core/runtime/surface_image_exchange.rs`,
`png` at `runtime/streamlib-engine/Cargo.toml:127`); CLI tests `tatolab_{exchange_verb,observation_verbs,mcp_verb,machine_runtime_socket}.rs`,
`common/tapped_channel_bag_fixtures.rs`; the wheel's `decode_tapped_channel_bag_frame_to_python_object`;
`tests/stream-on-runtime/test_cli_launch.py:124-153`, `:448-469`, `test_cli_mcp_verb_live.py:42-44`.
Nothing replaces them in the runtime. The rigs become tools: `tatolab-read-track <relay-url>
<broadcast> <track> --count N --out <dir>`, a `moq-net` subscriber writing each object's payload and
timestamp, its own never-shipped crate so no xtask gate compiles the QUIC stack (owner, 2026-10-19). The
codec round trip is two streams, as a deployment builds it (owner, 2026-10-19): `T/codec_roundtrip_stream.py`'s
sender exposes the encoder's port, its receiver reads that track through `RelayTrackSource` and decodes it;
`T/e2e_fixture_psnr_vivid.sh` reads the sender's track, decodes the dump with the rig's ffmpeg into PNGs
for `X/psnr.rs`, the baselines re-taken on that path, and reads the receiver's decoded-frame count off
`graph`. `E/../examples/codec_roundtrip_rig.rs`, an app built on an in-process engine, and
`T/e2e_fixture_psnr.sh` with its PNG-replay arm are deleted; vivid is the codec rig's source until
`TestPatternSource` renders deterministically per frame index (owner, 2026-10-19). From then the rig is
`T/e2e_fixture_psnr_test_pattern.sh`, its sender encodes `TestPatternSource`, and `cargo xtask psnr
bar-colors` locks each SMPTE bar's median colour per codec, replacing `channel-means` (owner, 2026-10-19). `T/verify_audio_channel.sh`
and `T/audio_track_dump_check.py` read the `AudioBlock` track and decode msgpack in the script.
`verify-live` (`:25-69`, `:103-135`), `verify-audio`, `capture-stream-evidence`, `tap-live-channel`,
`drive-running-stream`, `inspect-live-graph`, `list-running-streams`, `evidence-verifier.md:21-28`,
`README.md:283-408` and the nine `examples/*/README.md` move onto `read-track` and `/watch` in their own `.claude/` PR.

## REMOVED: windows — §Media I/O `:2483`

Every site the 2026-10-16 sweep listed, plus `VK_KHR_swapchain` (`E/vulkan/vulkan_device.rs:1092-1097`),
`window_system_surface_extensions` (`vulkan_instance.rs:59-209`), `get_swapchain_images`
(`video/codec_utils/helpers.rs:371`), `codec_roundtrip_rig.rs:89`, `:998`, `DISPLAY=` in
`T/e2e_fixture_psnr_vivid.sh:278`, `ci.yml:638`, `sdk/streamlib-sdk/src/lib.rs:101-104`.
`VulkanPresentCompositor` stays as the virtual camera's, renamed `VulkanOffscreenCompositor`, with
`display_blit.*` and `MAX_FRAMES_IN_FLIGHT` (`vulkan_present_compositor.rs:107-142`, `:28`) kept under
offscreen names; the present target and `create_present_target` go. The eight examples and
`packages/streamlib-webrtc/tests/live/whip_whep_roundtrip_stream.py:110-125` are consumers: filed.

## MODIFIED: the sample stream — §Product `:30-50`; §Media I/O `:2493`, `:2500`

`TestPatternSource` renders on the GPU: one compute kernel over `create_compute_kernel` writing
the pooled `Rgba32` frame per tick — SMPTE bars, the potato sprite bouncing corner to corner,
"Tatolab stream setup successfully" and "sample stream" bottom-right — glyph atlas and sprite
baked as `include_bytes!` RGBA; the owner supplies the sprite at implement time unless an issue
comment already does. `scaffold_template/stream.py` becomes `TestPatternSource → HalftoneLensEffect
(a drifting lens that turns what it covers into dots, replacing `InvertingEffect`; owner, 2026-10-19)
→ H264Encoder → expose(encoder.output("encoded_video"), Exposure.PUBLIC)` with the `BrightnessMeter`
fan-out; `--test-pattern` and `ScaffoldedStreamSource` go; the floor becomes the release that
ships this. The macOS gate and `reusable-distributions.yml:158` run `tatolab new` and `tatolab run`
and assert the stream loads and `TestPatternSource.video` counts ≥ 60 frames in `graph`; on a
runner with no encoder `H264Encoder` refuses naming itself and the stream still loads. The CPU
arm (`:2500`) is deferred past this change: no `openh264` enters.

## Tree consequences, by slice

1. **The reader** — `lost_since_last_read`, `read_message`. Proof: the drop test above.
2. **Catalog entries** — struct, both declarations, the op, the lookup, `graph`/registry, the generated
   classes. Proof: hang parses every built-in's entry; a bad one is refused naming the port.
3. **The media frame** — encoders, decoders, the SPS parser, force-keyframe, Opus, the deletions,
   `Mp4Sink`, the rig's fixture arm. Proof: the PSNR rigs lock on the relay path.
4. **Publisher, SDK, adapters, local relay, settings**; `tap`, `exchange`, the Tailscale read deleted.
   Proof: a `moq-net` client reads a public track at both listeners and a private one at loopback only,
   byte-identical to iceoryx2; an anonymous publish is refused; lowering a level ends the subscription;
   a secret never appears in `list_relays`, `graph`, a log or a child's environment.
5. **`RelayTrackSource`**; `remote_output` deleted. Proof: stream B decodes stream A's private port
   through loopback; two runtimes read a public port; frame spacing survives jitter.
6. **Loopback HTTP, the web project, `read-track`**; rigs and skills rewritten. Proof: the page plays in
   Chromium and Safari on the rig; `read-track` dumps what the publisher sent.
7. **Windows deleted.** Proof: both floors build without `winit`; the virtual camera still composites.
8. **The sample stream.** Proof: the gate above; on the rig the browser plays it.
9. **Hosted adapters.** Proof: rig-only, cannot-run without credentials.
Order: 1, 2, 7 first; 3 after 1 and 2; 4 after 2; 5 and 6 after 4; 8 after 3 and 6; 9 after 4.

## Assumptions stated, not asked

- `lost_since_last_read` is the inbound link's dropped-bag delta; refused writes stay the producer's count.
- The Python entry helpers are dataclasses in `tatolab.stream.catalog`, one per hang section.
- `tatolabd` answers `--auth-url` in-process over a unix socket, no `moq auth serve` child; its own
  publish token is minted at start, never written to disk. Ports 4443, 4445, 4446 are settings defaults.
- The rig's PSNR decoder is ffmpeg, already on the rig, never shipped.
- The web project is built with bun, pinned in the workflows, staged like the lend; the scaffold
  exposes the encoder's `encoded_video`.

## REMOVED

- REMOVED: remote_output
- REMOVED: AwaitingRemote
- REMOVED: NodeOutputPortReferenceByAddress
- REMOVED: reconcile_a_link_reading_another_stream_op.rs
- REMOVED: RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL
- REMOVED: TAP_TOOL_NAME
- REMOVED: SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE
- REMOVED: surface_image_exchange.rs
- REMOVED: decode_tapped_channel_bag_frame_to_python_object
- REMOVED: PortAddress
- REMOVED: port_address.rs
- REMOVED: InvalidPortAddress
- REMOVED: PortAddressNamesAnotherMachine
- REMOVED: EncodedVideoFrame
- REMOVED: EncodedAudioPacket
- REMOVED: encoded_stream_ordering.rs
- REMOVED: mp4_sink.rs
- REMOVED: mp4_fragmented_file_writer.rs
- REMOVED: mp4_inspect.rs
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
- REMOVED: CatalogFragment
- REMOVED: e2e_fixture_psnr_vivid.sh
- REMOVED: psnr_vivid_baseline
- REMOVED: channel-means
- REMOVED: RgbChannelMeans
