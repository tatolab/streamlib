# The one-host-per-machine pivot: graphs as data, and a host that decides what leaves the machine

Rationale for the `[one-host-per-machine]` entries in `docs/plan/ARCHITECTURE.md`, from the owner's pivot of 2026-09-30. The owner confirmed five sentences verbatim, then reopened the second in discussion the same day.

This record keeps what is decided apart from what is open. The plan carries:
- six DECIDED entries, taken from sentences 1, 3, 4 and 5;
- fourteen OPEN entries.

Annotated in place: `runtime-mesh.md`, `importable-python-library.md`, `product-mvp-sentence.md`, `control-plane-one-surface.md` and `python-kernel-api.md`. `macos-platform-floor.md` stands whole, with a read-with note.

## Trigger

Read this before any of the following:
- building a host, a daemon, or several graphs in one process (all OPEN);
- building a host's mesh session from engine defaults;
- proposing that MoQ carry links between runtimes;
- changing the control plane's transport (OPEN);
- renaming the packages.

## The direction, verbatim (owner-confirmed)

1. A graph is data: a serializable description that apps build and hosts run, and `setup(rt)` compiles to one.
2. Each machine runs one host daemon that owns the GPU, the shared-memory transport, the machine's single Zenoh session, one local API and the scheduler, and loads many graphs live, while the single-graph embedded mode stays for development.
   - *Reopened by the owner the same day. See Open.*
3. The host decides what leaves the machine: it builds its Zenoh session from its own configuration, dials routers, and enforces a pushed stream map that refuses links to unexposed ports and authenticates peers.
4. Zenoh stays the transport between machines and MoQ stays edge I/O, so the MoQ-gateway transport change on `spike/moq-gateway` is not happening.
5. streamlib ships under a `tatolab.*` namespace, as a pure-Python stream package and a native host package, with accelerators optional, an extension point for an external control client, and support for Linux and Apple Silicon macOS with no streamlib-owned service.

## Decided

- **A graph is data.** `setup(rt)` compiles to a serializable description, which hosts run.
- **The host decides what leaves the machine.**
  - It builds its Zenoh session from its own configuration and dials routers.
  - It enforces a pushed stream map that refuses links to unexposed ports and authenticates peers.
- **Zenoh between machines, MoQ at the edge.** MoQ never carries links between runtimes, and the MoQ-gateway transport change is not happening.
- **Packaging.** streamlib ships under a `tatolab.*` namespace as a pure-Python stream package and a native host package. It supports Linux and Apple Silicon macOS, and owns no service on either.
- **Accelerators are optional.** A stream that needs no GPU runs on a machine without one.
- **An extension point for an external control client.** streamlib runs complete without one.

## Open

The owner will settle these through the plan's own process. Each is an OPEN entry in the plan.

- **How streams are hosted.** Sentence 2, reopened. The candidates:
  - a host daemon beside an embedded single-graph mode;
  - one kind of runtime that runs one or more streams, started with a stream or run as a service.
- **How the stream package and the host package stay independent,** and how they agree on a build.
- **How an external control client plugs in.**
- **The remaining names:** the extension group, distributions and imports, the CLI and the Rust crate.
- **What a graph description holds,** and whether it extends the engine's graph snapshot.
- **Several streams in one engine process:** names, links, what one stream may reach of another, conflicting Python packages, and surface sharing.
- **Resources across streams.**
- **Failure isolation when one process holds several streams.**
- **What optional accelerators mean for the engine.**
- **Camera and microphone permission on Apple** when something other than the user's own application starts the engine.
- **Whether addresses gain a stream level.**
- **The stream map's details:** who pushes it, the default with no map, its validity offline, and peer authentication.
- **Agent access by URL:** descriptions, local URLs, and end-to-end encryption.
- **The local API:** its protocol, MCP access, remote reach, and the stream-management verbs.

The owner's constraints on these:

- **Streams get their own compute,** and every stream is addressable by URL, somewhat in the manner of Plan 9.
- **No second mode** unless it solves a real problem.
- **It runs on very low-power devices.**
- **A change to a stream never requires reinstalling the host,** and each package, and each stream, is testable on its own as an atomic unit.
- **Every Python processor keeps its own process,** and agents keep changing live graphs.
- **Agents reach streams by URL from any tool,** and a user with no account can see and use their own streams locally.
- **People won't accept the operator seeing their content,** so end-to-end encryption is expected.

## Why

Today every streamlib app is a full runtime, with its own engine, GPU device, shared-memory transport, Zenoh session, and API server with MCP. That causes five problems:

1. **Cost per graph.** A machine running several graphs, such as a robot with a graph per limb, pays for all of that several times over. Links between graphs go over the network, and stamps from different runtimes can't be compared.
2. **Networking leaks into graphs.** Making an output reachable from another machine means wiring a publisher into the app's own graph.
3. **No enforcement point.** Every runtime listens on every interface with multicast discovery, and any runtime on the mesh may wire to any port.
4. **Imports collide.** Graphs from different authors need different Python packages.
5. **Resources are unmanaged** across graphs.

## Rejected alternatives (by the owner)

- **MoQ as the transport between machines** (the `spike/moq-gateway` direction), or a publisher inside every app. It adds a second network protocol to every machine, and keeps networking inside graphs.
- **A new high-level wiring language** replacing `add` and `connect`. It invents a second way of working.
- **Per-runtime launchd services, with an orchestrator sharing surfaces.** This is the earlier setup, which `macos-platform-floor.md` already rules out.

## Consequences

- **No change proposal builds against an OPEN entry** until it is decided.
- **The rename follows sentence 5.** The exact names are OPEN.
- **The records annotated in place change only where a confirmed sentence changes them.**
