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
  | pure | a function in Python (`@node.pure`) or Rust (`#[pure_node]`, `tatolab-module-sdk`) | outputs from inputs only; state and time are parameters | inside the node that reads it |
  | class | a class with a lifecycle, `@node` | anything: devices, network, files, threads, GPU | its own process, as today |

- **Which to reach for:** pixels in and an Image out is a shader. Computing from inputs (maths,
  logic, a join, a curve, a custom lerp of numbers) is a pure function: Python by default, Rust
  when it is hot or a native node reads it. Touching the world, or holding a resource across
  calls, is a class.
- **Language is not a kind.** A Rust pure node and a Python one keep the same contract. The
  language only changes where it can run: Rust runs inside any reader, Python inside a Python
  reader, or the pool in phase 4.
- **The runtime's own small nodes** (`Lerp`, `Lfo`, `Clamp`, …) are Rust pure nodes in the
  bundled first-party module `tatolab-ops`, built on the same SDK as anyone's.
- **The rule an agent needs:** a class is a process; a function, shader or pure, runs wherever
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

- Every graph node and catalog entry carries `kind`: `shader`, `pure` or `class`, and a pure
  node its `language`, `python` or `rust`.
  `graph` and `/api/registry` render it, beside each port's `family`. Kind and family are
  separate axes: a family says what flows on a wire, a kind says how a node's code runs.
- Linking rules: a shader takes Images and values and returns an Image; a pure function takes
  values, Records and Arrays and may pass an Image along without reading its pixels; a class
  takes anything.

### One run loop

- A helper's loop is the engine's runner, running in the child and calling the Python
  instance's `process()` in-process. The bootstrap keeps the lifecycle commands, imports and
  describe; it no longer schedules. Every rule below is written once, in Rust.

### Pure nodes in Rust, and `tatolab-ops`

- **The pure-node contract** in `tatolab-module-sdk` is the Rust form of `@node.pure`. A
  `#[pure_node]` function declares its ports, families, config and state in its signature, as
  the Python form does; the module's native half exports its descriptor and an evaluate entry
  the read seam calls with the input values, config, `now` and its state, returning its outputs.
  The module lists it in its `tatolab.nodes` entry points like any node, so its type is its
  import path. Two modules may both ship a `Lerp`, told apart by import path.
- The entry extends the module host table: the one place the engine calls module code, versioned
  with the table, and loaded only in a helper. No node code, first-party or not, runs in
  `tatolabd`.
- **`tatolab-ops`** is a first-party module of Rust pure nodes, bundled with the runtime and built
  exactly the way a third party builds one; nothing in the engine knows their names
  (`tatolab.ops:Lerp`). It carries:
  - Math: `Add`, `Subtract`, `Multiply`, `Divide`, `Power`, `Minimum`, `Maximum`, `Absolute`,
    `Modulo`.
  - Shaping: `Lerp`, `Clamp`, `Remap`, `Smoothstep`, `Step`.
  - Logic: `Compare`, `Switch`, `Select`, `Gate`.
  - Values and noise: `Constant`, `Random`, `PerlinNoise`.
  - Time: `Lfo`, `Timer`, `Lag`, `Trigger`, `Counter`, `SampleAndHold`.
  - Images: `ImageFile` (loads once, keeps publishing one frame) and `HoldFrame` (keeps one
    frame until told to refresh).
- `Select` picks a value; `Gate` passes or blocks messages, so a reactive node behind a closed
  gate does not run. That is the graph's if/else.

### Evaluation islands

- The compiler groups each connected run of shader and pure nodes into an **island**.
  Inside an island a link is a function call; at its edge a link is an ordinary channel with
  its family. The graph stays flat: an island is a compile result, never a container.
- **Where an island runs:**
  - **Pure nodes read by a helper** run inside that helper's read: an island output is a
    synthetic port in `InputMailboxesInner`, and a read evaluates it. No process, no thread, no
    processor instance. A Rust pure node runs inside any reader's read, a module's native half
    included.
  - **Shaders** run on one native evaluator per stream in `tatolabd`, which owns the device,
    and publish on a channel. A shader's code is Slang the engine compiles, never module code.
  - **A stateful pure node that feeds several readers** runs in one pure-node helper per
    stream, spawned only when the stream has such a node, and publishes on a channel. One
    state never has two copies.
  - **A Python pure island read by a module's native half** runs on the pool (phase 4) or the
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
- **Literal pins.** An unlinked input takes `config[<parameter>]`, else its default; a config
  change is live through `update_config`. See [NEEDS DECISION 3].
- **A control value on a wire.** An island edge carrying one value is Channels with an
  irregular control rate: one sample per message, rate unset. See [NEEDS DECISION 4].
- **The step ledger** leaves links inside an island off its balance; edge links count as today.
- **Loss.** A function-call link cannot drop; edge links take loss slots on the hosting
  helper's board, so no loss is silent.

### The state board

- Each stream has a board of fixed-size state slots, modelled on the loss-count board: the
  parent creates it, the host writes it. A stateful pure node keeps its state there,
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

### `@node.pure` in Python

```python
@node.pure
def remap(x: float, low: float = 0.0, high: float = 1.0) -> float:
    return low + x * (high - low)

@dataclass(frozen=True)
class SmoothedValue:
    value: float

@node.pure
def smooth(x: float, seconds: float, now: Time, previous: State[SmoothedValue]) -> Stateful[float, SmoothedValue]:
    ...
```

- Parameters are input ports and the return annotation is the output; a dataclass return gives
  several named outputs. `float` is a one-channel Channels port, `dict` is Records, and a read
  type is its family. A ParamSpec decorator keeps the call signature, so a test calls it.
- `Time` is integer nanoseconds. `State[T]` comes in and `Stateful[Out, T]` names what goes out,
  so the state is visible in the type. The state lives on the board.
- `add` takes a function through its own overload, and the zero-argument hole is closed.
- **The purity lint** runs at decoration, an `ast` reader in the shape of `_cross_floor_check.py`.
  It refuses by name calls to `time.*`, `random.*`, `open`, `socket`, `threading`,
  `os.urandom` and `datetime.now`, `global` and `nonlocal`, closures, and referenced mutable
  globals. It cannot see aliased, dynamic or transitive access; a function that cheats computes
  wrong values but cannot crash anything outside its interpreter. A Rust pure node is held to
  the same contract by review, not a lint.
- **A crash breadcrumb.** Before each pure call, in either language, the helper writes the node's
  name to its board.
  When a helper dies with the breadcrumb set, its restart moves that node out to its own
  process: promotion on crash, counted under `metrics` in `graph`. One crash of the reader is
  the cost. See [NEEDS DECISION 5].

### The pool and promotion (phase 4)

- Built only if real graphs carry pure Python that no Python class reads. One pool interpreter
  per stream hosts such nodes as plain calls. A pure node that runs past its island's frame
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
- **Inside an island** a resolved variable is the link it resolved to, so a get feeding a pure node is
  a function call like any other.

### Viewers

An island holds its latest values, so a viewer of a lightweight node's output reads them through
whatever read path `gateway-nodes` settles, at no process cost. Relays, gateways, tracks and
previews are class nodes a person places, never pure nodes, because they touch the network or
the device.

### What Tato Studio reads

- `kind` and `family` on every `graph` node and port, from the real runtime.
- A palette: category per node type, every installed module's pure nodes, `tatolab-ops`
  included, and a project's shader and pure nodes listed without running their code.
- Where each lightweight node runs: its host node, its fused group, promoted or not.
- The source-writing edit verbs accept shader and pure types and write an idiomatic
  `stream.py` line.
- Log records and state attribute a lightweight node by its own name, not its host's process.

## MODIFIED

- **The placement entry** (`ARCHITECTURE.md:1529-1546`). It reads: a Python **class** node runs
  in its own child process, exec'd from the stream's venv, never fork; shaders and Rust pure
  nodes run without a process; Python pure nodes run inside their Python reader, in a pool only in phase 4, and
  in its own process after a crash or a stall. "No processor may ever block another" holds
  between class nodes.
- **The class-identity entry** (`ARCHITECTURE.md:1732`). `@node` gains `@node.pure` and
  `@node.shader`, and a module's `#[pure_node]` is a node type too; a function's type is its
  import path, as a class's is.
- **The execution-flavors OPEN** (`ARCHITECTURE.md:1775-1777`) becomes DECIDED as evaluation
  islands.
- **The built-ins entry** (`ARCHITECTURE.md:539-549`). `tatolab-ops` is a first-party module of
  Rust pure nodes, bundled with the runtime and built on the SDK any module uses, so the runtime
  still ships no node of its own.
- **The module host table entry** (`ARCHITECTURE.md:550-559`) gains the pure-node entry, the one
  place the engine calls module code, and only in a helper.
- **`docs/decisions/helper-process-placement-only.md`**: the clauses this change retires are
  struck in place.
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
- **[NEEDS DECISION 5] A crash in inline pure code.** Breadcrumb and promotion on crash, or
  always isolate? *Recommendation:* the breadcrumb; it bounds the damage to one restart of the
  reader without a pool.
- **[NEEDS DECISION 6] The class kind's spelling.** *Recommendation:* keep `@node`; the
  class-versus-function shape already says which is which.

Assumed unless overruled:
- `tatolab-ops` is installed in every stream's environment with the runtime, so every helper can
  load it.
- The shader template is the `ImageInput` shape above.
- A material graph is a later change that asks `slang-shader-compiler` for a fragment target and
  adds a renderer node; nothing here precludes it.
- Blueprint's execution wire ("do A, then B") is not part of this change; gates cover streams.
- Wasm is out of scope.

## Proof

- `Lfo → Remap → <Python class>` runs with exactly one child process, the class's, beside the
  class case in `tests/stream-on-runtime/test_helper_placement.py`.
- `connect` and `disconnect` re-island live, and a cycle without a delay node is refused by name.
- A fixture module's Rust pure node joins an island beside `tatolab-ops`' and is read by a
  module's native half.
- A stateful pure node keeps its state across a helper restart and a stream reload.
- A stateful pure node feeding two readers gives both the same value.
- Two streams' shader dispatches no longer serialise on one host fence.
- One `@node.shader` runs Image in, Image out, with no Python process.
- A three-node point-wise shader chain records one submission per frame, matches the unfused
  output, and a blur in the middle splits it.
- `@node.pure` with `State` keeps its state across a reload; the lint refuses `time.time()`,
  `random.random()` and `open()` by name.
- A pure node that aborts its reader is promoted after one restart, and `graph` counts it.
- One `SetVariable` of an Image output read by two `GetVariable` nodes yields two links from the
  setter's source and no variable node at run time; a variable holding a gateway reference yields
  reference edges in `graph`; a missing setter, a duplicate setter and a wrong-family get are each
  refused by name.

## Tickets (expected)

1. Rust pure nodes at the read seam:
   1. `kind` in the descriptor, catalog and `graph`; the one run loop; the pure-node contract in
      `tatolab-module-sdk`; `tatolab-ops` with `Lfo`, `Remap`, `Clamp`; a fixture module's pure
      node joining an island beside them.
   2. The island pass on live edits, with cycle refusal.
   3. Literal pins and the full `tatolab-ops` set.
   4. Stateful pure nodes on the state board.
   5. Fan-out of a stateful pure node in the per-stream pure-node helper.
2. Shaders:
   1. Asynchronous GPU submission.
   2. One `@node.shader` on the native evaluator.
   3. Fusion under the split rule.
3. Pure nodes in Python:
   1. `@node.pure` with `Time` and `State`, inline in a Python reader.
   2. The purity lint.
   3. The crash breadcrumb and promotion on crash.
4. The pool and promotion, only if graphs need it.
5. Graph variables and the reference edge, after `strict-port-families`' contract ticket and
   before `gateway-nodes`' module ticket. Independent of phases 1 to 4.

## Notes (not in scope)

- Measure a helper's real cost (wheel import, idle memory, iceoryx2 and Vulkan setup) and the
  per-message hop before phase 3; phase 1 removes the hop for Rust pure nodes.
- The shader examples in `examples/` and the scaffold's `HalftoneLensEffect` move to
  `@node.shader` as consumer backlog. Retiring `GlslPixelEffect` is a separate owner call.
- A thread pool for native class nodes is a separate change.
