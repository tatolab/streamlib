# One host per machine: graphs as data, a local API, and a mesh the host configures

Rationale for the `[one-host-per-machine]` entries in `docs/plan/ARCHITECTURE.md`, decided by the owner in a pivot on 2026-09-30, direction confirmed verbatim the same day. It supersedes `control-plane-bind-posture.md` and clauses of `runtime-mesh.md`, `control-plane-one-surface.md` and `importable-python-library.md`; it amends `helper-process-placement-only.md`, `product-mvp-sentence.md`, `extension-model.md`, `execution-model.md` and `shutdown-ladder.md`; and it reopens the build-id handshake of `local-transport-hardening.md`. Each is annotated in place. It keeps `macos-platform-floor.md` whole, and confirms §Networking's reading of MoQ as edge I/O.

## Trigger

Read this before any of the following:

- starting a second runtime on one machine to run a second graph;
- building a mesh session from engine defaults;
- wiring a publisher into an app's graph to make an output reachable from another machine;
- giving a runtime its own HTTP control-plane server, or making a control plane reachable from another machine;
- spawning a helper with the app's interpreter;
- assuming a GPU is present;
- proposing that MoQ carry links between runtimes.

## The direction, verbatim (owner-confirmed)

1. A graph is data: a serializable description that apps build and hosts run, and `setup(rt)` compiles to one.
2. Each machine runs one host daemon that owns the GPU, the shared-memory transport, the machine's single Zenoh session, one local API and the scheduler, and loads many graphs live, while the single-graph embedded mode stays for development.
3. The host decides what leaves the machine: it builds its Zenoh session from its own configuration, dials routers, and enforces a pushed stream map that refuses links to unexposed ports and authenticates peers.
4. Zenoh stays the transport between machines and MoQ stays edge I/O, so the MoQ-gateway transport change on `spike/moq-gateway` is not happening.
5. streamlib ships under a `tatolab.*` namespace, as a pure-Python stream package and a native host package, with accelerators optional, an extension point for an external control client, and support for Linux and Apple Silicon macOS with no streamlib-owned service.

## What it means, piece by piece

- **Graph description.** A graph is a serializable description. It names processors by import path and carries their config, the connections between them, the graph's name, a resource request, and optionally the outputs its author suggests exposing. It extends the round-trippable graph snapshot the engine already loads and saves, rather than standing beside it. The Python and Rust APIs build it; a host runs it. `setup(rt)` keeps working because the runner compiles it into a description.
- **Declarations are metadata.** A processor's declaration is recorded on its class and readable without the engine, so the pure-Python distribution can build and check a description alone. The description carries each processor's declaration, so a host learns a graph's processors without importing the graph's classes; only the graph's helpers import them.
- **Two modes.**
  - *Embedded:* one graph in one process, today's `streamlib dev`.
  - *Hosted:* graphs submitted to the machine's host.

  Both run the same description.
- **The host daemon** owns, once per machine:
  - the GPU device and its surface pools;
  - the iceoryx2 domain;
  - the machine's single Zenoh session;
  - the local API;
  - scheduling: requests and limits per graph (cores, threads, GPU memory, priority), one arbiter for realtime thread priority, and admission control that refuses a graph the machine can't fit.

  Graphs are namespaces inside it. They load and unload live through the graph mutations the control plane already carries: `add_processor`, `remove_processor`, `connect`, `disconnect`.
- **Local API.** One Unix domain socket, with permissions limited to the machine's users, replaces each runtime's own HTTP/WebSocket server. The host serves it in hosted mode, and an embedded runtime serves its own. The builders' MCP tools are served from it. The CLI and any desktop UI are clients of it. The control plane is reachable only on its machine; what crosses machines is the mesh.
  - How an MCP host reaches the tools on the socket is OPEN, since MCP's transports are stdio and HTTP.
  - Whether a host serves a machine or a user is OPEN: which user it runs as, and where its socket lives.
- **Mesh.** The host builds its Zenoh session from its own configuration: the routers to dial and the credentials to present. It no longer uses fixed defaults.
- **Policy hook.** The host enforces a stream map pushed to it by whoever controls the machine:
  - which ports may be linked from outside the machine, and by which peers;
  - remote links to any other port are refused;
  - peers authenticate before linking.

  A host with no stream map exposes nothing beyond the machine. The stream map arrives through the local API or from an external control client. This is the security pass the mesh decision deferred; how a peer authenticates is OPEN.
- **Addresses** gain a graph level: `machine/graph/processor/port`.
- **Environments.** Each graph may carry its own environment. Helpers for a graph's processors run that graph's interpreter, which is still an exec, never a fork.
- **Accelerators.** A host without a GPU runs the graphs that need none, and refuses at load a graph that needs one. Accelerator support beyond the engine's own RHI is a capability extension, present where a device is. Where that line falls — which adapter crates move, and whether any `GpuContext` capability moves with them — is OPEN.
- **Control-client extension point.** An external control client, such as a product's enrolment and policy agent, loads into the host through the capability-extension mechanism. streamlib runs without one.
- **Packaging.** `tatolab-stream` is pure Python: the graph description, stream types and processor declarations. `tatolab-host` is native: the engine, the CLI and the local-API client. Both sit in one PEP 420 namespace, and no distribution ships `tatolab/__init__.py`.
- **macOS.** The host runs with no streamlib-owned `.app`, launchd service or installer, so `macos-platform-floor.md` stands. A separate product app may keep the host alive.
  - Surface sharing to the host's spawned helpers carries over unchanged, and needs verifying.
  - Sharing surfaces beyond the host's own children is OPEN.
  - How camera and microphone permission is attributed when a launch agent starts the host is OPEN. The engine finds the responsible application by walking up the process tree, and a launch agent's parent is launchd.
- **MoQ** stays edge I/O at a runtime boundary. Publishers leave app graphs, and the `spike/moq-gateway` direction is retired.

## Why

Today every app is a full runtime with its own engine, GPU device, iceoryx2 domain, Zenoh session, and API server with MCP. That causes five problems.

1. **Cost per graph.** A machine running several graphs, such as a robot with a graph per limb, pays for all of that several times over. Links between graphs go over the network, and stamps from different runtimes can't be compared.
2. **Networking leaks into graphs.** Making an output reachable from another machine means wiring a publisher into the app's own graph.
3. **No enforcement point.** Every runtime listens on every interface with multicast discovery, and any runtime on the mesh may wire to any port.
4. **Imports collide.** Premade graphs from different authors need different Python packages, and one venv can't hold them all.
5. **Resources are unmanaged.** Cores, threads, realtime priority and GPU memory have no owner across graphs.

## Rejected alternatives

- **A runtime per graph in production (today's model).** It multiplies engines, GPU contexts and network sessions. Links between graphs become network hops, clocks can't be compared, and there's no enforcement point.
- **Everything in one runtime with one environment.** It's efficient, but graphs with conflicting imports can't coexist.
- **A new high-level wiring language replacing `add` and `connect`.** It invents a second way to build graphs, and chaining sugar gets hard to follow once stages have several inputs and outputs. Authoring stays with `add` and `connect`.
- **MoQ between machines (the `spike/moq-gateway` direction), or a publisher inside every app.** It puts a second network protocol on every machine and keeps networking inside graphs. §Networking already reads MoQ as edge I/O.
- **A local Zenoh router on each machine.** With one host per machine, its single session is already the machine's edge. A local router adds a hop and a second owner.
- **Splitting the core by GPU and non-GPU.** The data model is shared, so two cores would duplicate the scheduler and ports and fracture the processor ecosystem. Accelerators become an extension instead.
- **A per-runtime launchd service with an orchestrator sharing surfaces.** This was an earlier setup. It is what `macos-platform-floor.md` rules out, and one host per machine removes the need for it.

## Consequences

- **"The engine runs once per process" becomes "once per machine"** in hosted mode. Embedded mode keeps the old shape for development and tests.
- **Blast radius.** A host crash takes every graph on the machine with it. The mitigations:
  - keep the host small;
  - processors stay in helper processes;
  - make the GPU and network services restartable without killing graphs.
- **Version skew.** A graph's own environment can carry a different build of the native distribution than the host's. Today a helper refuses anything but its parent's exact build, so how the two agree is OPEN.
- **Remote control retires.** Nothing drives a machine's engine from another machine through the control plane any more, and runtimes stop announcing control-plane URLs on the mesh.
- **Migration.** `setup(rt)` apps keep working in both modes. Apps that embed publishers keep working until a rip-out change removes them.
- **The glossary changes.**
  - *Host* gains a new sense, the per-machine daemon. The retired loads-plugins sense stays retired.
  - *Graph description*, *hosted mode*, *embedded mode*, *local API* and *stream map* are added.
  - *Node*, *App-process*, *Control plane* and *Placement* are amended.
