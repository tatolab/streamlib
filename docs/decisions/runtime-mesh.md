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
  name belongs to the runtime, defaults to `<hostname>-<app directory>`, and is unique within
  a mesh.
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
- **Loss and scope.** A send the network cannot take is dropped and counted per remote link.
  Notify services, the event bus, request-response and blackboard never cross.
- **Visibility.** `graph` and `streamlib nodes` show mesh peers. A runtime without a control
  plane still carries links.

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
