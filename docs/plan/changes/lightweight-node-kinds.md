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
- **One graph, four node kinds.** Every node has the same ports, families, links and `type`.
  What differs is its kind, declared by a contract about its code; the runtime derives where
  it runs. No placement surface and no dial.

  | Kind | Author writes | Contract | Runs |
  | --- | --- | --- | --- |
  | op | nothing; supplied (`Lerp`, `Lfo`, `Clamp`) | native, bounded | inside the read of the node that reads it |
  | `@node.shader` | a Slang function | Images and values in, an Image out, no side effects | on the GPU; a chain fuses into one kernel |
  | `@node.pure` | a Python function | no hidden state; state and time are parameters | inside the Python node that reads it |
  | `@node` class | a class with a lifecycle | anything: GPU, devices, threads, state | its own process, as today |

- **The rule an agent needs:** a class is a process; a function runs wherever the runtime puts
  it, and never gets a process of its own unless it crashes its reader.
- **The kind travels in the graph and the catalog**, so an agent reads it and an editor draws it.
- **One run loop.** A helper runs the engine's own Rust runner and calls Python's `process()`
  in-process, so reactive, continuous and the island wake rules are written once.

**Scale gate — this skill plus an ADR.** It changes the processor model and the Python API's
public contract. ADR: `docs/decisions/lightweight-node-kinds.md`.

**Sequencing.** Phase 1 needs the families' typed descriptors (`strict-port-families`, merged
in `3e7e58b`). The SDK phases land after that change's decorator and scaffold tickets, because
both rewrite the port surface. Phase 2 builds on `slang-shader-compiler`.

## Current state (recon, 2026-11-06, `main` `6e3fee9`)

- **Python nodes.** Each is a `ProcessorInterpreterSpawnHostProcessor` in Manual mode
  (`processor_interpreter_spawn_host.rs:1361`) that only starts and supervises the child. The
  child's bootstrap re-implements reactive and continuous in Python (`_run_reactive`,
  `_run_continuous`, `_processor_interpreter_bootstrap.py:1470-1580`). Native nodes use the
  Rust runner (`thread_runner.rs`).
- **Where readers run.** Every module's native half runs in its node's helper, and `tatolabd`'s
  one built-in, `RelayTrackSource`, has no inputs (`ARCHITECTURE.md:540-550`). Nearly every
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

- Every graph node and catalog entry carries `kind`: `op`, `shader`, `pure` or `process`.
  `graph` and `/api/registry` render it, beside each port's `family`. Kind and family are
  separate axes: a family says what flows on a wire, a kind says how a node's code runs.
- Linking rules: a shader takes Images and values and returns an Image; a pure function takes
  values, Records and Arrays and may pass an Image along without reading its pixels; a class
  takes anything.

### One run loop

- A helper's loop is the engine's runner, running in the child and calling the Python
  instance's `process()` in-process. The bootstrap keeps the lifecycle commands, imports and
  describe; it no longer schedules. Every rule below is written once, in Rust.

### Ops

- A fixed set in the engine crate, so `tatolabd` and the lent `tatolab.runtime` both carry
  them, each a node type in `tatolab.stream` generated with its families and kind:
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

- The compiler groups each connected run of op, shader and pure nodes into an **island**.
  Inside an island a link is a function call; at its edge a link is an ordinary channel with
  its family. The graph stays flat: an island is a compile result, never a container.
- **Where an island runs:**
  - **Ops and pure nodes read by a helper** run inside that helper's read: an island output is
    a synthetic port in `InputMailboxesInner`, and a read evaluates it. No process, no thread,
    no processor instance.
  - **Shaders, and a stateful node that feeds several readers,** run on one native evaluator
    per stream in `tatolabd`, which owns the device, and publish on a channel. One state never
    has two copies.
  - **A pure island read by a module's native half** runs on the evaluator's pool (phase 4) or
    the port is refused by name, since that read holds no GIL.
- **The island pass** runs between plan and PREPARE: union-find over lightweight nodes, a host
  per island, cycle refusal. It re-runs on every live `connect` and `disconnect`, touching only
  the islands the edit changes. Island nodes skip PREPARE and SPAWN.
- **The wiring envelope** gains an island payload for the reader: the op DAG, its config and
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
- **Cycles.** The island pass refuses a cycle unless it passes through a delay op (`Lag`,
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
  parent creates it, the host writes it. A stateful op or pure node keeps its state there,
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

### `@node.pure`

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
  wrong values but cannot crash anything outside its interpreter.
- **A crash breadcrumb.** Before each pure call the helper writes the node's name to its board.
  When a helper dies with the breadcrumb set, its restart moves that node out to its own
  process: promotion on crash, counted under `metrics` in `graph`. One crash of the reader is
  the cost. See [NEEDS DECISION 5].

### The pool and promotion (phase 4)

- Built only if real graphs carry pure Python that no Python class reads. One pool interpreter
  per stream hosts such nodes as plain calls. A pure node that runs past its island's frame
  interval three times running is promoted to its own process; a hung call cannot be
  pre-empted, so the pool is respawned with state handed over through the board.

### What Tato Studio reads

- `kind` and `family` on every `graph` node and port, from the real runtime.
- A palette: category per node type, ops listed as types, and a project's shader and pure
  nodes listed without running their code.
- Where each lightweight node runs: its host node, its fused group, promoted or not.
- The source-writing edit verbs accept op, shader and pure types and write an idiomatic
  `stream.py` line.
- Log records and state attribute a lightweight node by its own name, not its host's process.

## MODIFIED

- **The placement entry** (`ARCHITECTURE.md:1530-1547`). It reads: a Python **class** node runs
  in its own child process, exec'd from the stream's venv, never fork; ops and shaders run
  without a process; pure Python runs inside its Python reader, in a pool only in phase 4, and
  in its own process after a crash or a stall. "No processor may ever block another" holds
  between class nodes.
- **The class-identity entry** (`ARCHITECTURE.md:1732`). `@node` gains `@node.pure` and
  `@node.shader`; a function's type is its import path, as a class's is.
- **The execution-flavors OPEN** (`ARCHITECTURE.md:1776-1778`) becomes DECIDED as evaluation
  islands.
- **The built-ins entry** (`ARCHITECTURE.md:540-550`). The ops are the runtime's own nodes:
  general-purpose engine primitives, not media nodes. Nothing here depends on which other
  built-ins remain.
- **`docs/decisions/helper-process-placement-only.md`**: the clauses this change retires are
  struck in place.

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
- Ops live in the engine crate, so every host carries them.
- The shader template is the `ImageInput` shape above.
- A material graph is a later change that asks `slang-shader-compiler` for a fragment target and
  adds a renderer node; nothing here precludes it.
- Blueprint's execution wire ("do A, then B") is not part of this change; gates cover streams.
- Wasm is out of scope.

## Proof

- `Lfo → Remap → <Python class>` runs with exactly one child process, the class's, beside the
  class case in `tests/stream-on-runtime/test_helper_placement.py`.
- `connect` and `disconnect` re-island live, and a cycle without a delay op is refused by name.
- A stateful op keeps its state across a helper restart and a stream reload.
- A stateful op feeding two readers gives both the same value.
- Two streams' shader dispatches no longer serialise on one host fence.
- One `@node.shader` runs Image in, Image out, with no Python process.
- A three-node point-wise shader chain records one submission per frame, matches the unfused
  output, and a blur in the middle splits it.
- `@node.pure` with `State` keeps its state across a reload; the lint refuses `time.time()`,
  `random.random()` and `open()` by name.
- A pure node that aborts its reader is promoted after one restart, and `graph` counts it.

## Tickets (expected)

1. Ops at the read seam:
   1. `kind` in the descriptor, catalog and `graph`; the one run loop; `Lfo`, `Remap`, `Clamp`.
   2. The island pass on live edits, with cycle refusal.
   3. Literal pins and the full op set.
   4. Stateful ops on the state board.
   5. Fan-out of a stateful op on the native evaluator.
2. Shaders:
   1. Asynchronous GPU submission.
   2. One `@node.shader` on the native evaluator.
   3. Fusion under the split rule.
3. Inline pure Python:
   1. `@node.pure` with `Time` and `State`, inline in a Python reader.
   2. The purity lint.
   3. The crash breadcrumb and promotion on crash.
4. The pool and promotion, only if graphs need it.

## Notes (not in scope)

- Measure a helper's real cost (wheel import, idle memory, iceoryx2 and Vulkan setup) and the
  per-message hop before phase 3; phase 1 removes the hop entirely.
- The shader examples in `examples/` and the scaffold's `HalftoneLensEffect` move to
  `@node.shader` as consumer backlog. Retiring `GlslPixelEffect` is a separate owner call.
- A thread pool for native class nodes is a separate change.
