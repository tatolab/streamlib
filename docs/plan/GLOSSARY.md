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

**Placement**: settled, not an axis — every Python processor runs in its own helper
process (own interpreter, own GIL), spawned by the engine as an exec of
`sys.executable` from the app's venv. There is no second placement and no choice:
in-process hosting of a Python processor does not exist. Native built-ins running in
the app process ("app-process" code) are not a placement decision. _Avoid_:
"in-process placement", "both placements", "placement policy", "placement heuristic",
"transparent move".

**App-process**: the process that runs the entry file, the engine, the control plane,
and the native built-ins — and hosts no Python processor. Use this word for the
legitimate in-that-process senses so "in-process" stops doing double duty.

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
its fully-qualified import path, which requires the class to live in an importable,
side-effect-safe module; a class defined in the entry file (`__main__:<Type>`) is a
wiring error. _Avoid_: an authored `@org/package/Type` name.

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

**Monotonic clock**: the machine's boot-relative clock (`CLOCK_MONOTONIC` /
`mach_absolute_time`) — the epoch of every data-plane timestamp and of the V4L2 and ALSA
driver stamps. The default; anything a processor stamps or compares uses it. _Avoid_:
"media clock" for the epoch (`MediaClock` is the Rust naming seam, not a second clock),
"timestamp" unqualified where the epoch matters.

**Wall clock**: UNIX time — permitted only on the four observability surfaces (log
`host_ts`, log `source_ts`, log file naming, control-plane event timestamp), because they
correlate with the outside world. Never on the data plane, never compared against a
monotonic timestamp. _Avoid_: "system time", "real time".

**Engine primitive**: a hardware capability the engine owns and exposes through its
handle-shaped surface — GPU memory import/export (DMA-BUF / OPAQUE_FD), the present
target, texture rings, codec sessions, the audio clock, color resolution. Built-ins and
external code compose primitives; they never reimplement them.

**Handle**: a transferable value crossing the native↔Python boundary — a DMA-BUF fd, an
exportable device allocation (OPAQUE_FD), a surface id, a byte buffer. An
address-space-local pointer is not a handle. Pixels never cross as Python objects.

**Handle flavour**: which external-memory handle type a texture's allocation exports
as — DMA-BUF (explicit-DRM-modifier images, importable by EGL / V4L2 consumers) or
OPAQUE_FD (formats with no DRM FOURCC; imports only through Vulkan / CUDA external
memory). Fixed when the allocation is created, never convertible. _Avoid_: "format"
for the handle type (a format implies a flavour; they are not the same axis).

**Raw export**: handing a surface allocation's memory fd itself to native code, as
opposed to an engine-ordered view. A raw handle names the allocation, never the
frame — the surface-id lifetime guarantees end at export — and is minted only by
the Full capability surface. _Avoid_: "export" unqualified where the
allocation-vs-frame distinction matters.

**Present target**: the engine-owned presentation surface minted from a raw window
handle; the only way frames reach a window.

**Kernel**: a GPU program the engine compiles and runs on its device — compute,
graphics, or ray-tracing. _Avoid_: "shader" for the whole object (that is its source
text), "pipeline" (the Vulkan-internal object it builds).

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

Retired by the 2026-08-04 helper-placement pivot (see
`docs/decisions/helper-process-placement-only.md`): **In-process placement**,
**Placement policy**, **Placement heuristic**, **Transparent move** — there is one
placement, so these name nothing. For the surviving in-that-process Rust senses
(engine, control plane, built-ins, interop adapters), say **app-process**.
