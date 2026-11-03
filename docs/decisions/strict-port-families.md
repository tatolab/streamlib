# Strict port families

Rationale for the `[strict-port-families]` entries in `docs/plan/ARCHITECTURE.md` §Processor
model & scheduling and §Product, decided 2026-11-03. Partially supersedes
`schema-free-ports.md` and `data-plane-cast-not-contract.md`: the port's family is now compared
at `connect`. Class identity, no codegen, no schema registry and no versions stand.

## Trigger

Read this before adding a sixth family, before letting `connect` insert a converter, before
moving a payload between GPU and CPU without a node the author placed, before giving Records a
shared versioned schema, and before putting msgpack back into a user-facing call.

## Decision

Every port declares one of five families: **Image**, **Array**, **Channels**, **Packets** or
**Records**. `connect` compares families by equality and refuses a mismatch, naming both sides.
Inside a family, names stay open (channel names, attribute names, record keys, codec strings);
an input may require names (`requires=`), checked at `connect` against the output's declared
names. Conversion between families is always an explicit node. Each family has one
engine-owned memory layout, carried as a fixed `#[repr(C)]` descriptor in the iceoryx2 user
header, so reads borrow the sample in place and writes loan the destination. GPU pool slots are
imported once per consumer and claimed through a shared-memory claim table. msgpack leaves user
code: Records takes plain dicts and encodes them itself.

## Why

- **A foundation of primitives, not a loose system.** Holoscan and TouchDesigner each build on a
  small, clearly defined set of data types rather than bytes or negotiated capabilities. The
  set is the foundation everything else composes on, and the families are ours.
- **No accidental slow paths.** The trade is that a person or an agent converts explicitly. In
  return, no conversion is ever automatic or implied, so a developer can never create a
  non-optimal path by accident: every conversion is a node someone placed and can see.
- **Agents and editors need something to wire against.** With bytes alone, nothing in the
  graph says whether two ports fit, and a mismatch surfaces as a decode failure deep in a
  consumer. One family tag compared by equality is the least type system that makes a graph
  checkable before a frame flows.
- **Limited types, as TouchDesigner and Holoscan do.** Five families cover video, tensors and
  point data, audio and simulation signals, encoded media, and events. Flexibility comes from
  uniform contracts inside a family, open names, and conversion nodes, not from negotiation.
- **Zero copy needs a known layout.** A payload the engine cannot read cannot be borrowed in
  place. A fixed descriptor per family lets Python read through the buffer protocol and DLPack,
  and lets an encoder write straight into shared memory.

## Rejected alternatives

- **Capability negotiation between ports** (GStreamer caps) — rich, but every link becomes a
  search, and the result is hard to predict for an author or an agent.
- **Engine-inserted converters** (Holoscan's `auto_insert_converters`) — the cost of a
  conversion becomes invisible, and a GPU frame can silently become a CPU copy.
- **Versioned shared record schemas** — brings back the shared-class coupling, codegen and
  registry `schema-free-ports.md` removed.
- **A bounded `Opaque` family as the escape hatch** — Records already is one; a second
  invites everything to route around the families.
- **Audio as its own family** — planar named samples at a rate is the same layout as FMU
  signals and control values, so Channels covers all three.

## Consequences

- Every built-in port and every `MsgpackCast` in the tree migrates in the same work, the
  first-party modules in `packages/` included. Records can carry a node not yet converted.
- The descriptors are a cross-language ABI across Python, Rust and the module host table,
  and each carries a layout regression test.
- A held borrow pins a shared-memory slot; borrows per port are bounded, and past the bound
  the link drops and counts, so a slow reader costs only itself.
- The claim table's cross-process atomics need a written memory-ordering argument and
  crash-reclaim tests; a mistake serves recycled pixels.
- Records is the one family with no brake; built-ins never publish Records for data that fits
  another family, and the catalog marks a Records port as unchecked.
- `ImageToArray` has to be zero-copy where the layouts match, or authors route around it.
- A known layout per family gives every port a preview, so the machine's owner can see any
  port on the machine, exposed or not, as TouchDesigner shows every wire. This reverses the
  2026-10-17 rule that an internal port is seen only through its stream's logs; a preview stays
  on the machine and never reaches a relay off it.
