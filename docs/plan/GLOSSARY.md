# Glossary

The shared language. Terms only — zero implementation detail. Maintained by the
`glossary` skill inside plan-editing sessions; project-specific terms only.

**The wheel**: the single distributed artifact for Python — Python API + CLI + engine
in one PyO3 package. _Avoid_: "the binary" (pre-pivot), "the SDK" (that is its API
surface). _Amended 2026-09-30 by the one-runtime-per-machine pivot: it becomes two
distributions, a pure-Python stream package and a native runtime package; until the rename,
"the wheel" names what ships today._

Register markers, since the 2026-09-30 pivot: _(user)_ is what a Tatolab user reads and
types; _(engine)_ is what the runtime's code, the plan and the decision records say, and
never a user surface; _(crosses)_ means one meaning on both sides. An unmarked entry
predates the markers.

**Stream** _(user)_: the unit a person writes and runs — today's app: a function decorated
`@stream` that builds a graph, in a directory with `pyproject.toml` and one venv that may hold
several; named by its function; it may expose ports. _Avoid_: "app", "pipeline", "dataflow", "workflow", "graph"
for the unit.

**Pack** _(user)_: one ordinary Python distribution carrying nodes and streams for others to
use — the unit a registry lists and `add` installs into a project, never beside the runtime.
_Avoid_: "plugin", "module", "custom node" (ComfyUI's word); an extension wheel is a pack that
also brings a capability.

**Package**: an ordinary PyPI or cargo package. A processor package's native internals
expose handles to Python and never speak streamlib internals. _Avoid_: "plugin",
"module" (pre-pivot module-system terms).

**Built-in**: a first-party native processor shipped inside the wheel (camera, display,
audio, the seven codec blocks, the virtual camera) — instantiated and configured from
Python; its per-frame path never enters the interpreter. Since the 2026-09-04
extension-model pivot, not the default home for a first-party capability: a new built-in
must meet the criterion in §Packages & extension model — a deadline the interpreter hop cannot
meet, an engine-only primitive, or an OS-facing device the wheel must present to other
applications, and a named consumer. _Avoid_: "built-in" for an optional capability (that is an
**extension wheel**).

**Extension wheel**: a separate PyPI package — Rust inside for speed, a Python processor as
the binding — that depends on the `streamlib` wheel as a binary and never builds it from
source. First-party optional capabilities and third-party native code both ship this way.
_Avoid_: "plugin" (pre-pivot ABI), "integration package" (retired), "built-in" (inside the
wheel).

**Processor extension**: an extension wheel's Python processor class whose per-frame work
runs in native code the same wheel carries and which it calls directly; `stream.add(TheClass)`
is its registration and it runs in its own processor interpreter like any Python node.

**Support hook**: the one callable a capability extension exports (`load(host)`) that the
engine runs once in every process taking an engine role. _Avoid_: "plugin init",
"entry point" for the callable (that is how it is declared, not what it is).

**Edge I/O processor**: a source or sink processor that ingests or egresses an
external-world stream at a runtime boundary — WebRTC, MoQ, raw UDP. Not the runtime mesh.
_Avoid_: "transport processor", "network transport" (runtime-to-runtime transport is the
**runtime mesh**).

**Runtime mesh**: the runtimes that have discovered each other over Zenoh under one mesh
name, carrying remote links as engine transport beside iceoryx2 — always on, never a
processor or an extension. _Avoid_: "fabric" (retired for this term), "gateway", "cluster",
"federation".

**Runtime** _(crosses)_: the one program per machine that runs streams — the engine, the
accelerator when one is present, the transports, the local API; installed once, never
constructed by a stream. Runtime mesh, mesh name and cross-runtime links keep their meaning
with one runtime per machine. _Avoid_: "host", "daemon", "server", "node", "engine" on a user
surface (the engine is the library inside it).

**Machine** _(user)_: what a runtime is addressed as — the first chunk of an address,
defaulting to the hostname; a containerised runtime is its own machine. _Avoid_: "host",
"device", "runtime name" (retired).

**Mesh name**: the name of one runtime mesh — one chunk of the channel-name grammar,
`default` unless a runtime names another. Everything a runtime announces lives under it, and
naming a different one is how groups sharing a network separate what they announce and read —
never what dials whom. _Avoid_: "namespace" (Zenoh's own config key, which this is not),
"cluster", "domain" (that is the iceoryx2 one).

**Address** _(crosses)_: the right-anchored path `<machine>/<stream>/<node>/<port>`, leading
chunks omitted to mean "here"; the same string on every surface, so a link whose ends are on
two machines is spelled by two addresses. _Avoid_: "channel name", "mesh address", "service
name", "remote link" (retired).

**Exposed port** _(crosses)_: an output a stream lets leave the machine, readable at its URL by
peers and relays under the stream map. `expose` is the verb — in the stream's function as the
author's suggestion, at the CLI as the owner's decision. _Avoid_: "export", "publish", "endpoint".

**Form** _(user)_: the shape an exposed port is served in — the URL's child chunk (`ndjson`,
`png`, `ts`, `hls`, `whep`, `moq`, `page`). _Avoid_: "format" (a pixel format), "transport".

**Relay** _(crosses)_: a runtime in the relay role — the same software, whose router other
runtimes dial and whose URL browsers reach; a role, not a place. _Avoid_: "gateway", "tower",
"router" unqualified (Zenoh's component inside it).

**Mesh peer**: another runtime on the same runtime mesh. _Avoid_: "node" for a peer (a node is
a step in a stream).

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

**Local API** _(engine)_: the runtime's per-machine control surface over a local socket — for
observing, inspecting and changing the live graphs of its streams — which the CLI, MCP hosts
and an external control client call. Embedding happens by importing the runtime package, never
through the local API. _Avoid_: "control plane" (retired), "API server" as the concept.

**Node catalog** _(user)_: what a runtime reports it can add — every registered node class
with its description, config schema and ports — served over the local API. _Avoid_:
"registry" for the served view (the registry is the process-global table behind it),
"processor catalog" (retired).

**Node** _(user)_: a step in a stream — the class a person writes (`@node` in Python,
`#[node]` in Rust) and each placement of it under a name; "node instance" when the
distinction matters, as for any class. The class is identified by its fully-qualified import
path, so it must live in an importable, side-effect-safe module — a class defined in
`stream.py` (`__main__:<Type>`) is a wiring error. A placed node is what a log line, a crash
and a process id belong to. _Avoid_: "processor" on a user surface, "operator", "element",
"stage", "step", "instance" alone; the live-runtime sense of "node" is retired.

**Node reference** _(user)_: what `stream.add` hands back — a handle carrying the node's name
and its `output()` / `input()` port references; the node itself exists once the stream runs.
_Avoid_: "the instance" for the handle.

**Name** _(crosses)_: a node's or a stream's one-chunk address part — letters, digits, `-`,
`_`, `.`; a node's defaults to its class's short name; never an identity. _Avoid_: "display
name" (retired), "id", "label".

**Graph** _(crosses)_: a stream's nodes, links and exposures as one JSON shape — what a
stream's function compiles to, and what the runtime renders live; emitted, never authored. _Avoid_: "graph description", "stream
description", "snapshot", "manifest", "pipeline file".

**Processor** _(engine)_: the engine's word for a node — its trait, its id, its interpreter;
kept in Rust identifiers until the rename. _Avoid_: on any user surface.

**Processor interpreter** _(engine)_: the interpreter process the runtime execs from the
stream's venv for one Python node. _Avoid_: "helper", "helper process", "worker", "child
interpreter", "sandbox".

**Runtime process** _(engine)_: the runtime's own OS process — engine, local API, built-ins;
it hosts no Python node. _Avoid_: "app-process" (retired), "daemon".

**Stream map** _(engine)_: the policy the runtime enforces on what leaves its machine — which
ports may be linked from outside it, and by which authenticated peers; links to unexposed
ports are refused. It is pushed to the runtime; by whom, and its other details, are OPEN.
_Avoid_: "ACL", "firewall".

**Control client** _(engine)_: an external component that plugs into the runtime through the
pivot's extension point, for example to enrol a machine with a coordination service; the
runtime runs complete without one, and how it plugs in is OPEN. _Avoid_: "connector",
"sidecar", "agent".

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
driver stamps. One per machine, not one per mesh: two stamps from two machines are
readings of two unrelated clocks and subtracting them means nothing, which is what
**clock identity** exists to say. The default; anything a processor stamps or compares
uses it. _Avoid_: "media clock" for the epoch (`MediaClock` is the Rust naming seam, not a
second clock), "timestamp" unqualified where the epoch matters, "one clock" where the
machine matters.

**Clock identity**: which machine's monotonic clock a stamp was taken on — the kernel's
boot-session UUID (`/proc/sys/kernel/random/boot_id`, `kern.bootsessionuuid`), the boot
alone, so a container and its host share one. It rides every mesh message's attachment,
renders on every link in `graph` as `stamp_clock_identity`, and is read by
`inbound_link_stamp_clock_identity(port, link)` against
`this_machines_stamp_clock_identity()`. Two stamps are comparable when their links name the
same identity **and** neither stamp was restated by a relay: what a link names is the clock
of the machine that last wrote the bag, not necessarily of the machine that took the reading,
so a processor restating an upstream stamp on a local output makes its link name this machine
confidently and wrongly. That is the known relay gap, and it is the common-clock OPEN's to
close. _Avoid_: "host identity" (that is the boot id **plus** the pid-namespace inode, and it
settles duplicate runtime names, never stamps), "boot id" unqualified, "clock id", "epoch".

**Wall clock**: UNIX time — permitted only on the three observability surfaces (log
`host_ts`, log `source_ts`, log file naming), because they correlate with the outside
world. Never on the data plane, never compared against a
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

**Pixel effect**: a GPU effect written as one shader function over a frame's pixels, run
by the engine as an ordinary kernel. _Avoid_: "filter", "shader node" (it is not a graph
node of its own).

**Tensor buffer**: an engine-owned GPU buffer with a declared tensor shape and element
type, named by surface id and read by tensor libraries over DLPack. _Avoid_: "float
pixel buffer" (a pixel buffer is image-shaped).

**Floor**: a supported platform the product promises the same capabilities on — Linux +
NVIDIA, and Apple Silicon. A capability absent on one floor is on a closed list and refuses
by name. _Avoid_: "target", "backend" (a floor is the promise, not the driver).

**Cross-floor check**: the source-reading check that names what binds a Python processor
to one floor and the portable spelling for it. _Avoid_: "portability check" (that word
belongs to the **portability gate**).

**Portability gate**: the test that the wheel's native extension links nothing the host
may not supply. _Avoid_: using it for code that runs on only one floor — that is the
**cross-floor check**.

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

Retired by the 2026-09-30 one-runtime-per-machine pivot (see
`docs/decisions/one-runtime-per-machine.md`): **App** — say **stream**; **Node** in its
live-runtime sense — say **runtime**; **Display name** — say **name**; **Runtime name** — a
runtime is addressed by its **machine**; **Remote link** — a link spelled by two addresses;
**Control plane** — say **local API**; **Processor catalog** — say **node catalog**; **Graph
description** — say **graph**; "helper" and "helper process" — say **processor interpreter**;
"processor" on a user surface — say **node**. **Host** stays retired: the pivot's sentences
said "host" for what the glossary calls the **runtime**. The older entries that still use the
retired words are facts about the shipped tree, read through the plan's reading rule until the
rename change re-spells them.
