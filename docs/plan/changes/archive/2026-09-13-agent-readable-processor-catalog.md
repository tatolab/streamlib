# agent-readable-processor-catalog

What a node can tell an agent about a processor before the agent adds it: a config shape
that is a JSON Schema derived from the config type the author already wrote, a Python
class that is in the catalog the moment its decorator runs, and nothing about what a bag carries — that question is the section's OPEN. Implements
the three
`[agent-readable-processor-catalog]` entries in `docs/plan/ARCHITECTURE.md` §Processor
model (`:576-606`, `:675-680`) and the corrected first-add sentence in §Control plane
(`:2381-2384`). Owner align 2026-09-10; rationale in
`docs/decisions/agent-readable-processor-catalog.md`. What a port reports about the bags
it carries is the section's OPEN at `:618` and is not touched by anything here.

**Scale gate — this skill, plus an ADR (it exists).** New behavior on the processor model
(the descriptor carries a schema document, registration moves to declaration) and a
changed contract on the Python API's public surface (config is a class; keyword-argument
configuration is deleted).

**Precondition.** Every entry this touches is DECIDED — `:576`, `:586`, `:600`, `:675`,
and §Control plane `:2368`. The section's OPEN at `:618` is untouched: no ticket derived
from this renders anything new on a port.

**Verified against the tree 2026-09-10 (HEAD 7f531555e)** — two read-only recon sweeps.

- The descriptor's config slot is a type-name string: `config_schema: Option<String>` at
  `sdk/streamlib-processor-schema/src/descriptors.rs:151-153`, set by the macro from the
  config type's last path segment (`sdk/streamlib-macros/src/grammar.rs:245-251`) or the
  `config_schema = "…"` attribute key (`:203-210`), emitted at
  `sdk/streamlib-macros/src/codegen.rs:625-630`. Three grammar tests pin it
  (`grammar.rs:810-841`). The Python path never sets it
  (`sdk/streamlib-python-wheel/src/python_processor_declaration.rs:39-52`).
- `ConfigField` / `ConfigDescriptor` (`descriptors.rs:90-127`), the `ConfigDescriptor`
  derive (`sdk/streamlib-macros/src/config_descriptor.rs`, re-exported at
  `sdk/streamlib-sdk/src/lib.rs:153`) and `ConfigFieldOutput`
  (`runtime/streamlib-engine/src/core/json_schema.rs:218-230`) are dead: nothing derives
  or constructs them.
- `schemars = "0.8"` (0.8.22 in `Cargo.lock`) is a dependency of the engine
  (`runtime/streamlib-engine/Cargo.toml:107`) and the schema crate; neither the engine
  (`runtime/streamlib-engine/src/lib.rs:18-20`) nor the SDK facade
  (`sdk/streamlib-sdk/src/lib.rs:136-142`) re-exports it, and neither the media crate nor
  the api-server depends on it. Its derive honours `#[serde(default = "fn")]`,
  `rename_all`, `skip_serializing_if` and container `default`; a `#[serde(with = …)]`
  field needs `#[schemars(with = …)]` to say what it is.
- Every in-tree `config =` type is derivable: eleven in `runtime/streamlib-media-builtins`
  with fields of `u32`, `String`, `Option<…>`, `PathBuf` and three local enums
  (`OpusEncoderApplication`, `VirtualCameraDoor`, `DisplayScaling`), plus the control
  plane's own `ApiServerConfig` (`runtime/streamlib-api-server/src/api_server.rs:114-118`).
  A processor with no `config =` gets `EmptyConfig`
  (`runtime/streamlib-engine/src/core/processors/mod.rs:36-58`), a hand-written serde
  unit struct with no schema.
- Python: `@processor` reads nothing about `__init__`
  (`sdk/streamlib-python-wheel/python/streamlib/_processor_declaration.py:377-395`) and
  `description=""` has no docstring fallback (`:328`, `:386`). The helper constructs with
  `processor_class(**keyword_arguments)`
  (`sdk/streamlib-python-wheel/python/streamlib/_processor_hosting.py:33`) and
  reconfigures with `configure(**…)` (`:54`); the only construction site in the tree is
  `_helper.py:522`. No test calls the hosting module directly; no class defines
  `configure`.
- Python registration is descriptor and constructor together —
  `PROCESSOR_REGISTRY.register_dynamic(descriptor, Box::new(move |node| spawn_host…))` at
  `sdk/streamlib-python-wheel/src/python_processor_registration.rs:90-108`, from `rt.add`
  (`python_runtime_lifecycle.rs:278-320`) and from the unregistered-type resolver
  (`:227-247`). `register_descriptor_only`
  (`runtime/streamlib-engine/src/core/processors/processor_instance_factory.rs:402-435`)
  shares the descriptor map and refuses a second registration of one path, so a
  descriptor registered at decoration followed by today's `register_dynamic` at add would
  meet `duplicate_class_import_path` (`:583`).
- A helper is recognisable at import time: the child runs `python -m streamlib._helper`
  with `STREAMLIB_ENTRYPOINT` and `STREAMLIB_PROCESSOR_ID` in its environment
  (`python_helper_process_spawn_host.rs:165-190`; constants at `_helper.py:48-52`).
- Twenty-four Python processors take keyword configuration: six engine-tree fixtures
  (`capability_context_probes.py:98`, `helper_placement_processors.py:21`,
  `helper_process_probes.py:21`, `single_processor_under_test.py:38`,
  `texture_ring_producer_probes.py:70`, and the string fixture at
  `test_live_graph_mutation.py:78`), fourteen in `examples/`, four in the MoQ and WebRTC
  wheels with required parameters. `streamlib.testing` never constructs a class itself
  (`testing.py:106`).
- `/api/registry` renders `ProcessorDescriptorOutput` (`json_schema.rs:188-216`,
  `handlers.rs:357-373`). `dist/schemas/openapi.json` is regenerated by the
  `generate_openapi` bin and is not diff-gated; the served-equals-generated test is
  `handlers.rs:781-796`.

## ADDED: §Processor model — the config schema, Rust

- **The descriptor carries a schema document.** `ProcessorDescriptor.config_schema`
  becomes `Option<serde_json::Value>` — the JSON Schema of the config type, `None` only
  on a descriptor built by hand without one — and `with_config_schema` takes the
  document. `ProcessorDescriptorOutput.config_schema` mirrors it, so `/api/registry`
  serves the same document the MCP catalog will (`json_schema.rs:207-209`, `:417`);
  `dist/schemas/openapi.json` is regenerated in the same PR.
- **The macro emits the schema and names the missing derive.** `#[processor]` emits
  `.with_config_schema(<schema of #config_type>)` through the SDK's re-export —
  `__streamlib_sdk::schemars::schema_for!` serialized with `__streamlib_sdk::serde_json`
  — at the site that emits the id today (`codegen.rs:625-630`). A config type that does
  not implement `JsonSchema` fails to compile with a message naming the fix, through a
  bound-carrying helper trait in the engine with a `#[diagnostic::on_unimplemented]`
  note ("derive `JsonSchema` on `<Type>`, re-exported at `streamlib::sdk::schemars`")
  rather than a bare trait-bound error.
- **The SDK re-exports `schemars`.** `pub use schemars;` beside `serde_json` in the engine
  (`lib.rs:18-20`) and `pub use streamlib_engine::schemars;` in the facade
  (`sdk/streamlib-sdk/src/lib.rs:~140`). A third-party processor crate writes
  `#[derive(streamlib::sdk::schemars::JsonSchema)]` with
  `#[schemars(crate = "streamlib::sdk::schemars")]` and adds nothing to its `Cargo.toml`;
  first-party crates may depend on `schemars` directly.
- **Every in-tree config type derives it.** The eleven built-in configs, their three
  enums, and `ApiServerConfig`. `EmptyConfig` gains a `JsonSchema` impl — an object with
  no properties and `additionalProperties: false` — and its `Deserialize` refuses a
  non-empty map by name, so a processor that declares no config refuses one in Rust as
  the plan states (`:596-597`).
- **Descriptions come from doc comments.** schemars lifts `///` on a field into
  `description`, `#[serde(default = …)]` into `default`, and a field without a default
  into `required` — so the eleven configs need no text they do not already carry.

## ADDED: §Processor model — the config class, Python

- **One config parameter, one class.** `@processor` reads
  `typing.get_type_hints(cls.__init__, include_extras=True)`. An `__init__` with a
  `config` parameter names the config class by that annotation; an `__init__` with no
  parameters beyond `self` declares no config; any other signature — keyword
  parameters, an unannotated `config`, a `config` whose annotation is not a class — is
  refused at decoration with a message naming the class, the offending parameter and the
  fix. The class and the derived schema are stamped as
  `__streamlib_processor_config_class__` and `__streamlib_processor_config_schema__`
  beside the existing `__streamlib_processor_*__` attributes (`_processor_declaration.py:385-395`).
- **The schema is derived by the wheel with no dependency.** A `TypedDict` yields
  `properties` from its annotations and `required` from `__required_keys__`; a dataclass
  yields `properties` from its `init=True` fields only — an `init=False` field is not a
  constructor input and is not documented — `default` from a field's default, a field
  with a `default_factory` counted as optional, and `required` from the absence of both; an `Annotated[T, "text"]` string on a field is its
  `description`; a class exposing `model_json_schema()` (pydantic, duck-typed, never
  imported) contributes that document verbatim. Type mapping is the obvious one —
  `int`/`float`/`str`/`bool` to `integer`/`number`/`string`/`boolean`, `list[T]` to
  `array`, `dict[str, T]` to `object`, `Optional[T]` to a nullable, `Literal[…]` to
  `enum`; an annotation the mapper does not know renders `{}` for that key rather than
  refusing the class.
- **The helper constructs the class and hands the object in.**
  `construct_processor_instance` (`_processor_hosting.py:18-39`) becomes: a class
  declaring no config refuses a non-empty configuration by name; otherwise
  `processor_class(config=config_class(**configuration))`, and whatever the config
  class raises is what the author sees — a dataclass's `TypeError` for an unknown key, a
  pydantic `ValidationError`, a `TypedDict` accepting the mapping as itself. Construction
  is the only check the wheel performs, and how strict it is is the author's choice of
  config class, the same dial `read(port, into=T)` already is (`ARCHITECTURE.md:454-463`):
  a `TypedDict` admits anything, a dataclass refuses an unknown key but not a mistyped
  value, a model validates values. The wheel adds no validator of its own; the schema is
  what the agent reads to get construction right, not a gate the wheel enforces. `apply_configuration` (`:42-54`) calls
  `configure(config_class(**configuration))`; the refusal text names `configure(self,
  config)`. The wire is unchanged: `rt.add(cls, config={…})` still carries a dict, the
  graph node still stores JSON, `ctx.config` is still the mapping.
- **The stub follows.** `Runtime.add`'s docstring (`_engine.pyi:461-468`) says the dict
  is constructed into the class's config class; the `processor` decorator's doc names the
  `config` rule; `stubtest` and pyright gate both as today.
- **The six engine-tree fixtures migrate** to a config class in the change; the string
  fixture in `test_live_graph_mutation.py` with them. The fourteen example processors
  ~~and the four extension-wheel processors~~ lag as §Consumers states
  (`docs/plan/ARCHITECTURE.md:327-436`: consumers are never in a migration's scope; a
  converted consumer's breakage is filed as tracked backlog at that consumer), with the
  backlog issues filed at ship naming each file. — Amended 2026-09-11 by the owner's
  ruling that the four extension-wheel processors migrate in the same PR as the engine
  half (#2222), as the deliberate canary §Consumers reserves at `:430-433` for in-flight
  work: `packages/` is the only consumer tree with a CI lane, so migrating it is what
  proves the new construction path on real processors rather than on fixtures alone, and
  it keeps that lane green. Only the fourteen example processors owe backlog at ship.

## ADDED: §Processor model — declaration registers

- **`@processor` registers the descriptor.** After stamping the attributes the decorator
  calls one new wheel-internal entry, `_engine.register_declared_processor_class(cls)`,
  which reads them as `PythonProcessorDeclaration::read_from_class` does today
  (`python_processor_declaration.rs:31-52`), adds `config_schema`, and calls
  `PROCESSOR_REGISTRY.register_descriptor_only`. A duplicate import path meets the
  existing refusal at import time, where `importlib.reload` is the usual cause.
- **The decorator registers nothing inside a helper.** It reads
  `STREAMLIB_ENTRYPOINT` from the environment (`_helper.py:48-52`) and, when present,
  stamps the attributes and stops — a helper hosts no graph.
- **The constructor arrives at first add, onto the registered descriptor.**
  `ProcessorInstanceFactory` gains
  `install_constructor_for_registered_descriptor(processor_class_import_path,
  constructor)`: succeeds exactly when the path is registered without a constructor,
  refuses a path that already has one with the two-classes-one-path text, refuses an
  unknown path by name. `register_processor_class`
  (`python_processor_registration.rs:37-108`) calls it instead of `register_dynamic`;
  the unregistered-type resolver (`python_runtime_lifecycle.rs:227-247`) is unchanged in
  shape — importing the module runs the decorator, which registers, then the resolver
  installs the constructor.
- **Description falls back to the docstring.** A class decorated with no `description=`
  registers `inspect.getdoc(cls)` or `""` — stated below as an assumption.

## MODIFIED: what the tree already says, and now says differently

- **`#[processor]` grammar** (`grammar.rs:4-27`, `:69-81`, `:200-251`, `:643-665`): the
  `config_schema` attribute key and the `config_schema_id` synthesis are deleted;
  `PROCESSOR_ATTRIBUTE_KEYS` loses `"config_schema"`; the three grammar tests at
  `:810-841` are replaced by tests that the descriptor carries the config type's schema
  and that no-config carries `EmptyConfig`'s.
- **`register_dynamic`'s doc** (`processor_instance_factory.rs:358`) and
  `json_value_to_python_object`'s doc (`python_bag_conversion.rs:266`) stop describing
  config as keyword arguments; `Runtime.add`'s doc (`python_runtime_lifecycle.rs:272-276`)
  likewise.
- **`/api/registry`** renders `config_schema` as a document (`openapi.json:294-299`
  regenerated); `graph` is unchanged — a node's `config` stays the JSON it was added with.
- **This is a breaking release, pre-1.0.** `ProcessorDescriptor.config_schema`,
  `with_config_schema` and the `ConfigDescriptor` re-export are public Rust API and
  keyword configuration is public Python API; each implementing PR that changes one
  carries the conventional `!` marker so release-please cuts the next minor as the
  breaking line. No compatibility path: pre-1.0 renames cleanly and ships no shims
  (CLAUDE.md §Non-negotiables).
- **§Control plane's MCP entry** gains, at ship, the factual sentence #2215 already owes
  it: the node serves the processor catalog and the live graph as resources and four
  recipes as prompts beside its tools.

## Assumptions stated, not asked

- **One dialect, served.** Every catalog document is JSON Schema draft 2020-12 with no
  `$schema` key: `schemars` 0.8 emits draft-07, so the descriptor seam renames
  `definitions` to `$defs` and rewrites the `#/definitions/` references, which is the
  whole of the difference in what it emits (nullable is already `"type": [T, "null"]`
  and valid in both); the wheel's mapper emits 2020-12 directly, with `Optional[T]` as
  `anyOf` with `{"type": "null"}` and nested classes inlined, never `$ref`; a model's
  own document (pydantic v2 is 2020-12 already) is taken verbatim minus `$schema` and
  `title`. `additionalProperties` is stated only where the config class refuses unknown
  keys — a dataclass and a Rust struct with `deny_unknown_fields` — and omitted, meaning
  open, elsewhere. One normalizing function in the engine, one test per emitter.
- **Docstring fallback for `description`.** New behaviour the plan does not state; every
  agent SDK surveyed does it (`fn.__doc__` in the MCP Python SDK, griffe in Pydantic AI
  and the OpenAI SDK). Reversible in one line.
- **No schema validation in the wheel.** The wheel is stdlib-only; the config class
  decides strictness exactly as the read target does today, and a JSON Schema validator
  dependency would add a second opinion the author did not ask for.
- **`ctx.config` stays the mapping.** The processor holds the object it was constructed
  with; the context's `config` is the raw dict the graph node stores.

## Relationship to #2215 and #2217

Ticket #2215 renders what this change produces: the catalog resource reads
`PROCESSOR_REGISTRY.list_registered()` and serves `config_schema` as the descriptor now
carries it, and the prompts
name only tools the node serves. `/derive-tickets` may fold #2215 in as the control-plane
slice or keep it as the consumer ticket that lands after the descriptor slice; either
way it is one place the MCP surface changes. #2217 owns what a port reports and blocks
nothing here.

## REMOVED

- REMOVED: config_schema_id
  The synthesized id and its attribute key; the descriptor carries a document instead.
- REMOVED: ConfigDescriptor
  The dead per-field trait, its derive macro, and the SDK re-export at
  `sdk/streamlib-sdk/src/lib.rs:153`.
- REMOVED: sdk/streamlib-macros/src/config_descriptor.rs
- REMOVED: ConfigFieldOutput
  Dead mirror type at `json_schema.rs:218-230` and its `From` impl.
- REMOVED: ConfigField
  The dead per-field struct at `descriptors.rs:90-113`; it goes with the trait, and the
  pattern also covers its engine mirror.
- REMOVED: _as_keyword_arguments
  Keyword-argument construction in `_processor_hosting.py`; config is one object.
- REMOVED: keyword arguments to the class
  The construction refusal's old explanation; the new one names the config class.
- REMOVED: on it to accept updates
  The reconfiguration refusal's old spelling; the new one names `configure(self, config)`.
