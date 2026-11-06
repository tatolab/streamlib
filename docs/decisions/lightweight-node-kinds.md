# Lightweight node kinds

Rationale for the `[lightweight-node-kinds]` entries in `docs/plan/ARCHITECTURE.md` §Processor
model & scheduling, decided by the owner 2026-11-06. Amends `helper-process-placement-only.md`:
a Python class node still runs in its own process, and nodes of other kinds no longer need one.

## Trigger

Read this before giving a node a process or a thread it does not need, before adding a node
kind, before adding a placement setting or a dial to a node, before writing a second run loop,
and when someone asks why `Lerp` is not a process.

## Decision

A node has one of three kinds, declared by a contract about what its code does: a
**shader**, a Slang function from Images to an Image; a **function** node, written in Python
(`@node.function`) or Rust (`#[function_node]` in `tatolab-module-sdk`), that computes its outputs from
its inputs with time and state handed in; or a **class** (`@node`) that may do anything. Every
kind has the same ports, families, links and `type`; the runtime derives where each runs. The
compiler groups connected shader and function nodes into evaluation islands, where a link is a
function call. Function nodes run inside the read of the node that reads them; shaders run on one
native evaluator per stream, and stateful function nodes feeding several readers in one helper per
stream. A class runs in its own process, and every helper runs the engine's one Rust run loop.
Numeric Python in a function node is compiled to native code by Numba, so the runtime's own small nodes
(`Lerp`, `Lfo`) are ordinary `@node.function` functions in the bundled module `tatolab-ops`, written
the way a user writes one.

## Why

**The cost is the interpreter and the hop, not the work.** A process starts in about 0.5 ms;
CPython takes 7–50 ms to boot and 3–10 MB of private memory, and every message pays a process
crossing. For a node whose work is one multiply, that is all overhead.

**Every comparable editor compiles small nodes into calls.** Unreal Blueprint runs pure
functions where their data is needed and runs PCG kernels on the GPU as one fused operation;
TouchDesigner cooks a chain in one process; Houdini cooks in-process, out-of-process or in a
pool. One mode was where StreamLib started, not where it stops.

**The cheapest host already exists.** Nearly every node that reads data runs in a helper, and
every helper reads through one Rust seam with the GIL detached. Calling a compiled function node
inside that read needs no new process, thread or scheduler, which is why they ship first.

**Islands, not lighter processors.** Green threads or a rayon pool make many processors cheaper
to schedule; islands remove the scheduling. The empty execution-flavor components are removed
rather than kept beside them.

**One run loop.** Reactive, continuous and pacing existed in Rust and again in the Python
bootstrap. Island wake rules would have had to be written twice; running the engine's runner in
the helper writes them once.

**Cheap isolation exists only for code the runtime controls.** No OS or runtime gives arbitrary
CPython with C extensions a crash boundary cheaper than a process. So the kinds draw the line at
what the code may do, not at the language: a class may do anything and keeps a process; a
function with no hidden state may run anywhere. A function node that crashes its reader is
moved to its own process after one restart, found by a breadcrumb written before each call.

**Rules do not dictate architecture.** The process-per-Python-node shape was held by a rule
file, two reviewer hard-fails and a vocabulary gate. Those are retired; the plan is the one
place the architecture is stated, and it changes through the plan.

**One SDK, no special path.** Built-in nodes that a module could not write are how the media
built-ins ended up needing a migration of their own. The runtime's small nodes are a module from
the start, so the ones in the box and a third party's are built, loaded and namespaced the same
way.

**Language is not a kind.** A Rust `Lerp` and a Python one meet the same contract, and the read
seam calls the same C entry for both; Numba-compiled Python measured within 10–15% of native.
A separate "op" kind would have named a language, not a behaviour, and the built-ins written in
Python are the user path's own proof.

## Rejected alternatives

- **Ops as a fixed set in the engine crate** — a path only the engine can use, which modules
  would later need to be migrated onto (owner, 2026-11-06).
- **The built-in ops in Rust** — fast, but written in a way users mostly will not, so the box's
  nodes would not exercise the path users take.
- **Ops as a fourth kind beside function nodes** — the same contract under a second name, which leaves an
  author unsure which to reach for (owner, 2026-11-06).
- **A separate control-rate graph with its own value ports** — it reintroduces the two-graph
  problem (Blueprint beside Material, visual scripting beside shader graph).
- **A placement setting on each node** — it names where a node runs, which becomes wrong the day
  a pool or promotion moves it; a contract stays true and an agent can check its code against it.
- **Python function nodes and the pool first** — the costliest piece with the least certain payoff; Rust
  function nodes at the read seam carry the editor's whole math vocabulary with none of its failure
  modes.
- **CPython subinterpreters** — one crash domain, no kill, and no numpy or pyo3 extensions, so
  not StreamLib's own wheel.
- **Rust snippets compiled at run time** — every machine needs the Rust toolchain and each
  compile takes seconds; compiling numeric Python with Numba gives native speed with no new
  language.
- **Wasm for user code** — cheap and killable, but Python in Wasm is several times slower with no
  numpy; function nodes in Rust or Python and Slang shaders cover the editor's node list.

## Consequences

- An island's semantics are the runtime's to define: sourceless islands never wake a reader,
  inputs hold their latest value, cycles pass through a delay node, and links inside an island are
  off the step ledger and cannot drop.
- State lives on a per-stream board keyed by node name, so it survives a restart, a re-island and
  a reload, and can be shown.
- A function node's time and state are parameters, so evaluation is reproducible in stepped mode.
- Purity is linted at decoration but cannot be enforced in Python; a function that cheats computes
  wrong values without crashing anything outside its interpreter.
- One crash of a reader is the accepted cost of finding a crashing function node.
- Shader nodes need asynchronous GPU submission before they scale across streams.
- The editor gets two signals: wire colour from the family, node style from the kind.
