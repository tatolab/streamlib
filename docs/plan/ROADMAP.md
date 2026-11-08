# Roadmap: everything through the graph

The order the active changes land in, set by the owner on 2026-11-06. Every session picking
the next piece of work reads this first. A step starts when the step before it has landed,
unless its entry says otherwise. Changing the order is the owner's call.

**The goal.** A stream is a graph and nothing else, as in TouchDesigner, Houdini or Unreal
Blueprints. Every piece of data flows along a link someone drew, between nodes someone placed.
The runtime does nothing on its own: no automatic publishing, no hidden relay, no preview path
beside the graph. What a stream does is what its graph shows.

**Why this order.** Everything waits on the data types. A graph variable can only carry a
typed port, a gateway node holds its track through a variable, and the automatic path can only
go once the nodes that replace it exist. New capability (lightweight nodes, shaders) comes last
because nothing above waits on it.

## 1. Finish the data types

Change: [`changes/archive/2026-11-08-strict-port-families.md`](changes/archive/2026-11-08-strict-port-families.md),
shipped and folded into `ARCHITECTURE.md`. Milestone: Strict port families.

Every port has one of five families (Image, Packets, Records, Channels, Array), and `connect`
refuses a mismatch before a frame flows.

- Landed: #3197 (families and the connect check), #3198 (conversion nodes), #3202 and #3204
  (typed reads and writes per family), #3210 (Rust test nodes), #3211 (audio window stage),
  #3199 (the engine tree; `VideoFrame` becomes `ImageFrame`, `AudioBlock` becomes
  `ChannelsBlock`), #3200 (every module in `packages/`, PR #3238), #3201 (the decorator ports
  and `MsgpackCast` deleted, PR #3241), and the macro's refusal of a family-less Rust port with
  a port's family required end to end (PR #3273).

## 2. Graph foundations: variables and reference edges

Change: [`changes/lightweight-node-kinds.md`](changes/lightweight-node-kinds.md) §Graph
variables and reference edges. Ticket: #3235.

`SetVariable` and `GetVariable` as Blueprints have them, and the reference edge: a node's
config naming another node, recorded in the graph and checked at load. A variable carries only
the five families, so this waits on step 1 in full, through #3201. It is independent of that
change's phases 1 to 4.

## 3. Graph replacements for expose and preview

Change: [`changes/gateway-nodes.md`](changes/gateway-nodes.md), tickets 1 and 3.

- #3213: the `tatolab-moq` module (gateway, track, send and receive nodes) and the
  `GraphInput` / `GraphOutput` interface nodes. Waits on step 2.
- #3215: runtime viewers. Tato Studio reads any port from the runtime and shows it by family,
  with no relay in between, reading the port's channel and importing an Image surface
  zero-copy (decided 2026-11-07). It does not wait
  on #3213, so it may run beside it.
- [`changes/typed-stream-facts.md`](changes/typed-stream-facts.md): #3284, a port's stream facts
  as typed family fields and hang out of the engine, lands with #3215 so viewers and Properties
  show them; its second ticket, the Packets unit without hang's container, may run beside it.

## 4. Remove the automatic path

Change: [`changes/gateway-nodes.md`](changes/gateway-nodes.md), ticket 2.

- #3214: the exposed-port publisher, exposure and `expose`, the bundled local relay, the relay
  verbs and the preview page leave the tree. It waits on #3213, because the rigs move onto
  gateway nodes, and on #3215, so previews have their replacement before the old page goes.
- Done when the removal gate passes on every `REMOVED:` bullet of the change and
  `/ship-change` has folded it.

## 5. Projects and assets

Change: [`changes/project-and-assets.md`](changes/project-and-assets.md).

A project becomes a versioned repository of assets with its own history, a stream's graph is one
asset, every asset and node gets a runtime-generated UUID, every change is a transaction, and
another project's assets are linked dependencies. No `stream.py`. #3256 builds new code beside
the stream path and may start now, beside steps 2 to 4; #3257 and the tickets after it wait on
step 4, because `exposed` leaves the spec keys and the stream record there.

Tickets: #3256 (the project), then #3257 (streams as graph assets), then #3258 (transactions),
#3259 (generators as fragments), #3260 (linked dependencies) and #3152 (the project read).

## 6. New capability: lightweight nodes, then shaders

Changes: [`changes/lightweight-node-kinds.md`](changes/lightweight-node-kinds.md) and
[`changes/slang-shader-compiler.md`](changes/slang-shader-compiler.md).

1. Function nodes at the read seam, phase 1 of lightweight-node-kinds: #3224 (node kinds and
   compiled `@node.function`), #3225 (promotion when a function node crashes or stalls its
   reader), #3226 (islands on live edits), #3227 (pins and the full `tatolab-ops` set), #3228
   (stateful function nodes on the state board), #3232 (stateful fan-out), #3229 (Rust
   `#[function_node]`).
2. Slang replaces shaderc and glslc as the one shader compiler: #3222 (run-time kernels), #3223
   (build-time shaders).
3. Shaders, phase 2, on the Slang compiler: #3230 (asynchronous GPU submission), #3233 (one
   `@node.shader`), #3236 (fusion).
4. Python function nodes that do not compile, phase 3: #3234 (inline in the Python reader),
   #3231 (the side-effect check at decoration).
5. The pool, phase 4: #3237, one pool interpreter per stream for Python function nodes no
   Python class reads. It is required, not optional.

Step 6 may run beside step 5. #3226 (islands on live edits) and #3227 (pins) wait on step 5's
transaction ticket, #3258, so live edits and pin values are built once, as transactions. Nothing in steps
1 to 5 waits on this step.
