# The runtime mesh is always-on engine transport

Rationale for the `[runtime-mesh]` entries in `docs/plan/ARCHITECTURE.md` §Networking,
decided 2026-09-14.

## Trigger

Read this before proposing that runtime-to-runtime data ride a processor, a gateway node, an
extension wheel or upstream iceoryx2's Zenoh tunnel or gateway. Read it too before adding a
per-link or per-stream network configuration step, or before keying anything on the mesh by
a processor id or a channel name.

## Decision

Zenoh is the runtime's cross-machine transport. It is always on, and it sits beside iceoryx2
as the other half of how a link is carried. Every runtime opens one Zenoh session in the app
process, announces itself, and discovers other runtimes peer-to-peer by default. The engine
carries a link on iceoryx2 when both ends share a runtime and on Zenoh when they do not, so a
processor is unchanged either way.

- **Mesh name.** Everything on the mesh lives under a mesh name, `default` unless a runtime
  names another. There is no switch that turns the mesh off.
- **Addressing.** A port on the mesh is `<runtime name>/<display name>/<port>`. The runtime
  name belongs to the runtime, defaults to `<hostname>-<app directory name>-<id>` with the id
  hashed from the directory's full path, is never auto-suffixed, and is unique within a mesh.
- **Surfaces.** A top-level `surface_id` crosses as the frame's pixels and lands as a freshly
  minted local surface id. The `surface_id` key is a stand-in until a general mechanism
  replaces it.
- **Security.** There is no authentication in this work; that is a separate, later pass.
- **Remote links.** Any runtime may create one: a receiver pulling, a sender pushing, or a
  third runtime wiring two others. The runtime owning the input applies it with `connect`'s
  own refusals. A missing runtime makes the link wait; a missing port is refused by name.
  Nothing is sent for a port no remote link reads.
- **Stamps.** Stamps cross unchanged, tagged with the clock that produced them, and are never
  compared across clocks. A negotiated network clock (PTP, NTP or similar) is intended later.
- **Loss and scope.** A send the network cannot take is dropped and counted per remote link,
  by a gap in a sequence number the sending runtime carries in each mesh message's metadata,
  never in the bag.
  Notify services, the event bus, request-response and blackboard never cross.
- **Visibility.** `graph` and `streamlib nodes` show mesh peers. A runtime without a control
  plane still carries links.

## How the announcement is built

Recorded with the `runtime-mesh` proposal. It reads `zenoh` 1.10.1 and the rig probes.

- **Token plus queryable.** A liveliness token carries no payload. So the token key holds only
  what a runtime that has already died must still answer, its host identity and pid, and a
  queryable beside it describes the live runtime. The description is answered at query time,
  so a control plane hosted after construction appears in it.
- **A verbatim `@runtime` chunk.** No `**` subscription over port addresses ever matches it, and
  a display name may not begin with `@`, so no address can collide with it.
- **TCP and UDP, default features off.** Realtime media needs UDP (owner, 2026-09-14): a `udp/`
  link is best-effort, so a lost packet costs that message rather than stalling the ones behind
  it, and `?rel=1` gives a reliable link over unencrypted QUIC with nothing to provision. UDP
  brings CDLA-Permissive-2.0 `webpki-roots`, a permissive data licence with no conflict for
  commercial distribution, accepted into `deny.toml`. TLS `quic/` waits for the security
  milestone because it needs a provisioned key and certificate. Zenoh is elected under
  Apache-2.0.
- **The duplicate check needs the mesh before the runtime exists.** The session therefore opens
  in `Runner::new()`, beside the runtime-id socket refusal, which needs no GPU.
  - Cost: `Runtime()` takes Zenoh's 500 ms scouting delay while multicast discovery is on.
  - A same-host exception needs the pid in the key: a killed runtime's token is gone within
    milliseconds because the kernel closes TCP, but a restart can race that.
- **Tests run with discovery off.** Otherwise parallel test runtimes on one machine would find
  each other under one default name and refuse.

## How links are carried

Recorded with the `cross-runtime-links` proposal. It reads `zenoh` 1.10.1 at tag `1211779` and the
engine at `d4ce808f6`.

- **The input's runtime always pulls.** A push and a third-party wiring become a request to that
  runtime, which applies `connect` with a remote source. One data shape and one set of refusals
  then serve all three ways of creating a link.
- **Readers announce, sources watch.** A reader declares a liveliness token under the source runtime's
  verbatim prefix, and the source creates an egress when the first token for a port appears. Zenoh's
  stable matching status was rejected: one wildcard subscriber anywhere on the mesh would start every
  source's network work.
- **The hop count reuses the local sequence number.** Egress copies each sample's user-header number
  into the attachment, so one gap at ingress covers the egress ring, refused copies, Zenoh's silent
  `Drop`, the network and the ingress ring. A second numbering minted at egress would miss the
  source-side ring.
- **Nothing blocks on a Zenoh thread or a producer's.** `put().wait()` can hold its caller for about
  51 ms while a fragmented message queues, so it runs on the egress thread. A subscriber callback runs
  on the link's receive loop, so it only hands off into a ring.
- **Surfaces copy through export staging, never through `exchange`.** `exchange` converts to RGBA8 and
  builds resources per call. A pooled buffer's mapping is write-combined memory, where a 1080p memcpy
  cost 37 ms. The claim spans the GPU copy alone, so a slow network never pins the producer's slot. A
  frame lands as a pooled pixel buffer, because no texture mint stamps a generation.
- **Egress forwards every bag.** Skipping to the newest at the sender for a `newest` reader would make a
  gap at ingress ambiguous between loss and the profile working. Skipping stays at the receiving port.

## Rejected alternatives

- **A gateway processor, or a built-in pair.** A processor has to be wired into a graph, so
  any stream not wired to it is unreachable from another machine. The mesh would then behave
  like edge I/O, the MoQ and WebRTC shape, rather than like the transport links already ride.
- **An extension wheel.** It runs in a helper and sees only decoded bags. It could not reach
  the engine's per-link sequence numbers or copy a frame at the transport, and it would pay
  the helper hop's copies on every bag. The transport is also not an optional capability:
  easy, automatic exchange between runtimes is what the runtime is for.
- **Upstream iceoryx2's tunnel or gateway.** Its core joins every service as both publisher
  and subscriber. That collides with a channel's single publisher and a notify service's
  single listener. It keys by service hash rather than a name a remote runtime can know, it
  counts no drops, and it forwards surface ids that mean nothing on another machine.
  - Rechecked 2026-09-14 against upstream main `b1c4cae`. The tunnel is now the
    `iceoryx2-gateway` with an `integrations/zenoh/gateway-backend` (upstream #1891),
    unreleased: it needs iceoryx2 main, Rust 1.89 and zenoh 1.9 with `unstable`, and crates.io
    holds only `iceoryx2-tunnels-zenoh` 0.7.0 from 2025-09. It runs as `iox2 gateway zenoh`
    or embedded as a library. Every finding above still holds in that source:
    `ports/publish_subscribe.rs` does `open_or_create` plus a publisher and a subscriber;
    the key is `iox2/v1/publish_subscribe/<service hash>/<config fingerprint>`; the payload
    is a `Passthrough` byte frame under `Reliability::Reliable`; and it bridges every
    allow-listed service the moment it is discovered, so nothing is lazy. What it shares with
    this design: a liveliness token plus a queryable per announcement, `Locality::Remote`, and
    peer mode with multicast scouting.
- **Endpoint-config stream names (the MoQ `track_names` shape).** This would put a naming
  step on every stream a user wants to reach. A stable address derived from names the runtime
  already has makes every port reachable without configuration.
- **Per-run processor ids or cuid2 channel names as keys.** They change every run, so no
  remote runtime could name them.
- **A second per-processor name field for addressing.** It would give one processor two
  names. The display name is already human-chosen and unique within a graph.
- **Receiver-only link creation.** It would stop an agent in one runtime from pushing its
  data into a processor it found on another runtime unless that runtime hosted a control plane
  the agent could call. Dynamic rewiring by agents is the intent. The input's runtime still
  applies every link with the same refusals, so pushing adds no second rule set, and who may
  wire belongs to the security pass.
- **Comparing stamps across machines, or stamping on Zenoh's hybrid logical clock.** Each
  machine's monotonic clock has its own boot epoch, and the hybrid logical clock is wall time
  plus a counter, not a media clock. Until a common network time is negotiated, the only safe
  rule is never to compare stamps from two clocks.
- **Blocking a sender under network congestion.** It breaks "no link ever blocks a producer",
  and Zenoh closes a peer's transport after a stalled blocking send.
- **An off switch.** A dial the zero-ceremony bar does not want. Isolation already has three
  levers: a mesh name, explicit peers, and discovery turned off.
- **Auto-suffixing a duplicate default name, the way the control-plane port increments from
  9000.** A port is a transient the registry records; a runtime name is the address other
  runtimes and agents wire against. A suffix chosen by start order makes yesterday's link reach
  a different runtime today, and a `dev` restart that loses the pid-gone race comes up as `-2`
  while every remote link to the old name waits forever. Hashing the directory's full path into
  the default gives two checkouts different names with no flag, and a real duplicate still fails
  by name (owner, 2026-09-14).
- **Encoding a runtime's description into its token key.** A control-plane URL holds `/`, and it
  appears only once a control plane is hosted, after the token is declared.
- **Zenoh's `AdvancedPublisher` cache or `zenoh-ext` group membership for the description.** Both
  are `unstable`, and `zenoh-ext`'s default features turn every transport back on.
- **Zenoh's `namespace` config as the mesh name.** It prefixes keys but separates nothing
  further, and its tests are unstable-only, so the engine writes the prefix itself.

## Consequences

- A runtime is reachable on its network by default and carries no authentication until the
  security pass. That is accepted for deployments on isolated networks.
- Renaming a processor changes its address, so display names become part of what a mesh
  depends on.
- The plan's statement that the engine inspects no bag content gains a second, narrow
  exception: the mesh reads a top-level `surface_id`. The window contract is the first.
- The glossary's **Node** means a runtime reachable over its control plane. A runtime can now
  be on the mesh and carry links without hosting a control plane, so the two terms have to be
  kept apart.
- The data plane's one monotonic clock is comparable within one machine only. A join across
  machines has to re-anchor explicitly until a common network clock exists.
- Any runtime on a reachable network can wire links into any other until the security pass
  lands.
