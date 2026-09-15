# cross-runtime-links

A link may join ports in two runtimes, and the engine carries it. After this change:
- any runtime may wire another runtime's output into an input — its own, or a third runtime's;
- the runtime that owns the input applies the link with `connect`'s own refusals and says who asked;
- bags cross with their stamps unchanged, a top-level `surface_id` lands as a local surface, and
  every bag lost on the hop is counted on the link;
- nothing is sent for a port until a remote link reads it.

The change implements these `[runtime-mesh]` entries in `docs/plan/ARCHITECTURE.md`: §Networking
`:2432-2437` (surfaces), `:2441-2447` (who may wire), `:2448-2453` (waiting and refusal),
`:2454-2457` (stamps) and `:2458-2462` (loss), with §Processor model's remote carve-out `:440-453`
and remote link name `:468-491`. It settles the three items `runtime-mesh.md:292-298` left here.
Owner align 2026-09-14 (PR #2258). Rationale: `docs/decisions/runtime-mesh.md`, amended.

**Scale gate: this skill, plus an ADR.** Python API: `Runtime` gains remote port references and
`connect` takes them. A new wire: link requests, reader tokens and the data message with its
attachment. Processor model: link states and the inbound link name a helper receives. `graph`
changes a link's shape; MCP `connect` and `disconnect` gain arguments.

**Precondition.** Every entry above is DECIDED. Untouched: the common-clock OPEN `:2466`, the auth
OPEN `:2708`, and the zero-copy OPEN `:242`. Built on `runtime-mesh` (#2282-#2285),
`local-transport-hardening` (#2261-#2266), `loss-visibility` (#2268-#2270), and #2272/#2273.

**Verified against the tree 2026-09-14 (HEAD d4ce808f6).** Four read-only sweeps covered link
wiring, surfaces, stamps with test reach, and Zenoh 1.10.1 (tag `1211779`) with loopback probes.
Paths are under `runtime/streamlib-engine/src/` unless rooted.

**Link wiring**
- Every link end is `{processor_id, port_name}`: `OutputLinkPortRef`/`InputLinkPortRef`
  (`core/graph/edges/output_link_port_ref.rs:11-36`), Python references (`python_added_processor.rs:67-97`),
  MCP `connect` (`streamlib-api-server/src/mcp.rs:352-366`), `LinkPortRefOutput` (`core/json_schema.rs:143-149`).
- The graph is `DiGraph<ProcessorNode, Link>` (`add_e_op.rs:14-34`). Sizing and profile walk the
  source node's out-edges (`open_iceoryx2_service_op.rs:364-382`, `:537-579`) and fall back to
  `Newest` with none (`:578`); fan-in counts in-edges (`:473-482`).
- `connect` checks endpoint existence and port-name grammar (`operations_runtime.rs:255-316`); every
  other refusal runs at wire time, which needs `start()` and a GPU (`runtime.rs:353`, `:506`).
- No engine publisher exists outside a processor (`open_iceoryx2_service_op.rs:774`;
  `python_processor_link_data_access.rs:294`). `write_raw` frames any port key (`iceoryx2/output.rs:281-361`).
- A tap subscribes with no graph node; its "reserved" slot is only a count (`core/runtime/tap.rs:22-26`, `iceoryx2/node.rs:180-219`).
- A helper derives its inbound link name from `channel_service_name` (`python_processor_link_data_access.rs:401`);
  a Rust destination takes it as its own argument (`open_iceoryx2_service_op.rs:211`).
- Python `connect` refuses after `run()` and discards the link id (`python_runtime_lifecycle.rs:141-156`, `:326-342`).
- No lookup by display name exists (`add_v_op.rs:96-118`).

**Surfaces**
- A video bag names no pixel format (`streamlib-media-builtins/src/video_frame.rs:23-54`); the format lives on the backing.
- `exchange` always emits RGBA8, ignores `color_info`, refuses BGRA and float, and builds resources
  per call (`core/runtime/surface_pixel_exchange.rs:52`, `:277-312`, `:333-344`) — not an egress read.
- Pooled pixel buffers are host-mapped write-combined memory (`vulkan/rhi/vulkan_buffer.rs:163-181`); a
  1080p RGBA memcpy out of that type cost 37 ms (`docs/decisions/virtual-camera-sink.md:76-78`).
- `SurfaceExportStaging` reads a texture backing's native bytes into host-cached staging and refuses
  multi-plane formats (`core/context/surface_export_staging.rs:135-150`, `:241-262`, `:515-519`).
- The mint is `GpuContext::acquire_pixel_buffer` plus a memcpy, callable from app-process engine code
  (`gpu_context.rs:1086-1102`; `pooled_rgba_frame_staging.rs:23-52`). At cap it returns a `Configuration`
  string (`gpu_context.rs:650-652`); pools are never freed (`:146-147`); NV12 slots hold luma only (`:405`).

**Stamps**
- The 76-byte frame header carries one `timestamp_ns` and no clock identity (`streamlib-ipc-types/src/lib.rs:389-424`).
- Bags carry `VideoFrame.timestamp_ns` and `AudioBlock.first_sample_timestamp_ns`; encoded bags carry only the header's.
- Cross-link comparisons a remote link would make cross-clock: `Mp4Sink`'s epoch and fragment span
  (`mp4_fragmented_file_writer.rs:731-735`, `:519-521`, `:570-571`); the MoQ deadline and backlog age against
  local now (`packages/streamlib-moq/src/delivery_deadline.rs:127`, `:156`).
- Built-in relays restate an upstream stamp on a local output (`h264_decoder.rs:79-82`, `opus_encoder.rs:59-68`),
  so a stamp's clock cannot be derived from the link it arrives on.
- Nothing in the tree reads a boot id.

**Zenoh 1.10.1**
- `default-features = false` drops `transport_multilink`, so two peers share **one** link (`transport.rs:285-298`).
  Listening on TCP and UDP picks either at random: 3 TCP and 2 UDP in five probe pairs.
- A plain `udp/` link carries declarations, tokens and queries without retransmission (`common/seq_num.rs:145-158`).
  Per-message reliability is `unstable` (`api/builders/publisher.rs:162`).
- `udp/…?rel=1` is QUIC with payload encryption disabled and a self-signed key
  (`zenoh-link-commons/src/quic/plaintext.rs:39-47`, `quic/unicast.rs:375-380`), one stream per priority.
- Stable: attachments, `congestion_control(Drop)`, `express`, `priority`, liveliness with history, `get`/queryable with `reply_err`.
- `put().wait()` with `Drop` blocks its caller while a fragmented message queues, up to ~51 ms
  (`common/pipeline.rs:188-211`), and holds that priority's queue: 4 KB puts waited 12.9 ms behind 33 MB ones
  on one priority and 40 µs on another. A drop reports nothing (`api/session.rs:2504-2640`).
- A subscriber callback runs on the link's receive loop (`unicast/universal/link.rs:517-528`). `*`/`**` never
  match a chunk beginning `@` (`key_expr/borrowed.rs:354-382`).
- Loopback, 30 Hz, 30 puts: plain UDP delivered 24/30 at 3.1 MB and 7/30 at 33 MB; TCP and QUIC 30/30.

**Test reach.** No test boots two runtimes. `ProcessorLinkDataAccess` opens real channels with no `Runner`
(`packages/streamlib-moq/tests/test_data_track_round_trip.py:233-262`); no engine test re-executes itself.

---

## DECIDED (owner, 2026-09-14) — the mesh rides QUIC over UDP

The session `runtime-mesh` ticketed as #2283 listened on TCP and plain UDP (`runtime-mesh.md:146`). With
`transport_multilink` off two peers keep one link, chosen at random, and plain UDP carries link requests
and discovery tokens with no retransmission. So a runtime listens on `udp/[::]:0?rel=1` only: QUIC over
UDP, one stream per priority, so a raw frame never delays audio or a link request; congestion-controlled;
unencrypted and self-signed, so remote links, like the rest of the mesh, belong on isolated or trusted
networks until the security pass (`:2438-2440`). A loss inside one stream costs a
retransmit, bounded by the sender's drop. `mesh_listen_endpoints` and `mesh_peer_endpoints` still take
`tcp/` for a network that blocks UDP, and then everything rides that one TCP link; a plain best-effort
`udp/` endpoint is refused by name, naming `?rel=1`, since it would carry the control traffic unreliably.
Rejected: TCP for control plus QUIC for media — `transport_multilink`, `max_links = 2` and priority
ranges per endpoint, two sockets per peer pair for little over one QUIC link; and TCP plus plain UDP,
which needs Zenoh's `unstable` feature, lost most raw frames even on loopback, and needs a socket-buffer
sysctl Zenoh cannot set. TCP alone was never offered. `runtime-mesh.md:120-122`, `:146-147`, `:269-271`,
`:315` and #2283 are updated in this PR.

## ADDED: §Networking — how a remote link is spelled

- **Python.** `Runtime.remote_processor_output(runtime_name, display_name, port_name)` and
  `Runtime.remote_processor_input(...)` return `RemoteProcessorOutputPortReference` and
  `RemoteProcessorInputPortReference`, stub-gated. `connect(source, destination)` takes a local or
  remote reference on each end, before `run()` as today:
  `rt.connect(rt.remote_processor_output("bench-cam-a1b2", "CameraSource", "video"), display.input("video"))`.
- **Rust.** `OutputLinkPortRef` and `InputLinkPortRef` each gain a variant carrying a
  `MeshPortAddress { runtime_name, display_name, port_name }` — the existing types extended, never a
  parallel pair. `Runner::connect` applies a link on this runtime, its source local or remote;
  `Runner::request_link_on_remote_input_runtime` asks another runtime to apply one.
- **MCP.** `connect` takes `from_runtime_name` with `from_processor_display_name` in place of
  `from_processor_id`, and `to_runtime_name` with `to_processor_display_name` in place of `to_processor_id`,
  one pair per end. It returns `{link_request_id?, link_id?, input_runtime_name, state}`. `disconnect` takes
  `link_id` with an optional `input_runtime_name`, or a `link_request_id` to cancel a request still waiting.
- A runtime name equal to one's own is a local reference, resolved by display name.

## MODIFIED: §Networking `:2441-2447` — how a link request is read

1. **The input's runtime always pulls.** A push and a third-party wiring are one message: a request to
   the input's runtime, which applies `connect` with a remote source. One data shape serves all three.
2. **A request is a Zenoh query** to a queryable under the input runtime's `@runtime/<runtime name>`
   prefix (`runtime-mesh.md:163-178`), sent at `Drop` on the control priority with an engine-chosen
   timeout. The payload is msgpack `{operation, link_request_id, source_address, destination_address or
   link_id, requester_runtime_name, engine_version}`; the requester mints the id. The reply is
   `{link_id, state}` or a refusal by `reply_err`, and only `reply_err` refuses.
3. **Silence is not a refusal.** `Drop` loses a query or its reply without a word, so a timeout leaves the
   request waiting with reason `unanswered` and resends it on an engine-chosen backoff. The input's
   runtime keeps each applied `link_request_id` beside its link for the link's life, so a resend returns
   the link it already made, never a second one.
4. **`connect` never waits on the mesh**, the owner's helper ruling applied again: it returns
   `awaiting_remote` or `pending`, and the outcome lands in `graph`.
5. **An absent or silent input runtime.** The requester keeps the request, renders it with its id in
   `graph.mesh.link_requests_awaiting_runtime`, and sends it when that runtime appears. A request dies with
   its requester; `disconnect` naming its `link_request_id` cancels it.
6. **`disconnect` over the mesh** is the same request with `link_id`. Any runtime may send it until the security pass.
7. **Every link renders `created_by_runtime_name`**, its own runtime's name for a local link.

## MODIFIED: §Networking `:2448-2453` — how waiting, refusal and laziness are read

1. **States.** `LinkState` gains `awaiting_remote`, rendered with a reason naming the runtime or the port.
   The rest are `local-transport-hardening`'s `pending`, `wired` and `error` with reason.
2. **The source runtime appears.** The input's runtime queries its offered output ports, answered at query
   time, never announced. A missing port is `error` listing what is offered; so is a different
   `engine_version`, naming both (pre-1.0: no cross-version wire). The runtime then declares a reader
   liveliness token under `@runtime/<source name>/@readers/<display name>/<port>/<own name>`, subscribes to
   `streamlib/<mesh name>/<source name>/<display name>/<port>`, and reads `wired` when that subscriber and
   the local destination are (M3 for a helper).
3. **Leaving.** The source's token going returns the link to `awaiting_remote`; so does its egress token
   (below) going while the runtime stays, naming the port. A return re-wires and restarts the count.
4. **A name two live runtimes hold** (`runtime-mesh.md:220-222`) makes the link `error`, naming both hosts,
   carrying from neither until one leaves: a link that picked one could feed the wrong machine.
5. **Ingress.** One per remote source address on a runtime, shared by every link from it. Its callback
   only hands off into a ring of `ORDERED_DEPTH` evicting the oldest; its own thread writes into a local
   channel as that channel's single publisher through `write_raw`, with the carried stamp. The channel is
   engine-named (`channel_name.rs` grammar, hashed from the address), and sizing, profile and caps read
   the local destinations, never a missing source node.
6. **Egress.** A source runtime watches `@runtime/<own name>/@readers/**`. The first reader of an
   existing output port creates one egress, which takes one ordinary destination slot on that
   channel; the last reader's leave removes it. It declares `@runtime/<own name>/@egress/<display>/<port>`,
   drains FIFO on its own OS thread and puts at `CongestionControl::Drop` on one priority for its life:
   `Priority::DataLow` when its first bag carries a top-level `surface_id`, `Priority::Data` otherwise.
   Two priorities are two QUIC streams, which would reorder one port's sequence and read as gaps.
   Requests and tokens ride above both. With no reader it holds no subscriber, publisher or token.
7. **Tap** resolves a remote link's local channel by the link's mesh address.

## MODIFIED: §Networking `:2432-2437` — how a surface crosses

1. **Egress** decodes only the top-level map far enough to find `surface_id`; a bag without it is sent
   verbatim. With it, egress resolves the id, claims it, copies the native bytes out, releases the claim,
   then sends. The claim spans the copy alone, so a slow network never pins the producer's slot.
2. **The copy door is `SurfaceExportStaging`**, which gains a pooled-pixel-buffer source: a GPU copy into
   host-cached staging, never a memcpy out of write-combined memory, and a `GpuContext` method (RHI rule).
3. **The message** is `[pixel description][bag][pixel bytes]`. The description gives the backing's format
   by wire name, width, height and byte length — all read from the backing, not the bag.
4. **Ingress** acquires a pooled pixel buffer of that format and extent on the app `GpuContext`, copies in,
   replaces the top-level `surface_id` with the new `<slot>#<generation>`, and writes the bag.
5. **Refused, and counted as drops.** A retired or unresolvable id, and a multi-plane format, which
   staging cannot read (NV12), each warn once per link naming the reason. A pool at cap on ingress counts too.
6. **Where it bites.**
   - A texture-backed frame (kernel output) lands buffer-backed, inheriting the camera's gap: a
     bare-id kernel dispatch refuses it, and the display's buffer fallback draws only RGBA correctly
     (`gpu_context.rs:1461-1527`). No generation-stamped texture mint exists (`texture_ring.rs:244-253`).
   - An sRGB texture label collapses to its linear buffer format (`surface_export_staging.rs:131`).
   - `texture_layout` crosses verbatim, since the engine reads no other key. It is inert: a pooled id never
     takes the import path that reads it (`gpu_context.rs:1366-1379`).
   - Pools are never freed (`gpu_context.rs:146-147`), so an ingress admits an engine-chosen handful of
     distinct format and extent pairs per remote link, and refuses and counts a bag beyond them by name.
   - Bandwidth: 1080p RGBA (8.3 MB) must queue inside ~51 ms, roughly 1.3 Gbit/s, so raw frames over
     1 GbE mostly drop and are counted. Encoded bags are the path for ordinary links.

## MODIFIED: §Networking `:2454-2457` — how a stamp's clock is carried

A **clock identity** is the kernel boot id on Linux (`/proc/sys/kernel/random/boot_id`) and
`kern.bootsessionuuid` on macOS, so two runtimes on one machine, or a container and its host, share
one. It rides each mesh message's attachment. The frame header does not change, and the docstrings that
call a stamp comparable "across every process" say "on one machine".

**DECIDED (owner, 2026-09-14) — the inbound link carries the clock identity; the relay gap is recorded
as known.** A remote link carries its peer's identity: `graph` renders it on the link, and the
link-naming read surface gains `inbound_link_stamp_clock_identity(port, link)` in Rust and Python. A peer
returning with a new identity re-wires as a new wiring. `Mp4Sink` compares first stamps across tracks
(`mp4_fragmented_file_writer.rs:731-735`), so it stops a track whose link's clock differs from the
recording's first track, or changes mid-recording, by name — its existing per-track latch
(`:1928-1938`). **Known gap:** a relay — `h264_decoder.rs:79-82`, `opus_encoder.rs:59-68`, any Python
`write(..., timestamp_ns=)` — restates an upstream stamp on a local link, where it reads as local, so
mixing machines downstream of a relay goes uncaught until the common-clock OPEN `:2466` closes.
Rejected: the identity on every bag, in the user header beside `loss-visibility`'s sequence number,
which covers relays but changes every timestamped read and write signature in both languages.
**Zenoh's own timestamps were rechecked** at the owner's request: they are a hybrid logical clock —
`SystemTime::now()` wall time plus a counter and the session id (`uhlc-0.8.2/src/lib.rs:330-338`,
`zenoh/src/net/runtime/mod.rs:286`) — off for peers by default (`DEFAULT_CONFIG.json5:215`), refusing a
stamp too far ahead and adjusting no clock. They order events between hosts that already share NTP time
and cannot map a remote monotonic stamp onto ours; the OPEN's PTP/NTP direction stands.

## MODIFIED: §Networking `:2458-2462` — how hop loss is read

1. **The number is `loss-visibility`'s**, carried end to end. Egress copies each sample's user-header
   sequence number into a fixed little-endian attachment `{sequence_number u64, publisher_generation u64,
   clock_identity [u8; 16]}`, layout-tested. It bumps the generation when the sample's `origin()` changes.
2. **Ingress counts the gap** after its ring. A gap covers the egress ring's overwrites, refused copies,
   Zenoh's silent drops, the network and the ingress ring — everything between the producer's send and
   the local write. A new generation is a baseline, never a gap.
3. **It counts on both profiles.** Nothing on the hop skips by design, so a `newest` destination's
   hop loss is loss; its own port passing over bags stays uncounted.
4. **Rendering.** `metrics.mesh_hop_dropped_bags_by_link: {link_id: n}` on the destination node, beside
   `dropped_bags_by_link`, only for remote links. The ingress runs in the app process, so a helper
   destination's hop count needs no blackboard.

## MODIFIED: §Processor model `:468-491`, §Control plane `:2556-2596` — names and `graph`

- A remote link's inbound link name is `<runtime name>/<display name>/<port>`. A helper's wiring envelope
  gains `inbound_link_name` beside `channel_service_name`, and `wire_input_link` and its stub take it (a
  helper-protocol change, made safe by M4's build id).
- A link's `source` or `target` renders `{runtime_name, processor_display_name, port_name}` for a remote
  end. `LinkPortRefOutput` becomes one of two shapes; the schema, `mcp_prompts.rs:172`'s fixture and
  `generate_schemas.rs` follow. `graph.mesh` gains `egress_ports: [{processor_display_name, port_name,
  reader_runtime_names}]`.

## Assumptions stated, not asked

- **CI proves components, the rig proves runtimes.** CI cannot start a runtime. Egress and ingress are
  engine components a two-process test drives over real channels and a loopback session with no
  `Runner`. Two `streamlib run` apps run on the rig. A GPU-free `start()` would be a plan change.
- **Python gains no `disconnect` and no post-`run()` connect.** MCP is the dynamic door, as today.
- **Consumer backlog, filed when X1 merges:** `packages/streamlib-moq`'s deadline measures a remote stamp
  against local now (`delivery_deadline.rs:127`, `:156`).
- **Found, not fixed here:** a duplicate local link is never refused (a resent mesh request is covered by its
  id); NV12 pool slots are undersized (`gpu_context.rs:405`). They go in the PR body. `disconnect_impl`
  naming the target's port as the source port (`operations_runtime.rs:358`) is fixed in X2, which builds on it.

## Expected slices

`/derive-tickets` decides the breakdown. The shape the recon supports:

| # | Slice | Blocked by | Proof |
|---|---|---|---|
| X1 | Pull: `MeshPortAddress`, Python and MCP remote source, offered-port query, reader and egress tokens, ingress, egress without surfaces, states and reasons, version and duplicate-name errors, attachment, hop count, inbound link name incl. helper envelope, `graph` link shape, tap by address | #2283, #2263, #2265, #2268, #2272, #2273 | CI two-process components over loopback: bags byte-equal and stamps equal; dropping every k-th put counts exactly k's; SIGKILL of the source returns `awaiting_remote` and a restart re-wires from zero; no key without a reader; attachment golden bytes. Rig: two `streamlib run` apps, the known audio signal across, `tap_audio_channel.py --expect-frame-not-restamped` |
| X2 | Requests: push and third party, `link_request_id`, resend and idempotent apply, cancel, `link_requests_awaiting_runtime`, disconnect over the mesh and `disconnect_impl`'s source port, `created_by_runtime_name`, MCP `to_*` | X1 | CI: a request's reply and refusal by name; a dropped reply resent returns the one link; a silent input runtime stays waiting; an absent one's request sends on appearance and a cancelled one never does; the disconnect events name both ports on an asymmetric link. Rig: a third app wires the other two over MCP |
| X3 | Surfaces: staging's pooled source, egress copy, ingress mint, refusals counted | X1 | Rig: an RGBA source across, both ends exchanged, byte-exact, the ids differing; an NV12 and a retired id counted |
| X4 | Clock identity on the link, its read and `graph` rendering, `Mp4Sink`'s refusal | X1 | CI: boot id read on both platforms, cross-compiled; a track from another clock stops by name while the rest record |

Every new engine test is named in `.github/workflows/test.yml`'s slice and the `run_local_ci_gates`
mirror (`xtask/src/main.rs:204`), or it runs nowhere, and a two-process test must be reachable from that slice.

## Records the implementation owes, in the same PRs

- `_engine.pyi:745-751`'s link-name docstring; `mcp.rs:259`'s `wired` instruction gains `awaiting_remote`.
- `clock.py:6-11`, `video_frame.py:211-213`, `audio_block.py:66`, `video_frame.rs:30-32`, `audio_block.rs:50-51`.
- At `/ship-change`: the glossary's **Monotonic clock** becomes one per machine, and it gains **Clock identity**;
  `diagrams/system.mmd:53-55`.

## REMOVED

Clock records (X4):
- REMOVED: comparable across every process on the
- REMOVED: to a reading taken in any other process on the host
- REMOVED: share the kernel's monotonic epoch and are directly comparable
