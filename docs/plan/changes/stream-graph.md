# stream-graph

The first build step of the one-runtime-per-machine pivot: a stream is a `@stream` function, the
function compiles to the stream's graph, the graph is the engine's snapshot extended into the one
shape `graph` renders, the runtime loads it, and what the stream exposes is in it and is all the
machine offers. After this change:
- `stream.py` defines streams as `@stream` functions over a `Stream` builder; `setup(rt)` is gone;
- `@node` declares a node where `@processor` did, and the handles a stream is built with carry the
  glossary's names;
- one graph shape, loadable from `graph`'s own output — `stream`, `nodes`, `links`, `exposed`, the
  live keys beside them; `Runtime.load` runs it; `rt.add`, `rt.connect` and the remote references
  on `Runtime` are gone;
- a typed duplicate node name is refused where it is written; a defaulted one is suffixed as today;
- nothing leaves the machine until exposed: the mesh offers and sends exposed ports only.

Unchanged and left to later changes, each mapped below: one engine per `run` process, today's
runtime name as the first chunk of a mesh address, the environment a node runs from, the local
API's verbs, persistence across restarts.

**Scale gate — this skill, plus the existing ADR.** The Python API's public contract moves
(`@processor` → `@node`, `rt.add` → `stream.add`, `Runtime.load`); the engine accepts a graph as
data from Python, which it did not; the control vocabulary's `graph` output changes shape. The
rationale is `docs/decisions/one-runtime-per-machine.md` whole — "a graph is data", "emitted,
never authored", "`rt.add` was the wrong shape", "the builder object changes, the calls do not" —
and this change adds none of its own, so no second ADR.

**Precondition.** Every entry this delta builds is DECIDED: §Product `ARCHITECTURE.md:99-106` (a
stream is the unit), `:126-133` (several streams per project; `setup` retires), `:39-46` (terms
of the sentence, as amended), `:83-98` (the scaffold); §Processor model `:1253-1278` (identity is
the import path), `:1279-1297` (decoration registers), `:1300-1318` (the name, with its typed
duplicate amendment), `:1322-1327` (a graph is data), `:1328-1333` (emitted, never authored);
§Networking `:3986-3993` (nothing leaves until exposed). The OPENs beside them are not built
against: loading and keeping a stream `:114-125`, composition `:134-140`, what the graph holds
beyond nodes, links and exposures `:1334-1341`, several streams in one runtime process
`:1342-1352`, resources `:1353-1358`, discovery `:3994-4010`, the stream map `:4011-4018`, the
URL grammar `:4023-4034`, the local API `:4437-4444`. The address entry `:3969-3985` is DECIDED
and deliberately untouched here; "Left to later changes" says why.

**Verified against the tree 2026-10-01 (HEAD 4c2f22761).** Three read-only recon sweeps.

**The graph today**
- `GraphSnapshot { name: Option<String>, processors: Vec<ProcessorDefinition>, connections:
  Vec<ConnectionDefinition> }` (`runtime/streamlib-engine/src/core/graph_snapshot.rs:26-37`);
  a definition is `alias`, `type`, `config`, `display_name?` (`:41-62`); a connection is
  `"alias.port"` to `"alias.port"` (`:66-72`). Pretty JSON through serde (`:117-162`).
- `Runner::load_graph_snapshot` validates, adds each definition through `add_processor` and
  connects by alias (`core/runtime/runtime.rs:1341-1398`); `save_graph_snapshot` regenerates
  aliases from the live graph and refuses a graph holding any remote link (`:1432-1526`). Callers:
  engine tests only (`tests/graph_snapshot_round_trip_test.rs:71-82`); nothing in the api-server,
  the wheel or the CLI calls either.
- `graph` renders `GraphResponse { nodes, links, extensions, mesh }`
  (`core/json_schema.rs:30-41`): a node is `id`, `type`, `display_name`, `config`,
  `config_checksum`, `ports`, `components` (`:210-227`); a link end is a `processor_id` or
  `{runtime_name, processor_display_name, port_name}` (`:272-300`).
- `rt.add` sends `ProcessorSpec { name: <import path>, config, display_name }`
  (`sdk/streamlib-python-wheel/src/python_runtime_lifecycle.rs:361-408`,
  `core/processors/processor_spec.rs:13-22`); the constructor closure captures the import path,
  never the class (`src/python_processor_registration.rs:84-110`); the MCP `add_processor` path
  resolves an import path by importing it in the runtime process
  (`:183-215`, `python_runtime_lifecycle.rs:273-297`).
- Every requested display name is disambiguated with ` 2`, ` 3` …
  (`core/graph/traversal/mutation_ops/add_v_op.rs:97-121`); the typed-duplicate amendment at
  `ARCHITECTURE.md:1300-1304` is not in the tree.

**The authoring surface today**
- `@processor(execution=, interval_ms=, scheduling=, description=)`, `@input`, `@output`
  (`python/streamlib/_processor_declaration.py:249-341`); the config class is the one `config`
  parameter of `__init__` (`:435-518`); the stamp is read by the native half
  (`src/python_processor_declaration.rs:40-49`).
- `Runtime.add(processor_class, *, config=, display_name=) -> AddedProcessor`
  (`_engine.pyi:586-592`), `connect` (`:627-631`), `remote_processor_output` /
  `remote_processor_input` (`:603-619`); `AddedProcessor.processor_id`, `.display_name`,
  `.output()`, `.input()` (`:750-760`); the four port references (`:762-792`).
- `streamlib run` and `dev` execute `app.py` with `runpy`, read `setup` off its namespace,
  construct `Runtime(...)`, call `setup(runtime)`, host the control plane, `run()`
  (`python/streamlib/cli.py:67-69`, `:165-213`, `:216-293`). `new` renders
  `_scaffold_template/` — `app.py`, `processors/{__init__,inverting_effect,brightness_meter}.py`,
  `pyproject.toml` (`:302-407`).
- About forty test fixtures use `setup(rt)` (`tests/app_under_test.py` and siblings); thirteen
  converted examples and the two extension wheels' four processors use `@processor`.

**Exposure today**
- The offered-ports answer lists every output of every processor
  (`core/runtime/output_ports_in_this_runtimes_graph.rs:127-146`); an egress is created for any
  port the first remote reader token names (`core/runtime/mesh/mesh_port_egress.rs:4-18`); no
  `expose` surface exists in `runtime/` or `sdk/`.

---

## [NEEDS DECISION] 1 — the stream's name and description, and which stream `run` loads

`ARCHITECTURE.md:126-133` decides several `@stream` functions per file or package and records
the rest as direction. This change has to pick one spelling to build.

- **(a) The direction, as recorded.** A stream's name defaults to its function's name; its
  description is the function's docstring; `run` and `dev` with no argument load the sole
  `@stream` in `stream.py` and refuse by name, listing them, when there are several; `run
  <file>.py:<name>` and `run <module>:<name>` load one. `run --name <stream name>` overrides the
  name at load. A package declaring its streams under an entry-point group is the packs OPEN's
  (`:527-542`) and is not built here.
- **(b) Always name it.** `@stream(name=..., description=...)` required, `run <name>` required
  even with one stream. Zero ceremony loses its one-line hello.
- **(c) The entry-point group now.** Every project declares its streams in `pyproject.toml`
  before `run` can find them. That is the first streamlib-specific table an app would author,
  which `:3518-3524` already rejected once.

**Recommendation: (a).**

**RESOLVED — (a), owner, 2026-10-01.** Reversible later at low cost: the graph carries the name
either way, so moving to (b) is the decorator taking a required name and `run` refusing without
one, with no change to the graph, the loader or `graph`.

---

## ADDED: §Product — the stream, as proposed

- **`@stream`.** A module-level function taking one `Stream` argument, decorated bare. The
  decorator stamps the function with its identity (import path, the identity rule nodes already
  follow) and returns it unchanged: no registry, no engine, no side effect, so a stream module
  imports without a runtime in the process. Its name and description follow decision 1.
- **`Stream`, the builder** (`streamlib.Stream`, pure Python, stub-gated):
  ```python
  class Stream:
      name: str
      def add(self, node_class: type, *, name: str | None = None,
              config: Mapping[str, Any] | None = None) -> NodeReference: ...
      def connect(self, source: NodeOutputPortReference | RemoteNodeOutputPortReference,
                  destination: NodeInputPortReference | RemoteNodeInputPortReference) -> None: ...
      def expose(self, output: NodeOutputPortReference) -> None: ...
      def remote_output(self, runtime_name: str, node_name: str, port_name: str) -> RemoteNodeOutputPortReference: ...
      def remote_input(self, runtime_name: str, node_name: str, port_name: str) -> RemoteNodeInputPortReference: ...
  ```
  `add` records the node's `type` (the class's import path, derived by the seam `rt.add` uses
  today — `classify_processor_class`, reached from Python through one stub-gated function so a
  built-in's marker and a `@node` class answer alike), its config as a JSON object, and its name.
  `connect` records a link; `expose` records an output. Nothing runs: the builder holds data.
- **Names are resolved by the builder**, because only it knows a typed name from a defaulted one:
  a defaulted name is the class's short name, a defaulted duplicate takes ` 2`, ` 3` … exactly as
  the engine spells it today, and a typed duplicate raises at the `add` that typed it, naming
  both — the error lands on the author's own line. The emitted graph therefore carries resolved,
  unique names, and a link names its ends by them. The engine keeps its own defaulting for a live
  `add_processor` that names nothing, and gains the same typed-duplicate refusal; one rule, two
  seams, each proven on the same inputs.
- **`compile_stream_to_graph(stream_function, *, name=None) -> dict`** runs the function once
  over a fresh `Stream` and returns the graph. A function that raises propagates; a function
  that adds nothing yields an empty graph, which `load` refuses by name.
- **`Runtime.load(stream_or_graph, *, name=None) -> None`** takes a `@stream` function or a
  graph mapping, compiles the former, and loads the graph into the engine before `run()`: each
  node through today's `add_processor` with its resolved name, refusing a name already in the
  graph instead of suffixing it; each link through today's `connect`, a remote end through the
  link request a remote end already takes; each exposure into the exposure set below. A second
  `load` on one runtime is refused by name: several streams in one runtime process is OPEN
  (`:1342-1352`), and this change runs one stream per `run`.
- **Embedding** is `Runtime(...)`, `load(...)`, `run()`. Live changes after `load` go through
  the control vocabulary as they do for every other client; `Runtime` carries no `add`,
  `connect`, `remote_processor_output` or `remote_processor_input` any more, because a builder
  beside a runtime that also builds is the two-ways-of-working the ADR rejected.
- **`run` and `dev`** find the entry file — `stream.py` by convention, `-f` overriding — execute
  it as today, pick the stream per decision 1, `compile_stream_to_graph`, `Runtime(...)`,
  `load`, host the control plane, `run()`. The mesh flags, `--host` and `--port` are untouched.
  `dev`'s edit loop is the same restart over the same path.
- **The scaffold** writes `stream.py` with one `@stream` over the two nodes it writes today,
  each under `nodes/` instead of `processors/`, and exposes the effect's output:
  ```python
  @stream
  def main(stream: Stream) -> None:
      """Camera, inverted, in a window; brightness logged once a second."""
      source = stream.add(CameraSource)
      effect = stream.add(InvertingEffect)
      meter = stream.add(BrightnessMeter)
      window = stream.add(DisplayWindow, config={"title": "StreamLib", "scaling": "fit"})
      stream.connect(source.output("video"), effect.input("video_from_upstream"))
      stream.connect(effect.output("video_to_downstream"), window.input("video"))
      stream.connect(effect.output("video_to_downstream"), meter.input("video_from_upstream"))
      stream.expose(effect.output("video_to_downstream"))
  ```
  The templates stay importable and checkable as `:83-98` requires; ruff, pyright and the
  cross-floor check gate the render as today.

## ADDED: §Processor model — `@node`, and the names a stream is built with

- **`@node(execution=, interval_ms=, scheduling=, description=)`** is `@processor` renamed: the
  same stamp, the same config-class rule, the same registration at decoration, the same refusals.
  `@input` and `@output` are unchanged. `@processor` is deleted, not aliased.
- **`NodeReference`** replaces `AddedProcessor`: `name: str`, `output(port_name)`,
  `input(port_name)`. It carries no `processor_id` — ids are minted at load, after the builder
  has finished — so a test that read an id off the handle reads it off `graph`.
- **`NodeOutputPortReference`, `NodeInputPortReference`, `RemoteNodeOutputPortReference`,
  `RemoteNodeInputPortReference`** replace the four `Processor…PortReference` classes; a local
  reference carries the node's name and the port, a remote one the three-part address of today.
- The engine's Rust identifiers, the wire between runtime process and processor interpreter, and
  the processor registry keep the word "processor", as the glossary states until the rename.

## MODIFIED: §Processor model `:1322-1333` — the graph, one shape

- **The snapshot is the spec, and `graph` renders it live.** `GraphSnapshot` becomes:
  ```json
  {"stream": "main",
   "nodes": [{"name": "CameraSource", "type": "<import path>", "config": {}},
             {"name": "InvertingEffect", "type": "nodes.inverting_effect:InvertingEffect", "config": {}}],
   "links": [{"source": {"node": "CameraSource", "port": "video"},
              "target": {"node": "InvertingEffect", "port": "video_from_upstream"}},
             {"source": {"runtime_name": "rig", "node": "camera", "port": "video"},
              "target": {"node": "InvertingEffect", "port": "video_from_upstream"}}],
   "exposed": [{"node": "InvertingEffect", "port": "video_to_downstream"}]}
  ```
  Aliases, `processors`, `connections`, `ProcessorDefinition` and `ConnectionDefinition` go;
  `name` is the node's resolved name and the key links use; a remote end is today's three-part
  address under the keys `graph` already renders for one. `validate()` keeps its checks — names
  unique, every link end present or remote, every `type` registered — and gains: every exposed
  port names a node and an output it has.
- **`graph` renders exactly these keys plus the live ones**: `stream` at the top (absent until a
  load), `exposed` always; a node carries `name` where it carried `display_name`, beside `id`,
  `type`, `config`, `config_checksum`, `ports`, `components`; a link's local end is `{node,
  port}` with `processor_id` beside it, a remote end unchanged; `links[].id`, `state`, counters,
  `extensions` and `mesh` as today. A loader reads the spec keys and ignores the rest, so
  `streamlib graph`'s output is a loadable graph; `save_graph_snapshot` and its path form are
  deleted, since the render is the export. The OpenAPI schema, the generated schemas, the MCP
  instructions and the strict fixture follow.
- **Emitted, never authored, holds by construction**: the only writers of a graph are the
  builder and the render. `load` accepts a mapping because the render is one; nothing documents
  hand-writing it.

## MODIFIED: §Networking `:3986-3993` — exposure gates what leaves

- **The exposure set** is engine state beside the graph: `(processor id, port)` pairs, filled by
  `load` from `exposed`, rendered in `graph` by node name. A removed processor leaves it.
- **The offer answers exposed ports only**: `every_output_port_in` becomes the exposed subset
  (`output_ports_in_this_runtimes_graph.rs:127-146`), so a reader asking for an unexposed port
  gets today's `error` listing what *is* offered (`ARCHITECTURE.md:3754-3756`), and
  `ports_it_holds_and_cannot_send` is computed over the exposed subset too.
- **An egress is created for an exposed port only** (`mesh_port_egress.rs:4-18`): a reader token
  naming an unexposed port creates nothing, and `mesh.egress_ports` stays what it is.
- **Links between streams on one machine are unaffected**, there being one stream per runtime
  process in this change; links from a processor interpreter and the local tap are unaffected,
  since neither leaves the machine. A link request *into* this runtime is unaffected: exposure is
  about leaving (sentence 3), and the stream map OPEN owns the inbound side.
- **No `expose` verb** joins the control vocabulary here — it is the local API OPEN's
  (`:4437-4444`) — so a live `add_processor` adds an unexposed node, and exposing it means
  changing the stream's function, which is the "function wins" rule applied.
- The two-process mesh fixtures and the live arms expose what they wire.

## MODIFIED: §Product `:39-46`, `:83-98` and §Processor model `:1253-1318` — records re-spelled

- `:46`'s amendment reads "`setup(stream)` on a Stream builder" from before `setup` retired; it
  is re-spelled to "`@stream` functions over a `Stream` builder; `@node`" in this PR as a factual
  record. `:83-98`'s `processors/` becomes `nodes/`. `:1253-1278` and `:1300-1318` keep their
  text; where they say `rt.add` the reading rule applies until the fold.
- `docs/architecture/` pages describing `setup(rt)`, `rt.add`, `@processor` or the snapshot's
  aliases are corrected in the shipping tickets, as the record they are.

## Left to later changes, so nothing is lost

| Not here | Because | Lands with |
|---|---|---|
| The `<machine>/` address segment and the stream in the Zenoh key (`:3969-3985`) | With one engine per `run` process there is no machine to name but today's runtime name; adding the segment now would refuse the second stream on a machine | the change that makes one runtime host several streams |
| The name grammar (URL-segment characters, case) in the same entry | It is address grammar | the same change |
| `graph` returning several streams; `load`, `unload`, `streams`, `expose` as verbs; MCP argument spellings (`display_name` and friends) | the local API OPEN | the local-API change, after its align |
| The stream's environment (venv path) in the graph; needs derived from nodes | OPEN `:1334-1341` | per-stream environments |
| Persisting loaded graphs in a state directory; re-load on restart | OPEN `:114-125`; the one-engine entry's state directory | runtime startup |
| `@stream` declarations readable without an engine; `@node` registering nothing when no engine is present | the independence OPEN `:508-519` | per-stream environments |
| `ProcessorLinkDataAccess`, `ProcessorOwnedWindow`, `ProcessorOwnedWindowEvents`, `ProcessorOutputTextureRing` | per-node capability classes, not the stream-building surface | the namespace rename, which re-spells every public name at once |

The ripout change's inventory table (`one-runtime-per-machine-ripout.md:236-246`) mapped the
three-part `MeshPortAddress` to this change; it moves to the hosting change for the reason above.

## Assumptions stated, not asked

- The entry file is `stream.py`, as `:99-106` spells it; `-f` still overrides, and `app.py` is
  not looked for.
- `Runtime.load` refuses a second load; one stream per `run` until `:1342-1352` is decided.
- Exposure gates the mesh in this change rather than being recorded and inert: closed by default
  is decided, and a recorded `expose` that gated nothing would be the no-op surface the doctrine
  forbids. If the owner prefers the gate to ride the stream-map change, S4 below splits off whole.
- The extension wheels' four processors move to `@node` inside S1, as the canary §Consumers
  reserves and as the config-class change did; the thirteen examples are filed as backlog at
  ship, never fixed in-stream.
- The per-node capability classes keep their `Processor…` names until the namespace rename.

## Expected slices

- **S1 — `@node` and the references.** The decorator, `NodeReference`, the four port references,
  the stub, `__init__`'s exports, the wheels' four processors, the scaffold's `nodes/`.
  Independent.
- **S2 — the graph.** `GraphSnapshot` in the one shape; `graph` rendering it; `load_graph_snapshot`
  loading it by name with the typed-duplicate refusal in the engine; the saver deleted; schemas
  and fixtures. Independent of S1.
- **S3 — `@stream`, `Stream`, `load`, `run`.** The builder and its name resolution,
  `compile_stream_to_graph`, `Runtime.load`, `rt.add`/`connect`/remote references deleted, `run`
  and `dev` over `stream.py`, the scaffold's `stream.py`, the forty fixtures. Blocked by S1, S2.
- **S4 — exposure.** The exposure set, the offer and the egress over it, the mesh fixtures
  exposing. Blocked by S2.

Restart time stays an acceptance criterion of the hosting change, not this one (#2559's record).

## REMOVED

- REMOVED: def setup(rt
- REMOVED: rt.add(
- REMOVED: @processor
- REMOVED: APP_SETUP_FUNCTION_NAME
- REMOVED: read_app_setup_function
- REMOVED: DEFAULT_APP_ENTRY_FILE_NAME
- REMOVED: sdk/streamlib-python-wheel/python/streamlib/_scaffold_template/app.py
- REMOVED: sdk/streamlib-python-wheel/python/streamlib/_scaffold_template/processors/inverting_effect.py
- REMOVED: AddedProcessor
- REMOVED: ProcessorOutputPortReference
- REMOVED: ProcessorInputPortReference
- REMOVED: RemoteProcessorOutputPortReference
- REMOVED: RemoteProcessorInputPortReference
- REMOVED: remote_processor_output
- REMOVED: remote_processor_input
- REMOVED: ProcessorDefinition
- REMOVED: ConnectionDefinition
- REMOVED: save_graph_snapshot
- REMOVED: pipeline_name
