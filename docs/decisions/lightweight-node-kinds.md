# Lightweight node kinds

Rationale for the `[lightweight-node-kinds]` entries in `docs/plan/ARCHITECTURE.md` §Processor
model & scheduling, decided by the owner 2026-11-06. Amends `helper-process-placement-only.md`:
a Python class node still runs in its own process, and nodes of other kinds no longer need one.

## Trigger

Read this before giving a node a process it does not need, before adding a node kind, before
adding a placement setting or a dial to a node, and when someone asks why `Lerp` is not a
process.

## Decision

A node has one of four kinds, declared by a contract about its code: an **op** the runtime
supplies, a **`@node.shader`** GLSL function, a **`@node.pure`** Python function, or a
**`@node` class**. Every kind has the same ports, families, links and `type`; the runtime
derives where each runs. A class runs in its own process. An op or shader runs with no
process. A pure function runs inside the Python node that reads it, or in one pool interpreter
per stream, and is moved into its own process if it misbehaves.

## Why

**The cost is the interpreter and the hop, not the work.** A process starts in about 0.5 ms;
CPython takes 7–50 ms to boot and 3–10 MB of private memory, and every message pays a process
crossing. For a node whose work is one multiply, that is all overhead.

**Every comparable editor runs nodes in more than one mode.** Unreal Blueprint runs pure
functions where their data is needed and runs PCG kernels on the GPU as one fused operation;
Houdini PDG cooks in-process, out-of-process or in a pool of persistent sessions; TouchDesigner
cooks in-process and offers a separate process as an explicit component. One mode was where
StreamLib started, not where it stops.

**Cheap isolation exists only for code the runtime controls.** No OS or runtime gives arbitrary
CPython with C extensions a crash boundary cheaper than a process. So the kinds draw the line
at what the code may do, not at the language: a class may do anything and keeps a process; a
function with no hidden state may run anywhere, any number of times.

**Rules do not dictate architecture.** The process-per-Python-node shape was held by a rule
file, two reviewer hard-fails and a vocabulary gate. Those are retired; the plan is the one
place the architecture is stated, and it changes through the plan.

## Rejected alternatives

- **A separate control-rate graph with its own value ports** — it reintroduces the
  two-graph problem (Blueprint beside Material, visual scripting beside shader graph).
- **A placement setting on each node** — it names where a node runs, which becomes wrong the
  day a pool or promotion moves it; a contract stays true and an agent can check its own code
  against it.
- **CPython subinterpreters** — one crash domain, no kill, and no numpy or pyo3 extensions,
  so not StreamLib's own wheel.
- **Wasm for user code** — cheap and killable, but Python in Wasm is several times slower with
  no numpy; ops, pure Python and GLSL cover the editor's node list.

## Consequences

- A pure function's state and time are parameters, so evaluation is reproducible, a reload
  keeps state, and state can be shown and replicated.
- Pure nodes in one pool can stall each other; promotion bounds that, and every promotion is
  counted in `graph`.
- Purity is checked at decoration but cannot be enforced in Python; a function that cheats
  computes wrong values without crashing anything outside its interpreter.
- The editor gets two signals: wire colour from the family, node style from the kind.
