# lightweight-node-kinds

> **Nodes that are not processes**, decided by the owner on 2026-11-06. An editor that people
> and agents program needs dozens of tiny nodes — `mul`, `lerp`, `clamp`, an LFO, a compare —
> and a process per `add` is the wrong price. Unreal Blueprint and PCG, Houdini PDG,
> TouchDesigner, Blender, DaVinci Resolve and Effect House all run nodes in more than one
> mode; this change gives StreamLib the same. The owner retired the placement rule the same
> day: rules do not dictate architecture, and a process per Python node was the system as
> first built, not a limit on how it grows. The research behind this is the owner's doc
> "Lightweight nodes — research handoff" (2026-11-06).

Today every node is a processor. A Python `@node` class gets its own child process, and every
native processor gets its own OS thread. There is nothing smaller.

**After this change:**
- **One graph, four node kinds.** Every node has the same ports, families, links and `type`.
  What differs is its kind, declared by a contract about its code; the runtime derives where
  it runs. No placement surface and no dial.

  | Kind | Author writes | Contract | Runs |
  | --- | --- | --- | --- |
  | op | nothing — supplied (`Lerp`, `Lfo`, `Clamp`) | native, bounded | inline wherever it is read |
  | `@node.shader` | a GLSL function | Images and values in, an Image out, no side effects | on the GPU; a chain fuses into one dispatch |
  | `@node.pure` | a Python function | no hidden state; state and time are parameters | inside its Python reader, or the stream's pool |
  | `@node` class | a class with a lifecycle | anything: GPU, devices, threads, state | its own process, as today |

- **The rule an agent needs:** a class is a process; a function runs wherever the runtime puts
  it.
- **The kind travels in the graph and the catalog**, so an agent reads it and an editor draws it.

**Scale gate — this skill plus an ADR.** It changes the processor model and the Python API's
public contract. ADR: `docs/decisions/lightweight-node-kinds.md`.

**Sequencing.** Builds on `strict-port-families` and lands after it. The families are what let
the runtime inline and fuse: a typed descriptor says what a wire holds without decoding it.
Nothing in `strict-port-families` changes for this.

## Current state (recon, 2026-11-06, `main` `0477bbd`)

- **Python nodes.** `spawn_host_for_processor_node`
  (`runtime/streamlib-engine/src/core/compiler/compiler_ops/processor_interpreter_spawn_host.rs`)
  builds one host per graph node; the host execs the stream's venv interpreter with one class
  import path in `STREAMLIB_ENTRYPOINT` (`:64`). The Rust side is a supervisor that owns
  nothing but the child. `tatolabd` links no Python.
- **The decorator.** `node = NodeDeclarationDecorator()`
  (`sdk/tatolab-stream/tatolab/stream/_node_declaration.py:278-466`) declares a class:
  execution, interval, scheduling priority, description, and ports.
- **Native nodes** each run on a dedicated OS thread (`docs/decisions/execution-model.md`). The
  plan's OPEN on lighter execution flavors (`ARCHITECTURE.md:1776-1778`) is unstarted.
- **GLSL.** `GlslPixelEffect` (`sdk/tatolab-stream/tatolab/stream/glsl_pixel_effect.py`) wraps a
  user's shader body in a compute kernel and already takes per-frame floats as `dials`. It runs
  inside a Python processor.
- **Families.** Control values ride Channels (latest value under `newest`) and named values
  ride Records (`ARCHITECTURE.md` §Processor model, the five-families entry).
- **Built-ins.** The runtime keeps one, `RelayTrackSource` (`ARCHITECTURE.md:540-550`). The
  lent `tatolab.runtime` is the one native artifact every helper imports.

## ADDED

### Node kind

- Every graph node and catalog entry carries `kind`: `op`, `shader`, `pure` or `process`.
  `graph` and `/api/registry` render it. An editor draws a badge, colour and pin shape per kind;
  wire colour stays the family's.
- Kind and family are separate axes. A family says what flows on a wire; a kind says how a
  node's code runs. Neither decides the other, with three linking rules: a shader takes Images
  and values and returns an Image; a pure function takes values, Records and Arrays and may pass
  an Image along without touching its pixels; a class takes anything.

### Ops

- A fixed set the runtime supplies, each a node type in `tatolab.stream`:
  - Math: `Add`, `Subtract`, `Multiply`, `Divide`, `Power`, `Minimum`, `Maximum`, `Absolute`,
    `Modulo`.
  - Shaping: `Lerp`, `Clamp`, `Remap`, `Smoothstep`, `Step`.
  - Logic: `Compare`, `Switch`, `Select`, `Gate`.
  - Values and noise: `Constant`, `Random`, `PerlinNoise`.
  - Time: `Lfo`, `Timer`, `Lag`, `Trigger`, `Counter`, `SampleAndHold`.
- Op ports are Channels: one named channel per value, read latest-value under `newest`.
- Ops are Rust, compiled into the lent `tatolab.runtime`, so the same code runs in `tatolabd`
  and inside any helper that reads one. See [NEEDS DECISION 3].

### Evaluation islands

- The compiler groups each connected run of op, pure and shader nodes into an **evaluation
  island**. Inside an island a link is a function call; at its edge a link is an ordinary
  channel with its family. The graph stays flat — an island is a compile result, never a
  container the author sees.
- An island is evaluated by whoever reads it:
  - **A Python class reads it** → the island runs inside that reader's interpreter when it
    reads the input. Zero hops, no new process. This is Blueprint's pure-node model.
  - **A native node reads it** → ops and shaders run on one engine evaluator per stream; pure
    Python runs in the stream's pool (below).
  - **A stateful node feeds several readers** → it runs in exactly one place, the evaluator or
    the pool, and publishes on a channel, so two copies of one state never diverge.
- This settles the execution-flavors OPEN: the flavor is derived from the kind, and no node
  declares a dial.

### `@node.shader`

```python
@node.shader("""
vec4 shade(vec2 uv) { return mix(texture(a, uv), texture(b, uv), amount); }
""")
def blend(a: Image, b: Image, amount: float) -> Image: ...
```

- The function's signature declares the ports; its body is GLSL. The engine compiles it through
  the kernel path `GlslPixelEffect` already uses, with no second kernel system. A float input
  becomes a push constant, as `dials` are today.
- No readback and no side effects. A chain of shader nodes compiles into one kernel and one
  dispatch.
- It needs no Python at run time and no process.

### `@node.pure`

```python
@node.pure
def remap(x: float, low: float, high: float) -> float:
    return low + x * (high - low)

@node.pure
def smooth(x: float, seconds: float, now: Time, previous: State[float]) -> Smoothed:
    ...  # returns the new value and the new state
```

- Parameters are input ports, the return annotation is the output port, and a dataclass return
  gives several named outputs. `float` maps to a one-channel Channels port; `dict` to Records;
  a family type to its family.
- **Time is an input.** `now: Time` is the frame's timestamp on the stream's monotonic clock,
  never a clock read inside the function, so every evaluation is reproducible. Smoothing uses
  the real step, `exp(-dt/τ)`.
- **State is explicit.** `State[T]` comes in and the new state goes out. The runtime holds it
  keyed by the node, so a crash loses nothing, a reload keeps an LFO's phase, and `graph` can
  show it.
- **The purity check** runs at decoration and refuses, by name, a body that reads a clock or a
  random source, opens a file or socket, starts a thread, or assigns module state. Python
  cannot enforce purity; a function that cheats computes wrong values but cannot crash
  anything outside its interpreter. Module-level constants are the only shared state.

### The pool and promotion

- Each stream with pure Python that no Python class reads gets **one pool interpreter**: a
  helper process exec'd from the stream's venv, like any other. A chain inside it is plain
  function calls. The worst case is one extra process per stream, not one per node.
- Nodes in one pool can stall each other. **Promotion** is the remedy: a pure node that runs
  past its island's frame interval three times running is moved into its own process. It is
  counted and rendered under `metrics` in `graph` as `promoted_pure_nodes`. Authors never
  choose; the runtime does, from behaviour, as Cloudflare does with misbehaving Workers.

## MODIFIED

- **The placement entry** (`ARCHITECTURE.md:1530-1547`, "Helper-process placement is the only
  execution placement"). It reads: a Python **class** node runs in its own child process,
  exec'd from the stream's venv, never fork; ops and shaders run without a process; pure
  Python runs in its Python reader's interpreter or the stream's pool. "No processor may ever
  block another" holds between class nodes; inside a pool it is bounded by promotion.
- **The class-identity entry** (`ARCHITECTURE.md:1732`, "`@node` declares execution, interval,
  scheduling priority, and description only"). Adds the kind attributes, `@node.pure` and
  `@node.shader`; a function's type is its import path, as a class's is.
- **The execution-flavors OPEN** (`ARCHITECTURE.md:1776-1778`) becomes DECIDED as evaluation
  islands, above.
- **The built-ins entry** (`ARCHITECTURE.md:540-550`). The ops join `RelayTrackSource` as the
  runtime's own nodes, as general-purpose engine primitives rather than media nodes.
- **`docs/decisions/helper-process-placement-only.md`** is annotated as amended by this change.

## REMOVED

These two are removed in the PR that carries this proposal, by owner ruling (2026-11-06).

- REMOVED: .claude/rules/placement.md
- REMOVED: check_no_in_process_placement

## Decisions

The owner settled the direction on 2026-11-06. These choices remain; each carries a
recommendation that is built if nobody overrules it.

- **[NEEDS DECISION 1] The class kind's spelling.** Keep plain `@node`, or make it explicit as
  `@node.process`?
  *Recommendation:* keep `@node`. The class-versus-function shape already says which is which,
  and `process` names a placement that could change.
- **[NEEDS DECISION 2] When a pure node is evaluated.** Once per input message for each island
  (memoised by timestamp), or once per pull, as Blueprint does?
  *Recommendation:* once per message. A per-pull `Random` read by two nodes gives two values,
  Blueprint's best-known surprise.
- **[NEEDS DECISION 3] Where ops live.** Compiled into the lent `tatolab.runtime`, running in
  `tatolabd` and every helper; or a first-party native module installed per stream?
  *Recommendation:* the lent runtime. An island must inline into any reader, and the lent
  artifact is already in every helper.

Assumed unless overruled:
- An op or pure node with no inputs, such as an LFO, is pulled at its reader's frame
  timestamp, which is exact.
- Built-ins as readers: a built-in that should take a control value gets a Channels input of
  its own, one built-in at a time, in its own change. This change adds none.
- Wasm is out of scope. Ops, pure Python and GLSL cover the editor's node list.

## Proof

- A stream `Lfo → Remap → <Python class>` runs with exactly one child process: the class's.
  The behavioural gate `tests/stream-on-runtime/test_helper_placement.py` grows this case beside
  its class-node case.
- `Lfo → Remap → <native reader>` runs with no child process.
- A chain of three `@node.shader` nodes records one dispatch per frame.
- `@node.pure` with `State[float]` keeps its state across a stream reload.
- A pure node that sleeps past its interval is promoted, the stream keeps running, and `graph`
  shows the promotion.
- The purity check refuses `time.time()`, `random.random()` and `open()` in a body, by name.
- A pool interpreter that crashes takes down only its stream's pure nodes; the runtime and
  other streams keep running.

## Tickets (expected)

1. Node kind in the graph and catalog, the ops in the lent runtime, and the engine evaluator:
   `Lfo → Remap` feeding a native reader.
2. Islands inline into a Python class reader.
3. `@node.shader` and chain fusion.
4. `@node.pure` with time, state and the purity check, inline in a Python reader.
5. The pool and promotion.

## Notes (not in scope)

- A zygote to make class nodes cheaper to start is a separate optimisation, Linux-only.
- Measuring a helper's real cost (wheel import, idle memory, iceoryx2 and Vulkan setup) and
  the per-message hop would size how much each kind saves. The doc lists them; none blocks
  this change.
