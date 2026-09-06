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
audio, the seven codec blocks, the virtual camera) — instantiated and configured from
Python; its per-frame path never enters the interpreter. Since the 2026-09-04
extension-model pivot, not the default home for a first-party capability: a new built-in
must meet the criterion in §Packages & extension model — a deadline the helper hop cannot
meet, an engine-only primitive, or an OS-facing device the wheel must present to other
applications, and a named consumer. _Avoid_: "built-in" for an optional capability (that is an
**extension wheel**).

**Extension wheel**: a separate PyPI package — Rust inside for speed, a Python processor as
the binding — that depends on the `streamlib` wheel as a binary and never builds it from
source. First-party optional capabilities and third-party native code both ship this way.
_Avoid_: "plugin" (pre-pivot ABI), "integration package" (retired), "built-in" (inside the
wheel).

**Processor extension**: an extension wheel's Python processor class whose per-frame work
runs in native code the same wheel carries and which it calls directly; `rt.add(TheClass)`
is its registration and it runs in its own helper like any Python processor.

**Support hook**: the one callable a capability extension exports (`load(host)`) that the
engine runs once in every process taking an engine role. _Avoid_: "plugin init",
"entry point" for the callable (that is how it is declared, not what it is).

**Edge I/O processor**: a source or sink processor that ingests or egresses an
external-world stream at a runtime boundary — WebRTC, MoQ, raw UDP. Not the
runtime-to-runtime fabric. _Avoid_: "transport processor", "network transport" (that is
the fabric's word).

**Capability extension**: an extension wheel's support code — declared by a standard entry
point in its `pyproject.toml` that pip records and the engine runs once at startup, like
loading a driver — which may bring up a device or network stack, or introduce an
engine-grade capability the engine does not provide (graphics processing, a transport, a
device class; the Unreal-module shape). Sandboxed so two packages cannot unsafely alter
engine features; extends rather than rewrites. _Avoid_: "plugin" unqualified.

**Codec block**: one of the seven codec built-ins that shipped — encoder, decoder, or
muxer inside the wheel (`H264Encoder`, `Mp4Sink`, ...), configured like any built-in; its
per-frame path never enters an interpreter. The next codec follows the built-in criterion
and is not a codec block by default. _Avoid_: "codec processor" (user-authored shape),
"codec plugin" (pre-pivot).

**Conversion**: rewriting a pre-pivot consumer from scratch in the current idiom — the
old directory mined for logic only and deleted in the same PR. _Avoid_: "upgrade",
"port" (both imply editing the old form in place).

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

**Cast object**: the typed object `read(port, into=T)` constructs from a bag — the
consumer's view of a payload. A cast type that claims its surface is also the
tensor-protocol producer for that frame. _Avoid_: "typed bag", "frame object" (a cast
type need not be a frame).

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

**Delivery profile**: the consuming input port's read policy — `newest` (drain to the
most recent bag) or `ordered` (receive bags in publication order). Declared explicitly on
every input port; there is no default. Names a read policy only: both drop under
pressure, and depth and overflow are engine-chosen. _Avoid_: "QoS", "channel mode",
"queue"; and never a word implying guaranteed delivery — `lossless` and `every_sample`
are retired for exactly that (see [[delivery-profile-vocabulary]]).

**Dropped bag**: a bag a port discarded under pressure, counted by that port and reported
in `graph`. Distinct from a **tap's** `dropped_bags`, which counts what the tap's own
reserved subscriber slot missed — different subject, and the two are never summed.

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
OPAQUE_FD (imports only through Vulkan / CUDA external memory). Chosen when the
allocation is created, never convertible; a format with no DRM FOURCC can only ever
be OPAQUE_FD. _Avoid_: "format" for the handle type (format constrains the flavour;
it does not name it).

**Raw export**: handing a surface allocation's memory fd itself to native code, as
opposed to an engine-ordered view. A raw handle names the allocation, never the
frame — the surface-id lifetime guarantees end at export. _Avoid_: "export"
unqualified where the allocation-vs-frame distinction matters.

**Present target**: the engine-owned presentation surface minted from a raw window
handle; the only way frames reach a window.

**Processor-owned window**: a window a processor requested from the engine and owns the
policy of — title, extent, which frame it shows, what close means. For an owner outside
the app process, the engine runs the window's native present loop, fed by surface ids
the owner names. _Avoid_: "debug window" as the concept (a use, not the capability).

**Kernel**: a GPU program the engine compiles and runs on its device — compute,
graphics, or ray-tracing. _Avoid_: "shader" for the whole object (that is its source
text), "pipeline" (the Vulkan-internal object it builds).

**AudioBlock**: the audio bag and its cast — a timestamped run of interleaved samples
riding the link inline (first-sample timestamp, rate, channels, dtype; the sample
count is per channel), CPU-resident, never surface-backed. _Avoid_: "AudioFrame"
(the dead schema-era type; in device APIs a frame is one sample across channels),
"audio chunk".

**Window contract**: an audio input port's declared rate / channels / dtype / window /
hop — window and hop in samples at the declared rate — the engine resamples, mixes
down, and frames to it natively, so `process()` receives exact-size blocks.
_Avoid_: "windower" as an object name (it is a port declaration, not a graph node).

**Conditioning**: the engine-internal AEC / noise-suppression / AGC chain between an
audio device and its published `AudioBlock`, declared on the built-ins and bypassable.
_Avoid_: "effects" (production vocabulary; conditioning serves perception).

**Audio plugin**: a third-party CLAP / VST3 / LV2 binary a future out-of-process
helper would run, declared project-locally. Always qualified — bare **Plugin** stays
retired (it named StreamLib's deleted plugin ABI, a different thing).

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

Retired by the 2026-09-04 extension-model pivot (see `docs/decisions/extension-model.md`):
**Integration package** — say **extension wheel**; and "built-in" as the default home for
a first-party capability — a built-in is now the exception the criterion admits.
