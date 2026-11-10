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

- Landed: #3235 (PR #3242).

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

- Landed: #3213 (PRs #3244 and #3245). #3215's read path landed (PRs #3248 to #3252, #3255
  and #3286) and Tato Studio's per-family views landed in tatolab-app #166; left are stream
  facts in the app (tatolab-app #170) and the Image rig preview.

## 4. Remove the automatic path

Change: [`changes/gateway-nodes.md`](changes/gateway-nodes.md), ticket 2.

- #3214: the exposed-port publisher, exposure and `expose`, the bundled local relay, the relay
  verbs and the preview page leave the tree. It waits on #3213, because the rigs move onto
  gateway nodes, and on #3215, so previews have their replacement before the old page goes.
- Done when the removal gate passes on every `REMOVED:` bullet of the change and
  `/ship-change` has folded it.
- Landed: #3214 (PR #3262). The `/ship-change` waits on #3215.

## 5. Projects and assets

Change: [`changes/project-and-assets.md`](changes/project-and-assets.md).

A project becomes a versioned repository of assets with its own history, a stream's graph is one
asset, every asset and node gets a runtime-generated UUID, every change is a transaction, and
another project's assets are linked dependencies. No `stream.py`. #3256 builds new code beside
the stream path and may start now, beside steps 2 to 4; #3257 and the tickets after it wait on
step 4, because `exposed` leaves the spec keys and the stream record there.

Tickets: #3256 (the project), then #3257 (streams as graph assets), then #3258 (transactions),
#3259 (generators as fragments), #3260 (linked dependencies) and #3152 (the project read).

- Landed: #3256 (PRs #3261 and #3263) and #3257 (PRs #3275 and #3285). #3258 is in progress.

[`changes/graph-groups-and-nested-fragments.md`](changes/graph-groups-and-nested-fragments.md)
rides this step: a generator placing a generator joins #3259, and labelled groups in the graph
document, #3308, follow #3258.

## 6. One control API contract

Change: [`changes/control-api-contract.md`](changes/control-api-contract.md), Order step 1.

The operation registry and dispatcher for the first operations, the checked contract file, the
JSON-RPC adapter and the generated TypeScript client in the runtime unit. Tato Studio moves onto
that client, which also fixes its Run and stream rename. It may run beside step 5; the change's
later steps follow it in the change's own order (owner, 2026-11-10).

## 7. New capability: lightweight nodes, then shaders

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

- Landed: #3225, #3228, #3229 and #3232 of item 1, with most of #3224 (PRs #3276 to #3278);
  item 2 in full, #3222 (PR #3272) and #3223 (PR #3288); #3230 of item 3 (PR #3292); #3234 of
  item 4 (PR #3297); and the first of item 5's pool, islands a built-in reads (PRs #3305 and
  #3310).

Step 7 may run beside step 5. #3226 (islands on live edits) and #3227 (pins) wait on step 5's
transaction ticket, #3258, so live edits and pin values are built once, as transactions. Nothing in steps
1 to 6 waits on this step.

## 8. Loop zones and hand-editable containers

Design not yet written. Input: *Loops for function nodes: options* (2026-11-08), whose option the owner has yet to pick. A
zone's body is a fragment run N times, and a zone drawn by hand needs a container a person edits,
which is also Unreal's Collapse Nodes. Waits on step 5's #3259.
