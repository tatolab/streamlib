# Glossary

The shared language. Terms only — zero implementation detail. Maintained by the
`glossary` skill inside plan-editing sessions; project-specific terms only.

**The wheel**: the single distributed artifact for Python — Python API + CLI + engine
in one PyO3 package. _Avoid_: "the binary" (pre-pivot), "the SDK" (that is its API
surface).

**App**: a normal Python codebase — an entry file (`app.py` defining `setup(rt)`) plus
`pyproject.toml` and one venv; no manifest, no streamlib-specific files. _Avoid_:
"project", "consumer app" (redundant).

**Package**: an ordinary PyPI or cargo package. A processor package's native internals
expose handles to Python and never speak streamlib internals. _Avoid_: "plugin",
"module" (pre-pivot module-system terms).

**Built-in**: a first-party native processor shipped inside the wheel (camera, display,
audio) — instantiated and configured from Python; its per-frame path never enters the
interpreter.

**Placement**: where the engine runs a processor — in-process, or in a helper process
spawned from the same interpreter and venv. An engine decision behind a single opt-in,
never a user-facing runtime definition.

**Bag**: the self-describing msgpack named map a link carries — the schema-free view of
a payload; consumers cast it to a type at read time. _Avoid_: "message", "envelope".

**Control plane**: the HTTP/WebSocket/MCP surface a runtime hosts for observing and
inspecting running nodes; the CLI is its client. Embedding happens by importing the
wheel, never through the control plane. _Avoid_: "API server" as the concept (that is
the component hosting it).

**Node**: a live runtime reachable over its control plane; discovered via the per-user
on-disk registry. _Avoid_: "instance", "server".

**Processor**: the unit of pipeline computation — a Python class (`@processor`) or a
Rust type (`#[processor]`) — wired by ports. Its identity is the class itself, named by
its fully-qualified import path. _Avoid_: an authored `@org/package/Type` name.

**Display name**: an instance's human-facing label — passed at `add`, prefixing its log
records, defaulting to the class's short name. Never an identity. _Avoid_: "processor
name", "id".

**Port**: a processor's named attachment point for a link. Declares name, description,
and — on an input — delivery profile; never a type. _Avoid_: "channel" for the port
itself.

**Link**: one wired connection, output port → input port, carrying bags. _Avoid_:
"edge", "connection", "pipe".

**Delivery profile**: the consuming input port's policy for which bags it receives —
`latest`, `every_sample`, or `lossless`. Declared explicitly on every input port; there
is no default. _Avoid_: "QoS", "channel mode".

**Engine primitive**: a hardware capability the engine owns and exposes through its
handle-shaped surface — GPU memory import/export (DMA-BUF / OPAQUE_FD), the present
target, texture rings, codec sessions, the audio clock, color resolution. Built-ins and
external code compose primitives; they never reimplement them.

**Handle**: a transferable value crossing the native↔Python boundary — a DMA-BUF fd, a
CUDA device pointer, a surface id, a byte buffer. Pixels never cross as Python objects.

**Present target**: the engine-owned presentation surface minted from a raw window
handle; the only way frames reach a window.

**The plan**: `docs/plan/ARCHITECTURE.md` plus `docs/plan/diagrams/` — the single source
of architectural decisions.

**Change**: a typed delta proposal against the plan (`docs/plan/changes/`), marked
ADDED / MODIFIED / REMOVED.

Retired by the 2026-08-02 pivot (see `docs/decisions/importable-python-library.md`):
**Host** (loads-plugins sense), **Plugin**, **Plugin ABI**, **Package source**,
**Link** (the CLI verb — the local-dev install path, not the connection above),
**Lag by design** — these named the deleted plugin-ABI / module-system world; do not
reuse them.

Retired by the 2026-08-03 schema-free-ports decision (see
`docs/decisions/schema-free-ports.md`): **Schema**, **SchemaIdent**, **Schema
agreement**, **Wire tag**, **Flow class** — the engine has no type layer, so these name
nothing. Type information is the authoring language's and has no project-specific term.
