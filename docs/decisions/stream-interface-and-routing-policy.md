# A stream's interface in code, its routing as runtime policy

Rationale for the `[stream-interface-and-routing-policy]` entries in
`docs/plan/ARCHITECTURE.md` §Networking, §Processor model & scheduling and §Product, decided
2026-11-03. Supersedes the levels in `exposure-levels.md`. The edge check, a change applied
live with no restart, and a restriction recorded before it changes live all stand.

## Trigger

Read this before adding a level, a network word or a `local_only` flag to `expose`, before
letting a stream's code decide which relay a port reaches, and before adding a second way to
reuse a stream inside another.

## Decision

In code, `expose` declares a stream's interface: the inputs and outputs it offers, with no
level and nothing about networks. The stream-level name is optional and defaults to the port's
own name. `stream_builder.add` takes a `@stream` function as it takes a node class: its extra
parameters are promoted config, its exposed ports are the only reachable ones, and it flattens
at compile with scoped node names. The same function still runs alone as a stream.

Outside code, a runtime-owned routing policy decides what goes where, edited in the app, the
CLI and MCP. Egress publishes every exposed output by default, with per-output toggles and
segments. Ingress binds an exposed input to another stream's output or a remote track, off by
default; for a remote track the runtime inserts the receive node. Relays are destinations over
a transport-neutral manifest of exposed ports; the MoQ catalog is one projection of it. In the
app, the policy lives in the existing Relays settings section, a page per relay.

## Why

- **Code says what a stream is; the machine says where it goes.** The same stream runs on a
  laptop, a render box and CI. Which relay it reaches is a fact about the machine and its
  owner, the way Docker's `EXPOSE` declares a port and `-p` publishes it.
- **Levels leaked policy into code.** `PRIVATE` and `PUBLIC` in a stream's function, and a
  `local_only` veto, made the author decide something only the machine's owner knows.
- **Subgraphs are the reuse unit.** Exposed inputs and outputs are exactly a subgraph's
  ports, so one `expose` serves both the network and composition. Prior art: Houdini Digital
  Assets, GStreamer bins with ghost pads.

## Rejected alternatives

- **Exposure levels in code** (`internal` / `private` / `public`) — policy in the wrong place.
- **A `local_only` veto in code** — the same leak in the other direction.
- **Stream-to-stream links that cross a stream's edge** — subgraphs and ingress binding cover
  the real need without a second wiring model.
- **A compose-style project file now** — later, if asked for; it would only seed the policy.

## Consequences

- `Exposure`, its levels and `expose_port {level}` leave the stream package and the local API.
- Ingress opens a door into a running stream, so whether it needs its own consent step is
  left OPEN.
- Nesting and which installed `@stream` functions are addable are left OPEN.
- Each transport beyond MoQ is one adapter over the manifest.
