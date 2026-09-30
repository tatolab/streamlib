# One runtime per machine: streams as data, and a runtime that decides what leaves the machine

Rationale for the `[one-runtime-per-machine]` entries in `docs/plan/ARCHITECTURE.md` and the
2026-09-30 vocabulary in `docs/plan/GLOSSARY.md`, from the owner's pivot of 2026-09-30. The
owner confirmed five sentences verbatim, reopened the second's "daemon" wording the same day,
then restated its substance and settled the vocabulary while reviewing the plan PR. "Host" in
the sentences is what the glossary calls the **runtime**.

Annotated in place: `runtime-mesh.md`, `importable-python-library.md`, `product-mvp-sentence.md`,
`control-plane-one-surface.md` and `python-kernel-api.md`. `macos-platform-floor.md` stands
whole, with a read-with note.

## Trigger

Read this before any of the following:
- building the runtime's stream loading, a second engine process on a machine, or anything
  that runs several streams in one process;
- building the runtime's mesh session from engine defaults, or proposing that MoQ carry a link
  between runtimes;
- changing the local API's transport, or adding a URL form;
- naming anything a Tatolab user reads: a stream, a node, a port, an address, the runtime;
- renaming the packages.

## The direction, verbatim (owner-confirmed)

1. A graph is data: a serializable description that apps build and hosts run, and `setup(rt)`
   compiles to one.
2. Each machine runs one host daemon that owns the GPU, the shared-memory transport, the
   machine's single Zenoh session, one local API and the scheduler, and loads many graphs live,
   while the single-graph embedded mode stays for development.
   - *Reopened the same day for its "daemon" and "embedded mode" wording, then restated: the
     runtime is centralized on the machine, runs many streams from different people or teams
     behind a single Zenoh router, and the GPU is optional. Testing a stream without a full
     runtime is separate work.*
3. The host decides what leaves the machine: it builds its Zenoh session from its own
   configuration, dials routers, and enforces a pushed stream map that refuses links to
   unexposed ports and authenticates peers.
4. Zenoh stays the transport between machines and MoQ stays edge I/O, so the MoQ-gateway
   transport change on `spike/moq-gateway` is not happening.
5. streamlib ships under a `tatolab.*` namespace, as a pure-Python stream package and a native
   host package, with accelerators optional, an extension point for an external control client,
   and support for Linux and Apple Silicon macOS with no streamlib-owned service.

The owner's later clarifications, the same day:

- A **stream** is what we call an app today: a graph plus its code, built by `setup`; it may
  expose several ports. The point is composing streams written by different people or teams on
  one machine, run by the one runtime, the way vehicles run one Zenoh setup.
- The motive is decoupling: before, "a game engine per stream" coupled the engine, the GPU,
  the shared-memory transport, the Zenoh session and the API server in every app. The split is
  Tailscale-shaped and maps onto standards-compliant `tatolab.*` distributions — no custom
  module system.
- The **relay is a role**, not a place: a public VM runs the same runtime software — a Zenoh
  router plus a fast translation to MoQ — with a stable URL; a single node on a private network
  can serve MoQ itself.
- **MoQ is only for WebTransport into a browser**, or a consumer that asks for it. Raw UDP,
  MPEG-TS, HLS, JSON lines and whatever else the consumer wants are expected: the goal is
  "anything accessible anywhere", drawing on Plan 9 and Tailscale.
- **People can use Tailscale.** streamlib does not solve every networking problem; the focus is
  streams available by URL, even inside a private network, made super simple. Not every problem
  needs a Tatolab answer.
- **Users never meet the mechanism.** As PyTorch users never think about its C++ core, a Tatolab
  user knows only that they are using a stream. `rt.add` was the wrong shape because a stream is
  being built, not a runtime modified; `@processor` was the wrong word because it does not read
  as part of a stream. The user's words are the glossary's `(user)` entries: stream, node, name,
  port, link, address, exposed port, form, graph, runtime, machine, relay. A word for an exposed
  output ("feed") is a later addition if the double meaning of "stream" bites.

## Decided

- **A stream is the unit** a person writes and runs, and **one runtime per machine** runs many
  of them behind the machine's single Zenoh session, owning the accelerator when present, the
  local API and the scheduler. Every Python node keeps its own process; agents keep changing
  live graphs.
- **A graph is data.** `setup(stream)` compiles to the stream's graph, one shape with what the
  runtime renders live; the existing snapshot extended, never a second format.
- **The runtime decides what leaves the machine.** It builds its Zenoh session from its own
  configuration, dials routers, and enforces a pushed stream map that refuses links to
  unexposed ports and authenticates peers. Its session is the machine's router.
- **Zenoh between machines; MoQ only as a browser form.** MoQ never carries a link between
  runtimes, and the MoQ-gateway transport change is not happening.
- **The relay is a role** of the same runtime software; machines link peer to peer where they
  can reach each other and through a relay where they cannot.
- **streamlib does not solve every networking problem.** NAT, peer identity and wire
  encryption between machines are Tailscale's, a VPN's, or a relay's; streams as URLs, what
  leaves a machine, and many streams on one runtime are streamlib's.
- **Every exposed port is a URL** from any tool, in the form that tool wants; a user with no
  account can see and use their own streams locally, and inside a private network.
- **Packaging.** A `tatolab.*` namespace: a pure-Python stream package and a native runtime
  package; Linux and Apple Silicon macOS; no streamlib-owned service.
- **Accelerators are optional.** A stream that needs no GPU runs on a machine without one.
- **An extension point for an external control client.** The runtime runs complete without one.
- **The vocabulary** in the glossary, with its register markers.

## Open

Each is an OPEN entry in the plan. Where the plan records a *direction (review, not decided)*,
that is the 2026-09-30 review's recommendation, kept apart from what the owner said.

- How the runtime is started and kept alive, and what `run` does with none running.
- How the stream package and the runtime package stay independent, and how the runtime and a
  stream's processor interpreters agree on a build.
- How an external control client plugs in.
- The remaining names: distributions, imports, the CLI command, the Rust crate, the extensions'
  entry-point group.
- What the graph holds beyond nodes, links and exposures; whether it is only emitted or may be
  authored; whether loading a stream provisions its environment; what is authoritative after a
  live edit.
- Several streams in one runtime process: what must stop being process-wide, and how streams
  reach each other.
- Resources across streams.
- Failure isolation: whether a native crash in one stream must leave the others running.
- What optional accelerators mean for the engine.
- Camera and microphone permission on Apple when the product starts the runtime.
- Whether addresses gain a stream level.
- The stream map's details, including what a runtime with no map exposes.
- The URL grammar, the forms and their order, certificates per reach tier, where the MoQ server
  lives, and whether end-to-end encryption through a relay is a launch requirement.
- The local API's protocol, MCP reach, remote reach and the multi-user case.

The owner's constraints on these: streams get their own compute; every stream is addressable
by URL, somewhat in the manner of Plan 9; no second mode unless it solves a real problem; it
runs on very low-power devices; a change to a stream never requires reinstalling the runtime,
and each package and each stream is testable on its own; every Python node keeps its own
process; agents keep changing live graphs; agents reach streams by URL from any tool; a user
with no account can see and use their own streams locally; people won't accept the operator
seeing their content, so end-to-end encryption is expected.

## Why

Today every streamlib app is a full runtime: its own engine, Vulkan device, shared-memory
transport, Zenoh session, and — under `run` and `dev` — its own API server with MCP. The
coupling, not any one cost, is the problem: networking and the GPU are wired into each app.
Checked against the tree on 2026-09-30:

1. **Cost per stream.** Each app pays for a tokio pool, a Zenoh session, an iceoryx2 node, a
   surface-share service and a Vulkan device. A link between two apps on one machine rides
   loopback QUIC with every frame copied out of the GPU and back in. (Stamps from two apps on
   one machine already compare — the clock identity is the boot id — so the earlier claim that
   they cannot is withdrawn.)
2. **Networking leaks into streams.** Reaching a browser means wiring a MoQ or WebRTC publisher
   into the app's own graph, and the control plane itself is a processor added to the graph.
3. **No enforcement point.** Every app's session listens on every interface with multicast
   discovery on; any runtime may wire a link into, or remove a link from, any other; every
   output port is offered; and the MCP server binds `0.0.0.0:9000` with authentication off.
   Nothing in the engine has a notion of "exposed". This is the strongest reason for the pivot,
   and its hardening does not wait on it.
4. **Streams from different authors need different Python packages.** Today each app has its
   own venv, so nothing collides; this becomes a requirement the moment one runtime runs many
   streams, and the design has to meet it.
5. **Resources are unmanaged across streams.** There is no arbiter for realtime priority and no
   GPU memory budget; no failure has been observed yet, and the one relevant learning runs the
   other way — two Vulkan devices in one process crash on NVIDIA, which is why one runtime holds
   exactly one.

## Rejected alternatives (by the owner)

- **MoQ as the transport between machines** (the `spike/moq-gateway` direction), or a publisher
  inside every app. It adds a second network protocol to every machine and keeps networking
  inside streams.
- **A new high-level wiring language** replacing `add` and `connect`. It invents a second way
  of working; the builder object changes, the calls do not.
- **Per-runtime launchd services with an orchestrator sharing surfaces.** The earlier setup,
  which `macos-platform-floor.md` already rules out.
- **"Host" as the name of the per-machine program.** Retired once already, carried about
  ninety other senses in the plan (host-visible memory, host pointers, host identity, `--host`),
  and users never say it; "runtime" keeps every mesh term with one runtime per machine.
- **A separate accelerated distribution.** An extra cannot change the runtime's native module,
  and a second engine-linking distribution reverses "no process ever holds two streamlib
  engines" and the deleted bridge traits. Optional accelerators are a runtime property.

Kept as the fallback, not rejected: **one engine process per stream behind the same router**,
if the owner rules that a native crash in one stream must never end the others.

## Consequences

- **No change proposal builds against an OPEN entry** until it is decided.
- **The rename follows sentence 5** and comes last; the exact names are OPEN, with
  `tatolab-stream` and `tatolab-runtime` the stated assumption.
- **Older entries are read through the plan's reading rule** until the rename change re-spells
  them; the records annotated in place change only where a confirmed sentence changes them.
- **The rip-out change** (`docs/plan/changes/one-runtime-per-machine-ripout.md`) is on hold
  until the local-API entries are decided.
