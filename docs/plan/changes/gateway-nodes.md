# gateway-nodes

> **Asked for by the owner, 2026-11-06:** "I've decided to remove all the automatic stuff for
> relays … the intent is to move to a node based approach (identical to how unreal engine works
> for webrtc) meaning that relays are part of modules and there's nothing automatically done for
> moq instead we can connect content to a relay track, gateways are configured as a node on their
> own and we store the connection, and we can create a track and set that as a variable then
> elsewhere in the graph we can send out to a gateway tracks via the connection and such and
> include the catalog info … relays as automatic go away and for previews we should be able to
> visualize directly from the runtime with viewers based on all our new types image, array, etc
> directly (expose logic goes away as well since we move to explicit nodes)."

A pivot. The plan moved first (§Networking and the entries that named exposure, the publisher,
the local relay or the preview page). This file is the rip-out and the build that follow it.
Rationale: `docs/decisions/gateway-nodes.md`.

**The direction, in five sentences:**
1. Nothing leaves a stream on its own: the exposed-port publisher, exposure, the routing policy,
   the supervised local relay and the preview page are removed, and `expose` with them.
2. Networking is a module: a gateway node holds one connection to a relay, its URL and
   credential set in the node's own config.
3. A track node declares one outgoing track on a gateway, with its catalog entry, and the stream
   holds it as a graph variable; track send nodes elsewhere in the graph write to it by reference,
   and a track receive node reads a remote track through a gateway.
4. Previews come straight from the runtime: Tato Studio's viewers show any port of any node on the
   machine by its family, with no relay in between.
5. One stream reads another only through gateway nodes; inside a stream, nodes share memory as
   today.

**What a stream author writes after this change** (spelling assumes the open recommendations below):

```python
from tatolab.moq import MoqGateway, MoqTrack, MoqTrackSend, MoqTrackReceive

cloudflare = stream_builder.add(MoqGateway, kind="cloudflare", relay_url=CLOUDFLARE_URL, broadcast="desk")
camera_track = stream_builder.add(MoqTrack, gateway=cloudflare, track="camera")
send = stream_builder.add(MoqTrackSend, track=camera_track)
stream_builder.connect(encoder.encoded_video, send.content)

remote = stream_builder.add(MoqTrackReceive, gateway=cloudflare,
                            broadcast="studio", track="program", family=Packets, codec="avc1")
stream_builder.connect(remote.content, decoder.encoded_video)
```

**Scale gate — this skill plus an ADR.** It changes the Python API's public contract (`expose`
and `Exposure` go), the graph's spec keys (`exposed` goes), the local API (`expose_port`, `/watch`)
and the runtime unit's contents (`moq-relay`, the preview page).

## Current state (recon, 2026-11-06, `main` `7fcdf11`)

- **The publisher:** `runtime/tatolabd/src/exposed_port_publisher.rs` and
  `exposed_port_publisher_session_state.rs`, reading through the engine seam
  `core/runtime/exposed_output_port_subscription.rs`, `reserved_slot_forwarder.rs` and the
  per-channel reserved subscriber slot.
- **Exposure in the engine:** `core/runtime/loaded_stream_output_port_exposure.rs`,
  `core/graph/components/exposed_output_ports_component.rs`,
  `source_side_held_open_for_a_reader_outside_its_stream.rs`; the graph's `exposed` key
  (`graph_snapshot.rs`).
- **Exposure in Python:** `Exposure` and `StreamBuilder.expose`
  (`sdk/tatolab-stream/tatolab/stream/_stream_graph_builder.py:48`, `:348`), re-exported from
  `__init__.py:86`. `.expose(` is called in 18 Python files: 6 in the SDK's tests, 2 in the engine's
  fixtures, 2 in `tests/stream-on-runtime`, and 8 in consumers under `packages/`.
- **The local API and the CLI:** `expose_port` and `ExposePortLevel`
  (`streamlib-runtime-client-contract/src/local_api_wire_contract.rs`, `streamlib-api-server/src/mcp.rs:410`),
  `tatolab expose` (`tatolab-cli/src/main.rs:214`), and the MCP instructions that tell an agent to
  read a port by exposing it (`mcp.rs:100`).
- **Relay adapters in `tatolabd`:** the crates `streamlib-relay-adapter-sdk`, `-local`, `-cloudflare`
  and `-moqpro`; `tatolabd/src/relay_instances_of_this_runtime.rs`, `relay_instances_record.rs`,
  `relay_secret_store.rs`, `os_relay_secret_store.rs`, `local_relay_adapter_of_this_runtime.rs`;
  the four relay verbs and `/api/relay-adapters`.
- **The local relay:** `xtask/src/stage_local_relay.rs`, `moq-relay` in the runtime unit and the
  macOS helper bundle, its CI build steps (`ci.yml:286`, `:892`), and
  `tatolabd/tests/tatolabd_runs_the_local_relay.rs`.
- **The preview page:** `runtime/tatolab-watch/`, `xtask/src/stage_watch_page.rs`,
  `watch_page_third_party_notices.rs`, the `/watch` routes in
  `tatolabd/src/loopback_http_listener_of_this_runtime.rs`, bun in four workflow files.
- **The one built-in:** `streamlib-media-builtins/src/relay_track_source.rs`, generated into
  `_built_in_nodes.py`.
- **Stays:** `streamlib-relay-track-reader` (the receive node and `tatolab-read-track` share it),
  `tatolab-read-track`, the catalog-entry layout on output ports, the envelope's `starts_group`,
  the stamp re-basing rule, `packages/streamlib-webrtc`.

## ADDED

- **`packages/tatolab-moq`, a module.** A Rust-native extension wheel built as
  `packages/streamlib-webrtc` is, depending on `tatolab-stream` and the module host table and on no
  engine crate. It carries the moq-dev line (`moq-net`, `moq-tokio`, `hang`) at the pinned
  versions, the relay-track reader, and what differed per relay adapter — dialect, how a
  credential is held and refreshed, the root, the catalog rewrite cadence — as one gateway
  implementation per relay kind: a plain `moq-relay`, Cloudflare and moq.pro.
- **`MoqGateway`** — `execution = "manual"`, `paced_by_real_time`. Opens and holds one session to a
  relay, from its own config (decision 2): the relay kind, `relay_url` and the kind's credential
  keys. It publishes one broadcast it names (`broadcast`,
  under the connection's root). It writes the broadcast's catalog from its tracks' entries, the way
  the publisher did, and drops what no subscriber asked for. It has no data ports.
- **`MoqTrack`** — declares one outgoing track on a gateway (`gateway`, `track`, optional `catalog`
  overriding the upstream port's catalog entry). It has no data ports; it is the graph variable that
  send nodes refer to (decision 1). One track has one writer, so a second send node on a track is
  refused at load by name.
- **`MoqTrackSend`** — one input `content` of any family that has a wire form: Packets,
  Records, Channels and Array. It forwards each message's payload bytes as written, maps
  `starts_group` to the group boundary, drops the rest of a group on backpressure or on a message
  reporting a loss, and takes the catalog entry from its upstream port. A link from an Image port
  is refused at `connect`, naming an encoder, because an Image slot means nothing off the
  machine.
- **`MoqTrackReceive`** — the relay source node's contract, unchanged: `gateway`, `broadcast`,
  `track`, `max_age`, plus the family settings the strict-port-families change gave
  `RelayTrackSource` (`family`, and `codec` for Packets). Re-based stamps, losses counted in
  `lost_since_last_read`, `paced_by_real_time`.
- **`GraphInput` and `GraphOutput`** — the interface nodes (decision 4), each naming one port of
  one family; a stream's top-level Outputs are what Tato Studio displays, and a subgraph's are
  the ports the adding graph wires. They replace `expose` in the SDK and `exposed` in the graph.
- **Variables and the reference edge** (decisions 1 and 6) are defined once, in
  `lightweight-node-kinds` §Graph variables and reference edges, and built by its variables
  ticket. This change uses them: a track names its gateway and a send node its track by
  reference, and a stream holds a track as a graph variable.
- **Runtime viewers.** The local API serves a viewer's read of any port of any node on the
  machine, by family (§Control plane). How the viewer reads the port is the OPEN this change
  settles in its preview ticket; see decision 5.

## MODIFIED

- **§Networking** is rewritten: done in the plan PR with this file.
- **The codec rigs, the audio checks and the stream-on-runtime suite** read a port as a user
  does: the stream under test adds `MoqGateway` → `MoqTrack` → `MoqTrackSend` onto a `moq-relay` the
  rig starts as a fixture, and `tatolab-read-track` reads it. `codec_roundtrip_stream.py`'s receiver
  reads with `MoqTrackReceive`.
- **The relay verbs go.** A gateway's URL and credential are its config (decision 2), so the runtime stores
  no relay instance: `relays.json`, the OS relay secret store, the four relay verbs, `tatolab
  relays` and `relay`, and `/api/relay-adapters` with `streamlib://relay-adapter-catalog` are
  removed. No config key is a special secret type: a credential that comes from a secret store
  reaches the gateway on the wire, from a node that reads the store and writes the gateway's
  settings as a Records message to its `settings` input, which replaces the config's values.
  The in-flight `relay-setting-fields` change is orphaned and retires without
  shipping.
- **The MCP instructions** tell an agent to look at a port through the viewer read path and
  to send it off the machine with gateway nodes. They do not mention `expose_port`.
- **The scaffold** drops `H264Encoder` and `expose`. `TestPatternSource` → `HalftoneLensEffect`,
  with `BrightnessMeter` on a fan-out; the person sees the effect's output in Tato Studio's viewer.
  `tatolab-h264` leaves the scaffold's dependencies.

## REMOVED

- REMOVED: runtime/tatolabd/src/exposed_port_publisher.rs
- REMOVED: runtime/tatolabd/src/exposed_port_publisher_session_state.rs
- REMOVED: runtime/streamlib-engine/src/core/runtime/exposed_output_port_subscription.rs
- REMOVED: runtime/streamlib-engine/src/core/runtime/reserved_slot_forwarder.rs
- REMOVED: runtime/streamlib-engine/src/core/runtime/loaded_stream_output_port_exposure.rs
- REMOVED: runtime/streamlib-engine/src/core/graph/components/exposed_output_ports_component.rs
- REMOVED: runtime/streamlib-engine/src/core/graph/components/source_side_held_open_for_a_reader_outside_its_stream.rs
- REMOVED: class Exposure
- REMOVED: def expose
- REMOVED: ExposePortLevel
- REMOVED: expose_port
- REMOVED: runtime/streamlib-relay-adapter-sdk
- REMOVED: runtime/streamlib-relay-adapter-local
- REMOVED: runtime/streamlib-relay-adapter-cloudflare
- REMOVED: runtime/streamlib-relay-adapter-moqpro
- REMOVED: runtime/tatolabd/src/local_relay_adapter_of_this_runtime.rs
- REMOVED: runtime/tatolabd/src/relay_instances_of_this_runtime.rs
- REMOVED: runtime/tatolabd/src/relay_instances_record.rs
- REMOVED: runtime/tatolabd/src/relay_secret_store.rs
- REMOVED: runtime/tatolabd/src/os_relay_secret_store.rs
- REMOVED: runtime/tatolabd/src/secret_holding_test_relay_adapter.rs
- REMOVED: runtime/tatolabd/tests/tatolabd_holds_the_machines_relays.rs
- REMOVED: runtime/streamlib-api-server/src/relay_tools_tests.rs
- REMOVED: add_relay
- REMOVED: configure_relay
- REMOVED: remove_relay
- REMOVED: list_relays
- REMOVED: relay-adapter-catalog
- REMOVED: runtime/tatolabd/tests/tatolabd_runs_the_local_relay.rs
- REMOVED: runtime/tatolabd/tests/tatolabd_publishes_exposed_ports_at_the_local_relay.rs
- REMOVED: xtask/src/stage_local_relay.rs
- REMOVED: runtime/tatolab-watch
- REMOVED: xtask/src/stage_watch_page.rs
- REMOVED: xtask/src/watch_page_third_party_notices.rs
- REMOVED: watch_page_directory_beside_the_lend
- REMOVED: runtime/streamlib-media-builtins/src/relay_track_source.rs
- REMOVED: RelayTrackSource
  Continuation: the receive node's name in prose, the generated class and the MCP instructions
  all go; `tatolab.stream:RelayTrackSource` in a graph is refused naming
  `tatolab.moq:MoqTrackReceive` and `tatolab-moq`, the refusal every former built-in gets.

## Decisions

**Decided 1 — a send node names its track by a node reference in config (owner, 2026-11-06).**
`MoqTrackSend(track=camera_track)`, recorded as a reference edge and checked at load.
Rejected: a named variable string resolved at load; a connection carried on a special port and
wired with `connect` (owner, 2026-11-06).

**Decided 2 — a gateway's URL and credential are config on the gateway node (owner, 2026-11-06).**
"It would become a config variable in the node for gateways." Rejected: the runtime storing
named connections in `relays.json` and the OS secret store (owner, 2026-11-06).

**Decided 3 — the bundled local relay is removed (owner, 2026-11-06).** `tatolabd` runs no
relay; a rig or a person runs `moq-relay`. Rejected: keeping it, started only when a gateway names
the local connection (owner, 2026-11-06).

**Decided 4 — a graph's interface is Input and Output nodes, at every level (owner, 2026-11-06).**
"Similar to davinci resolve and effect house we need an output and input nodes (this will be
relevant also for what the tato studio can display) so it means at every level we are
'exposing' things." `GraphInput` and `GraphOutput` nodes declare a stream's and a subgraph's ports;
Tato Studio shows a graph's Outputs. Rejected: `expose` kept for subgraphs; leaving it OPEN
(owner, 2026-11-06).

**Decided 6 — variables hold a value or a node reference (owner, 2026-11-06).** "We will need to
support variables so we can get and set just like unreal engine blueprints." `SetVariable` and
`GetVariable`, specified by `lightweight-node-kinds`, resolve to a link for a port's output and to
a reference edge for a node reference. A connection is held as a reference to its gateway node.
Rejected: a connection carried as a Records value (owner, 2026-11-06).

**Decision 5 — how a viewer reads a port (left OPEN in the plan).** Direction (session, not
decided): Tato Studio opens a read of the port through the local API, and the runtime hands it
the port's channel as it hands a helper process one. Image surfaces are imported the way a helper
imports them, so a preview is zero-copy and never touches a relay. Settled in the preview ticket.

## Tickets (as few tracer bullets as the change needs)

1. **`tatolab-moq` with the gateway, track, send and receive nodes**, the interface nodes, and
   the gateway's `settings` input, proven by a rig stream that sends to and receives from a
   fixture `moq-relay`. `RelayTrackSource` and the relay adapters leave the runtime. Waits on
   `lightweight-node-kinds`' variables ticket, which waits on `strict-port-families` in full.
2. **The automatic path is ripped out:** the publisher, exposure, `expose`, `expose_port`,
   `tatolab expose`, the local relay, the relay verbs and their records, and the preview page. The rigs and the stream-on-runtime suite
   move onto ticket 1's nodes. The scaffold loses its encoder.
3. **Runtime viewers:** the viewer read path and the per-family views in Tato Studio, settling
   decision 5.

Consumers under `packages/` that call `.expose(` get backlog filed at each consumer
(§Consumers). They are not in this change.
