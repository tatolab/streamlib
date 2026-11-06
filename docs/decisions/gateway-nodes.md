# Gateway nodes: networking is explicit in the graph

Rationale for the `[gateway-nodes]` entries in `docs/plan/ARCHITECTURE.md` §Networking, §Product,
§Packages & extension model, §Processor model & scheduling and §Control plane & observability,
decided 2026-11-06. It supersedes the automatic sharing path of `moq-relays.md`,
`exposure-levels.md` and `stream-interface-and-routing-policy.md`: the exposed-port publisher,
exposure, the routing policy, the supervised local relay and the preview page. The relay-track
reader, the stamp re-basing rule, the catalog-entry layout, the group flag in the envelope and
"Tatolab solves no networking problem" are unchanged.

## Trigger

Read this before adding anything that sends a port off its stream without a node the author
placed: a publisher, an `expose` verb, an exposure level, a routing toggle, a relay the runtime
starts on its own, or a preview that reads through a relay. Read it too before putting a relay
adapter, a MoQ crate or a relay credential type back into `tatolabd`.

## Decision

Nothing leaves a stream on its own. A stream's data reaches a network only through nodes its
author added, in the Unreal Engine shape for WebRTC and media: a **gateway node** holds one
connection to a relay; a **track node** declares one outgoing track on a gateway, with its catalog
entry, and the stream holds it as a graph variable; elsewhere in the graph a **track send node**
writes content to that track by reference, and a **track receive node** reads a remote track
through a gateway. The nodes live in a module (`tatolab-moq`), built the way a third party
builds one; the runtime carries no relay adapter, no publisher, no local relay and no MoQ crate.

Previews come straight from the runtime. Tato Studio's viewers show any port of any node on the
machine by its family — Image, Array, Channels, Packets, Records — without a relay, an encoder or
an exposure in between. `expose`, `Exposure`, `expose_port`, the routing policy, the preview page
and `/watch` go.

## Why

- **The graph says everything the stream does.** An automatic publisher made a stream's network
  behaviour depend on state outside its code — exposure rulings, a toggle per relay, which relays
  the machine had joined. With gateway nodes, reading the graph tells you exactly what goes where.
- **Prior art.** Unreal Engine's media and WebRTC paths, Blueprint object variables, TouchDesigner's
  network operators and Houdini's object references all make a connection an object the graph
  holds and other nodes refer to, rather than a side channel the engine runs.
- **One core system per concern.** Networking was spread across the engine (exposure, the
  reserved subscriber slot), `tatolabd` (publisher, adapters, secret store, relay supervision), the
  CLI, the API server and a web project. As a module it is one package with one owner.
- **Previews never needed a network.** Reading a port on the same machine through an encoder, two
  loopback hops and a decoder cost about ten milliseconds and a lossy codec, and limited previews to
  what MoQ players can draw. A runtime viewer per family shows tensors, channels and records too.

## Rejected alternatives

- **An exposed-port publisher in the runtime** — sends data the graph does not show.
- **Exposure levels or a routing toggle** — policy outside the graph, in the runtime's records.
- **A long data wire from each sender to its gateway** — the graph variable is what keeps the
  connection reusable without wiring every sender across the graph.
- **Keeping the bundled local relay** — an always-supervised relay is the automatic behaviour this
  removes; a person or a rig that wants one runs `moq-relay` and points a gateway at it.
- **Previews through the local relay** — an encode and a decode for a picture on the same machine,
  and nothing for families a MoQ player cannot draw.

## Consequences

- The runtime ships no built-in node: `RelayTrackSource` moves into the module as the track
  receive node, and `_built_in_nodes.py` writes no class.
- Proofs that read a port as a user does — the codec rigs, the audio checks, the stream-on-runtime
  suite — add a gateway and a track send node to a relay the rig starts, then read it with
  `tatolab-read-track`. No engine door is added for them.
- One stream reads another only through gateway nodes. Inside a stream, nodes still share memory.
- Credentials leave `tatolabd`'s compiled adapters. Where they live, how a node refers to a track,
  how a subgraph declares its ports and how a viewer reads a port are settled in the change
  `docs/plan/changes/gateway-nodes.md`.
