# lightweight-node-kinds

> **Nodes that are not processes**, decided by the owner on 2026-11-06. An editor that people
> and agents program needs dozens of tiny nodes, such as `mul`, `lerp`, `clamp`, an LFO or a
> compare, and a process per `add` is the wrong price. Unreal Blueprint and PCG, Houdini PDG,
> TouchDesigner, Blender, DaVinci Resolve and Effect House all run nodes in more than one mode;
> this change gives StreamLib the same. The owner retired the placement rule the same day:
> rules do not dictate architecture, and a process per Python node was the system as first
> built, not a limit on how it grows. Research: the owner's "Lightweight nodes — research
> handoff" and the "Lightweight node kinds — viability study" (both 2026-11-06).

Today every node is a processor. A Python `@node` class gets its own child process, and every
native processor gets its own OS thread. There is nothing smaller.

**After this change:**
- **One graph, three node kinds.** Every node has the same ports, families, links and `type`.
  What differs is its kind, declared by a contract about what its code does; the runtime
  derives where it runs. No placement surface and no dial.

  | Kind | Author writes | Contract | Runs |
  | --- | --- | --- | --- |
  | shader | a Slang function, `@node.shader` | Images and values in, an Image out, no side effects | on the GPU; a chain fuses into one kernel |
  | function | a function in Python (`@node.function`) or Rust (`#[function_node]`, `tatolab-module-sdk`) | outputs from inputs only; state and time are parameters | inside the node that reads it |
  | process | a class with a lifecycle, `@node.process` | anything: devices, network, files, threads, GPU | its own process, as today |

- **Two contracts underneath.** Shader and function nodes have no side effects: they use only
  their inputs, config, `now` and, if they declare one, their own `State`. A function node with
  no `State` is stateless (`Lerp`, `Add`); one that declares it is stateful (`Lfo`, `Lag`,
  `Random` with its seed in state), receiving the last state and returning the next, which the
  runtime keeps. State the runtime holds is not a side effect, so a stateful node still replays
  the same. A class may have side effects: anything the runtime does not hand it.
- **Which to reach for:** pixels in and an Image out is a shader. Computing from inputs (maths,
  logic, a join, a curve, a custom lerp of numbers) is a function node, written in Python; a
  numeric body is compiled to native code for you, and Rust is there for code outside that
  subset that still needs native speed. Touching the world, or holding a resource across calls,
  is a class.
- **Language is not a kind.** Every function node meets one contract, and underneath the read seam
  calls one thing: a C function pointer with a fixed signature. Numba-compiled Python and Rust
  are two ways to produce it. Only where a node can run differs: a compiled node runs inside any
  reader; Python that does not compile runs inside a Python reader, or the pool in phase 4.
- **The runtime's own small nodes** (`Lerp`, `Lfo`, `Clamp`, …) are ordinary `@node.function`
  Python in the bundled first-party module `tatolab-ops`, written the way a user writes one and
  compiled to native code.
- **The rule an agent needs:** a class is a process; a function node or a shader runs wherever
  the runtime puts it, and never gets a process of its own unless it crashes its reader.
- **The kind travels in the graph and the catalog**, so an agent reads it and an editor draws it.
- **One run loop.** A helper runs the engine's own Rust runner and calls Python's `process()`
  in-process, so reactive, continuous and the island wake rules are written once.

**Scale gate — this skill plus an ADR.** It changes the processor model and the Python API's
public contract. ADR: `docs/decisions/lightweight-node-kinds.md`.

**Sequencing.** Phase 1 needs the families' typed descriptors (`strict-port-families`, merged
in `3e7e58b`). The SDK phases land after that change's decorator and scaffold tickets, because
both rewrite the port surface. Phase 2 builds on `slang-shader-compiler`. Graph variables carry
only the five families, so they wait for `strict-port-families` to land in full, through its
contract ticket that deletes the decorator ports; `gateway-nodes`' module ticket waits on them.
The order is: the families, then variables, then the gateway and track nodes.

## Current state (recon, 2026-11-06, `main` `6e3fee9`)

- **Python nodes.** Each is a `ProcessorInterpreterSpawnHostProcessor` in Manual mode
  (`processor_interpreter_spawn_host.rs:1361`) that only starts and supervises the child. The
  child's bootstrap re-implements reactive and continuous in Python (`_run_reactive`,
  `_run_continuous`, `_processor_interpreter_bootstrap.py:1470-1580`). Native nodes use the
  Rust runner (`thread_runner.rs`).
- **Where readers run.** Every module's native half runs in its node's helper, and `tatolabd`'s
  one built-in, `RelayTrackSource`, has no inputs (`ARCHITECTURE.md:539-549`). Nearly every
  graph of small nodes therefore ends at a helper.
- **The read seam.** `ctx.inputs.read` is Rust in the wheel: it calls
  `InputMailboxesInner::read_raw` with the GIL detached (`python_processor_link_data_access.rs`,
  `iceoryx2/input.rs`). Module native halves read through the same seam from native threads.
- **The groundwork is empty.** `ExecutionLightweightComponent`, `ExecutionRayonPoolComponent`
  and `ExecutionMainThreadComponent` are unit structs nothing uses
  (`core/graph/components/execution_*_component.rs`).
- **The compiler** is incremental and per node: plan, PREPARE, SPAWN, READY, WIRE, CONTINUE
  (`compiler.rs`, `compilation_plan.rs`). The wiring envelope's input entry assumes one node
  per helper (`open_iceoryx2_service_op.rs`).
- **GPU.** `GlslPixelEffect` wraps one single-input GLSL `effect` in a compute kernel inside a
  Python processor and escalates its dispatch. Every compute dispatch takes one recorder mutex
  and waits on a host fence, shared by all streams (`gpu_context.rs`).
- **Authoring.** `node` accepts classes only (`_node_declaration.py`), `add` refuses non-types,
  and `stream_builder.add(f)` type-checks for a zero-argument function and fails at run time.

## ADDED

### Node kind

- Every graph node and catalog entry carries `kind`: `shader`, `function` or `process`, and a function
  node its `language`, `python` or `rust`.
  `graph` and `/api/registry` render it, beside each port's `family`. Kind and family are
  separate axes: a family says what flows on a wire, a kind says how a node's code runs.
- Linking rules: a shader takes Images and values and returns an Image; a function node takes
  values, Records and Arrays and may pass an Image along without reading its pixels; a class
  takes anything.

### One run loop

- A helper's loop is the engine's runner, running in the child and calling the Python
  instance's `process()` in-process. The bootstrap keeps the lifecycle commands, imports and
  describe; it no longer schedules. Every rule below is written once, in Rust.

### The function-node contract

- **One entry the read seam calls:** per function node, a descriptor (ports, families, config,
  state) and a C evaluate function taking the input values, config, `now` and its state and
  returning its outputs. It extends the module host table: the one place the engine calls
  module code, versioned with the table, and loaded only in a helper. No node code, first-party
  or not, runs in `tatolabd`.
- **Two producers.** A `@node.function` Python function whose body is typed numeric code (floats,
  ints, numpy arrays, loops) is compiled by Numba to that entry. A `#[function_node]` Rust function
  in `tatolab-module-sdk` exports it directly, for code outside Numba's subset that still needs
  native speed. A Python body that does not compile runs as Python (§`@node.function` in Python).
- **Compilation** happens at load and is cached by a hash of the source, the signature and the
  Numba version, so a reload or a live edit of another node pays nothing; a module ships its
  function nodes pre-compiled in its wheel. `graph` shows whether each function node ran compiled.
- **Compiled state** is a typed record Numba can lower (`State[T]` with `T` a record of numbers
  and arrays); a state outside that keeps the node in Python.
- **Measured** (2026-11-06, one scalar ease function and a 10M-element loop, C `-O2` as the
  native baseline): 2.8 ns a call against 2.4 ns native and 155 ns plain Python; the loop
  within 10% of native; 140 ms to compile a scalar function, about 0.5 s an array loop.
- A function node's type is its import path in its module's `tatolab.nodes` entry points, like any
  node, so two modules may both ship a `Lerp`, told apart by import path.

### `tatolab-ops`

- A first-party module of `@node.function` functions, bundled with the runtime, pre-compiled in its
  wheel, and built exactly the way a third party builds one; nothing in the engine knows their
  names (`tatolab.ops:Lerp`). It carries:
  - Math: `Add`, `Subtract`, `Multiply`, `Divide`, `Power`, `Minimum`, `Maximum`, `Absolute`,
    `Modulo`.
  - Shaping: `Lerp`, `Clamp`, `Remap`, `Smoothstep`, `Step`.
  - Logic: `Compare`, `Switch`, `Select`, `Gate`.
  - Values and noise: `Constant`, `Random`, `PerlinNoise`.
  - Time: `Lfo`, `Timer`, `Lag`, `Trigger`, `Counter`, `SampleAndHold`.
  - Images: `HoldFrame` (keeps one frame until told to refresh), which passes an Image along
    without reading its pixels and so needs no compiling. Loading an image from a file touches
    the filesystem, so `ImageFile` is a class node in the same module.
- `Select` picks a value; `Gate` passes or blocks messages, so a reactive node behind a closed
  gate does not run. That is the graph's if/else.

### Evaluation islands

- The compiler groups each connected run of shader and function nodes into an **island**.
  Inside an island a link is a function call; at its edge a link is an ordinary channel with
  its family. The graph stays flat: an island is a compile result, never a container.
- **Where an island runs:**
  - **Function nodes read by a helper** run inside that helper's read: an island output is a
    synthetic port in `InputMailboxesInner`, and a read evaluates it. No process, no thread, no
    processor instance. A compiled function node runs inside any reader's read, a module's native
    half included.
  - **Shaders** run on one native evaluator per stream in `tatolabd`, which owns the device,
    and publish on a channel. A shader's code is Slang the engine compiles, never module code.
  - **A stateful function node that feeds several readers** runs in one function-node helper per
    stream, spawned only when the stream has such a node, and publishes on a channel. One
    state never has two copies.
  - **An uncompiled Python function island read by a module's native half** runs on the pool
    (phase 4) or the
    port is refused by name, since that read holds no GIL.
- **The island pass** runs between plan and PREPARE: union-find over lightweight nodes, a host
  per island, cycle refusal. It re-runs on every live `connect` and `disconnect`, touching only
  the islands the edit changes. Island nodes skip PREPARE and SPAWN.
- **The wiring envelope** gains an island payload for the reader: the island's node DAG, its config and
  the upstream channels the reader now subscribes to.
- This settles the execution-flavors OPEN: the flavor is derived from the kind, and no node
  declares a dial.

### Island semantics

- **Waking.** An island with no source upstream, such as a lone `Lfo`, never wakes its reader;
  it is sampled whenever the reader runs. A reactive reader fed only by sourceless islands is
  refused at `connect`, naming continuous or interval execution as the fix.
- **Latest values.** An island holds the last value of each upstream input. A read returns
  `None` only before the first value.
- **Pairing.** Several inputs pair as the latest of each; the node re-evaluates when any one
  changes. See [NEEDS DECISION 1].
- **Evaluation count.** Once per change of inputs or timestamp, memoised, so two readers of one
  `Random` agree. See [NEEDS DECISION 2].
- **Time.** `now` is the reader's `ctx.time` at the read, in integer nanoseconds on the
  stream's monotonic clock. Reproducible in stepped mode; read-time in real-time mode.
- **Cycles.** The island pass refuses a cycle unless it passes through a delay node (`Lag`,
  `SampleAndHold`), which reads the last evaluation's state.
- **Literal pins.** An unlinked input takes the value set on its pin, else its default
  (§Inputs and config). See [NEEDS DECISION 3].
- **A control value on a wire.** An island edge carrying one value is Channels with an
  irregular control rate: one sample per message, rate unset. See [NEEDS DECISION 4].
- **The step ledger** leaves links inside an island off its balance; edge links count as today.
- **Loss.** A function-call link cannot drop; edge links take loss slots on the hosting
  helper's board, so no loss is silent.

### Inputs and config

- **The rule.** A value that may change while the stream runs is an input. A value whose change
  means tearing something down and rebuilding it (a device, a resolution, a file to load, a
  codec, a model) is config. A tint's colour is an input; a camera's resolution is config.
- **Every input of every kind has a pin value.** An unlinked input takes the value set on its
  pin, else its declared default; with neither it must be linked, refused by name at `connect`.
  The graph records pin values per node beside its config, `graph` shows them, and an edit to
  one is live and costs nothing. Function and shader inputs take their default from the
  parameter's; a `@node.process` input declares one on its port
  (`Input(Channels, default=…)`).
- **Config is a hint, not a wall.** The editor shows a config field in the node's panel, and
  "expose as pin" on one node turns that field into an input for that node. A linked config
  field on a function or shader node replaces the config value for that evaluation; on a
  process node each new value arrives as a live config update, validated against the schema as
  any update is, so a costly field stays costly.

### The state board

- Each stream has a board of fixed-size state slots, modelled on the loss-count board: the
  parent creates it, the host writes it. A stateful function node keeps its state there,
  keyed by node name, so it survives a helper restart and a re-island, is persisted in the
  stream record on reload, and `graph` can show it.

### `@node.shader`

```python
@node.shader("""
float4 blend(ImageInput a, ImageInput b, float amount)
{
    return lerp(a.pixel, b.pixel, amount);
}
""")
def blend(a: ImageFrame, b: ImageFrame, amount: float) -> ImageFrame: ...
```

- The function's signature declares the ports in the read types `strict-port-families` added;
  its body is a Slang function named for the node, compiled by `slang-shader-compiler`. An
  Image parameter arrives as `ImageInput`: `pixel` is this pixel, and `sample(uv)` reads any
  position and marks the node as neighbourhood-sampling. A float is a push constant.
- The source reaches `tatolabd` through describe, so no Python runs at evaluation.
- **Fusion.** A chain compiles into one kernel: each node is a Slang type meeting one node
  interface, and the chain is a generic composition of them, which compiles to the same code
  as a hand-fused function. A chain splits, storing the intermediate, where a downstream node
  samples beyond its own pixel, where extent or format changes, where a non-shader node or the
  island's edge reads the intermediate, where it fans out, or where merged push constants pass
  128 bytes. A split chain is still one submission, with barriers.
- Shader nodes take RGBA Images and refuse others by name; a conversion node goes in front.
- **Prerequisite:** dispatch moves from the shared host fence to asynchronous submission with
  timeline signalling, so one stream's shaders do not serialise every other's.

### `@node.function` in Python

```python
@node.function
def remap(x: float, low: float = 0.0, high: float = 1.0) -> float:
    return low + x * (high - low)

@dataclass(frozen=True)
class SmoothedValue:
    value: float

@node.function
def smooth(x: float, seconds: float, now: Time, previous: State[SmoothedValue]) -> Stateful[float, SmoothedValue]:
    ...
```

- **The signature is the node**, read once at import into the catalog, so the editor draws it
  without running it. A plain parameter is an input pin, and its default the pin's value when
  nothing is wired; with no default the input must be wired, and `T | None = None` is optional.
  The return annotation is the output, named `result`; a dataclass return gives one output per
  field. `float`, `int` and `bool` are one-channel Channels, `dict` is Records, and a read type is
  its family.
- **`config`** is one dataclass named by the parameter called `config`, exactly as a
  `@node.process` `__init__` names it: its JSON Schema is published in the catalog, `add(f,
  config={...})` sets it and an update changes it live. Its fields are settings, never pins.
- `Time` and `State[...]` are found by type in any position, and are never pins; they follow a
  bare `*`, since Python refuses a parameter without a default after one with a default.
- **Refused at import, by name, with the fix:** a class (that is `@node.process`), a parameter
  without a type, `*args` or `**kwargs`, a `State` without a matching `Stateful` return or the
  reverse, a lambda, a closure or a nested function, `async def` and generators, and `-> None`.
  A prototype reader is 151 lines with every refusal.
- A ParamSpec decorator keeps the call signature, so a test calls it.
- `Time` is integer nanoseconds. `State[T]` comes in and `Stateful[Out, T]` names what goes out,
  so the state is visible in the type. The state lives on the board.
- `add` takes a function through its own overload, and the zero-argument hole is closed.
- **The side-effect lint** runs at decoration, an `ast` reader in the shape of `_cross_floor_check.py`.
  It refuses by name calls to `time.*`, `random.*`, `open`, `socket`, `threading`,
  `os.urandom` and `datetime.now`, `global` and `nonlocal`, closures, and referenced mutable
  globals. It cannot see aliased, dynamic or transitive access; a function that cheats computes
  wrong values but cannot crash anything outside its interpreter. A Rust function node is held to
  the same contract by review, not a lint.
- A body that compiles runs as native code (§The function-node contract); the rest of this section
  is how a body that does not compile runs.
- **A crash breadcrumb.** Before each function-node call, in either language, the helper writes the node's
  name to its board.
  When a helper dies with the breadcrumb set, its restart moves that node out to its own
  process: promotion on crash, counted under `metrics` in `graph`. One crash of the reader is
  the cost. See [NEEDS DECISION 5].

### The pool and promotion (phase 4)

- Built only if real graphs carry Python function nodes that no Python class reads. One pool interpreter
  per stream hosts such nodes as plain calls. A function node that runs past its island's frame
  interval three times running is promoted to its own process; a hung call cannot be
  pre-empted, so the pool is respawned with state handed over through the board.

### Graph variables and reference edges

This is the one definition of graph variables and of the reference edge. `gateway-nodes` uses
both and defines neither.

- **A reference edge.** A node's config value may be another node's reference, the value `add`
  returned (`MoqTrackSend(track=camera_track)`). The graph records it as `{"node": "<name>"}` in
  that config. The load refuses a reference to a missing node or to a node of the wrong type, by
  name, and `graph` renders reference edges beside the links. A reference edge carries no data
  and is never a link: the step barrier, delivery profiles, link counters and the island pass
  never see it.
- **`SetVariable` and `GetVariable`**, as Unreal Blueprints have them. `SetVariable` takes one
  value under a name; any number of `GetVariable` nodes read it anywhere in the stream with no
  wire drawn. A variable has no runtime of its own and holds nothing at run time:
  - **A port's output.** `SetVariable` takes one input. The compiler resolves each get to a link
    from the setter's source, so the variable keeps that port's family and the get's readers are
    checked against it as any link is. The family is one of the five, Image, Packets, Records,
    Channels or Array, from typed descriptors; a decorator port has no family a variable can
    keep, so a variable needs the decorators gone.
  - **A node reference.** `SetVariable` takes a node reference in config, and each get resolves
    to a reference edge from the reader to that node. A connection is held this way, as a
    reference to its gateway node, and a track the same way; neither is ever a record on a link.
  - A get with no setter, two setters of one name, or a get read as the wrong family or node type
    is refused by name at `connect`.
- **Inside an island** a resolved variable is the link it resolved to, so a get feeding a function node is
  a function call like any other.

### Viewers

An island holds its latest values, so a viewer of a lightweight node's output reads them through
whatever read path `gateway-nodes` settles, at no process cost. Relays, gateways, tracks and
previews are class nodes a person places, never function nodes, because they touch the network or
the device.

### What Tato Studio reads

- `kind` and `family` on every `graph` node and port, from the real runtime.
- A palette: category per node type, every installed module's function nodes, `tatolab-ops`
  included, and a project's shader and function nodes listed without running their code.
- Where each lightweight node runs: its host node, its fused group, promoted or not.
- The source-writing edit verbs accept shader and function types and write an idiomatic
  `stream.py` line.
- Log records and state attribute a lightweight node by its own name, not its host's process.

## MODIFIED

- **The placement entry** (`ARCHITECTURE.md:1529-1546`). It reads: a Python **class** node runs
  in its own child process, exec'd from the stream's venv, never fork; shaders and compiled
  function nodes run without a process; a Python function node that does not compile runs inside its Python reader, in a pool only in phase 4, and
  in its own process after a crash or a stall. "No processor may ever block another" holds
  between class nodes.
- **The class-identity entry** (`ARCHITECTURE.md:1732`). Bare `@node` is retired: every node
  names its kind, `@node.shader`, `@node.function` or `@node.process`, and a module's
  `#[function_node]` is a node type too; a function's type is its import path, as a class's is.
- **The execution-flavors OPEN** (`ARCHITECTURE.md:1775-1777`) becomes DECIDED as evaluation
  islands.
- **The built-ins entry** (`ARCHITECTURE.md:539-549`). `tatolab-ops` is a first-party module of
  function nodes, bundled with the runtime and built on the SDK any module uses, so the runtime
  still ships no node of its own.
- **The module host table entry** (`ARCHITECTURE.md:550-559`) gains the function-node entry, the one
  place the engine calls module code, and only in a helper.
- **`docs/decisions/helper-process-placement-only.md`**: the clauses this change retires are
  struck in place.
- **The port descriptor** gains an input's default (`strict-port-families`' `Input`), and the
  graph spec gains per-node pin values beside config.
- **The variables entry** (`ARCHITECTURE.md:1833-1844`): a variable carries only the five
  families, and the order is the families, then variables, then the gateway and track nodes.
  Done in this PR, from the owner's ruling of 2026-11-06.
- **`gateway-nodes.md`** points at §Graph variables and reference edges instead of defining the
  reference edge itself, and its module ticket waits on this change's variables ticket. Done in
  this PR.

## REMOVED

- REMOVED: .claude/rules/placement.md
- REMOVED: check_no_in_process_placement
- REMOVED: ExecutionLightweightComponent
- REMOVED: ExecutionRayonPoolComponent
- REMOVED: ExecutionMainThreadComponent
- REMOVED: _run_reactive
- REMOVED: _run_continuous

The first two are removed in the PR that carries this proposal, by owner ruling.

## Decisions

Each carries a recommendation that is built if nobody overrules it.

- **[NEEDS DECISION 1] Pairing.** Latest of each, re-evaluated when any changes (TouchDesigner),
  or wait for matching timestamps? *Recommendation:* latest of each; no input waits on another.
- **[NEEDS DECISION 2] Evaluation count.** Once per change, memoised, or once per pull
  (Blueprint)? *Recommendation:* once per change; a per-pull `Random` read twice disagrees.
- **[NEEDS DECISION 3] Pins.** Unlinked inputs take config, else default, or every number is a
  wired `Constant`? *Recommendation:* config, else default, as Blueprint's pins do.
- **[NEEDS DECISION 4] A control value on a wire.** Channels with an irregular control rate, or
  Records? *Recommendation:* Channels, which keeps control values where the plan puts them.
- **[NEEDS DECISION 5] A crash in an inline function node.** Breadcrumb and promotion on crash, or
  always isolate? *Recommendation:* the breadcrumb; it bounds the damage to one restart of the
  reader without a pool.
- **DECIDED 6 — The explicit decorators (owner, 2026-11-06).** Bare `@node` is retired: every
  node names its kind, so a reader or an agent never infers it. `@node.shader`, `@node.function`
  (stateful when it takes `State`) and `@node.process` (a class that may do anything, in its
  own process). Rejected: `@node.stateless` / `@node.stateful`, since what decides a process is
  side effects, not state; and `@node.class`, which Python cannot spell.
- **DECIDED 7 — Inputs and config (owner, 2026-11-06).** A value that changes freely is an input
  with a pin value; config is for what needs a rebuild, and any config field may be exposed as
  a pin (§Inputs and config).

Assumed unless overruled:
- A function's single output is named `result`; plain parameters are pins by default; a node's
  palette category defaults to its module's name, overridden by `category=`.
- `tatolab-ops` is installed in every stream's environment with the runtime, so every helper can
  load it.
- The shader template is the `ImageInput` shape above.
- A material graph is a later change that asks `slang-shader-compiler` for a fragment target and
  adds a renderer node; nothing here precludes it.
- Blueprint's execution wire ("do A, then B") is not part of this change; gates cover streams.
- Numba ships in the runtime's Python environment; the size (about 40 MB with llvmlite) is
  accepted, as Slang's was. Numba supports a new CPython some months after its release; in that
  gap function nodes run as Python, slower, and a native reader of one is refused by name.
- Wasm is out of scope.

## Proof

- `Lfo → Remap → <Python class>` runs with exactly one child process, the class's, beside the
  class case in `tests/stream-on-runtime/test_helper_placement.py`.
- `connect` and `disconnect` re-island live, and a cycle without a delay node is refused by name.
- `Lfo → Remap` from `tatolab-ops` runs compiled, from the wheel's pre-compiled code, and is
  read by a module's native half with no GIL.
- A user's numeric `@node.function` compiles on load, is cached, and matches its Python output; a
  non-numeric one runs as Python, and `graph` shows which.
- A fixture module's Rust function node joins an island beside `tatolab-ops`'.
- A stateful function node keeps its state across a helper restart and a stream reload.
- A stateful function node feeding two readers gives both the same value.
- Two streams' shader dispatches no longer serialise on one host fence.
- One `@node.shader` runs Image in, Image out, with no Python process.
- A three-node point-wise shader chain records one submission per frame, matches the unfused
  output, and a blur in the middle splits it.
- `@node.function` with `State` keeps its state across a reload; the lint refuses `time.time()`,
  `random.random()` and `open()` by name.
- A function node that aborts its reader is promoted after one restart, and `graph` counts it.
- One `SetVariable` of an Image output read by two `GetVariable` nodes yields two links from the
  setter's source and no variable node at run time; a variable holding a gateway reference yields
  reference edges in `graph`; a missing setter, a duplicate setter and a wrong-family get are each
  refused by name.

## Tickets (expected)

1. Compiled function nodes at the read seam:
   1. `kind` in the descriptor, catalog and `graph`; bare `@node` retired for `@node.process`
      across the engine tree; the one run loop; the function-node contract;
      `@node.function` with `Time` and `State` compiled by Numba, with its cache; `tatolab-ops` with
      `Lfo`, `Remap`, `Clamp`, pre-compiled in its wheel.
   2. The island pass on live edits, with cycle refusal.
   3. Pin values on every kind's inputs, expose-as-pin for config fields, and the full
      `tatolab-ops` set.
   4. Stateful function nodes on the state board.
   5. Fan-out of a stateful function node in the per-stream function-node helper.
   6. Rust `#[function_node]` in `tatolab-module-sdk`, proven by a fixture module.
2. Shaders:
   1. Asynchronous GPU submission.
   2. One `@node.shader` on the native evaluator.
   3. Fusion under the split rule.
3. Python function nodes that do not compile:
   1. Inline in a Python reader.
   2. The side-effect lint.
   3. The crash breadcrumb and promotion on crash.
4. The pool and promotion, only if graphs need it.
5. Graph variables and the reference edge, after `strict-port-families`' contract ticket and
   before `gateway-nodes`' module ticket. Independent of phases 1 to 4.

## Notes (not in scope)

- Measure a helper's real cost (wheel import, idle memory, iceoryx2 and Vulkan setup) and the
  per-message hop before phase 3; phase 1 removes the hop for compiled function nodes.
- The shader examples in `examples/` and the scaffold's `HalftoneLensEffect` move to
  `@node.shader` as consumer backlog. Retiring `GlslPixelEffect` is a separate owner call.
- A thread pool for native class nodes is a separate change.
