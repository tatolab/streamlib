# Glossary

The shared language. Terms only — zero implementation detail. Maintained by the
`glossary` skill inside plan-editing sessions; project-specific terms only.

**Stream package** _(crosses)_: `tatolab-stream`, the pure-Python distribution a stream's
venv installs — everything a stream module imports; a stream compiles with no runtime
present. _Avoid_: "the SDK", "the wheel".

**Runtime portion** _(engine)_: `tatolab.runtime`, the engine's native part — ships only with
the runtime, never through pip. _Avoid_: "the runtime package" on a user surface (users never
install it), "the engine wheel".

**The lend** _(engine)_: how a processor interpreter sees the runtime portion — the runtime
prepends its lend directory, the one holding `tatolab/runtime/`, to the interpreter's
`PYTHONPATH`, so PEP 420 merges that `tatolab/runtime/` with the venv's `tatolab/stream/`. _Avoid_: "injection", "shim", "vendoring".

Register markers, since the 2026-09-30 pivot: _(user)_ is what a Tatolab user reads and
types; _(engine)_ is what the runtime's code, the plan and the decision records say, and
never a user surface; _(crosses)_ means one meaning on both sides. An unmarked entry
predates the markers.

**Stream** _(user)_: the unit a person writes and runs — today's app: a function decorated
`@stream` that builds a graph, in a directory with `pyproject.toml` and one venv that may hold
several; it may expose ports. The function builds it through the `StreamBuilder` it is handed.
Once loaded it persists until stopped or removed. _Avoid_: "app", "pipeline", "dataflow",
"workflow", "graph" for the unit; "attached stream", "kept stream", "detach" (every loaded
stream persists).

**Project** _(user)_: a directory with `pyproject.toml` and one venv whose `stream.py` defines
one or more streams; `tatolab new` and `create_project` make one, its scaffold holding one
stream. Loading a project loads every stream in it, each its own unit.
_Avoid_: "stream project" (a project holds many streams), "app".

**Location** _(crosses)_: what a stream is — its project directory and its stream function.
Loading the same location again replaces the stream; a moved directory is a new stream.
_Avoid_: "source" (the relay source node's word), "origin" (MoQ's), "path" alone.

**Stream id** _(crosses)_: the handle computed from a stream's location — the folder, the
function and a short hash — and the stream part of every address; never chosen, never
renamed. _Avoid_: "stream name", "name" for the handle.

**Display name** _(user)_: the label a person reads for a stream or a pack — never an
address or a key, never unique; a stream's is recorded by the runtime and renamable.
_Avoid_: "name" for the label where a stream id could be meant.

**Pack** _(user)_: one ordinary Python distribution carrying nodes and streams for others to
use — the unit a registry lists and `add` installs into a project, never beside the runtime.
_Avoid_: "plugin", "module", "custom node" (ComfyUI's word); an extension wheel is a pack that
also brings a capability.

**Package**: an ordinary PyPI or cargo package. A processor package's native internals
expose handles to Python and never speak streamlib internals. _Avoid_: "plugin",
"module" (pre-pivot module-system terms).

**Built-in**: a first-party native processor shipped inside the runtime, never on pip (camera,
display, audio, the seven codec blocks, the virtual camera) — added and configured from
Python through the class `tatolab.stream` carries generated for it, its type that class's import
path (`tatolab.stream:CameraSource`), never where its native code lives; its per-frame path
never enters an interpreter. Since the 2026-09-04 extension-model pivot, not the default home
for a first-party capability: a new built-in must meet the criterion in §Packages & extension
model — a deadline the interpreter hop cannot meet, an engine-only primitive, or an OS-facing
device the runtime must present to other applications, and a named consumer. _Avoid_:
"built-in" for an optional capability (that is an **extension wheel**).

**Extension wheel**: a separate PyPI package — Rust inside for speed, a Python node as the
binding — that depends on `tatolab-stream` and never on the runtime; a first-party one is
`tatolab-<name>` importing as `tatolab.<name>`, and its nodes run in processor interpreters
where `tatolab.runtime` is lent. First-party optional capabilities and third-party native
code both ship this way. _Avoid_: "plugin" (pre-pivot ABI), "integration package" (retired),
"built-in" (inside the runtime).

**Processor extension**: an extension wheel's Python processor class whose per-frame work
runs in native code the same wheel carries and which it calls directly;
`stream_builder.add(TheClass)` adds it and it runs in its own processor interpreter like any
Python node.

**Edge I/O processor**: a source or sink processor that ingests or egresses an
external-world stream — WebRTC, raw UDP, and the **relay source node**. Sending an exposed
port to relays is the runtime's **publisher**; reading one back into a graph is a node.
_Avoid_: "transport processor", "network transport".

**Publisher** _(engine)_: the runtime's one component, in `tatolabd` beside the adapters, that
forwards every exposed port's messages to the relays its level reaches, holding no logic —
bytes as written, anything exposed, groups from the envelope, whole groups dropped under
backpressure or a reported loss, the catalog from the ports' entries. _Avoid_: "publish" as the user's verb
(that is **expose**), "broadcaster".

**Relay adapter** _(engine)_: one crate implementing the relay-adapter SDK's trait, carrying
what differs between relays — dialect, credential, root, its own settings — compiled into
`tatolabd`, any number of instances each, recorded in `relays.json`. Local, Cloudflare,
moq.pro; Red5 on hold. _Avoid_: "connector", "sink".

**Relay source node** _(user)_: the built-in that subscribes to one track at a relay and
writes its payloads out as received; the one way a graph reads a relay, on this machine or
another. _Avoid_: "remote link", "remote_output" (retired 2026-10-16), "subscriber node".

**Broadcast** / **Track**: MoQ's words, used as they are — a stream is one broadcast at a
relay, each exposed port one track in it. The relay source node names what it reads by them alone,
`broadcast` and `track`, each read exactly as its publisher wrote it: the runtime casts the names it
gives, never a name it reads (owner, 2026-10-18). _Avoid_: "channel" (iceoryx2's word), "topic",
"track address" / `track_address` (retired 2026-10-18 — an invented term for what MoQ already
names).

**Tailnet** _(user)_: the Tailscale network a machine may belong to. A docs word only: the
runtime reads, configures and requires nothing of Tailscale, and a tailnet reaches a machine's
local relay as any network does. _Avoid_: "mesh" (retired), "VPN", "network" unqualified.

**Runtime** _(crosses)_: the one program per machine that runs streams — the engine, the
accelerator when one is present, the transports, the local API; installed once, never
constructed by a stream. Its program is `tatolabd`, as Tailscale's is `tailscaled`; the word
users read stays "runtime". _Avoid_: "host", "daemon", "server", "node", "engine" on a user
surface (the engine is the library inside it).

**Machine** _(user)_: what a runtime is addressed as — the first chunk of an address: its
hostname, cast; a containerised runtime is its own machine. _Avoid_: "host", "device", "runtime name" (retired).

**Address** _(crosses)_: the path `<machine>/<stream id>/<node>/<port>`, the same string on
every surface — the `/watch` path names it, and at a relay it is the track `<node>/<port>` of
the broadcast `<machine>/<stream id>.hang`. _Avoid_: "channel name", "mesh address", "service
name", "remote link" (retired).

**Exposure** _(crosses)_: who outside a stream may read one of its output ports — **internal**
(the default: the stream's own nodes only), **private** (any stream or code on the machine) or
**public** (private, plus every **relay** the machine has joined, the local relay's
every-interface listener included). A live permission
the runtime checks at the stream's edge, never part of the stream's logic. `expose` is the verb —
in the stream's function, where the exposures start; at the CLI, the app or the local API, while
it runs. _Avoid_: "visibility" (discovery's word), "export", "publish", "endpoint".

**Exposed port** _(crosses)_: a private or public output port. _Avoid_: "shared port", "open port".

**Form** _(user)_: the shape the runtime's loopback HTTP serves an exposed port in — the
URL's leading chunk, `watch`, before the address. Live data is a track at a relay, never a
form, and no snapshot form is built. _Avoid_: "format" (a pixel format), "transport".

**Preview page** _(user)_: the lightweight built-in viewer the runtime serves on loopback at
`/watch/<address>`, the stack's own player over the local relay; not a product. _Avoid_:
"viewer page" (retired 2026-10-16), "dashboard", "console".

**Relay** _(crosses)_: a separate program a machine joins so its exposed ports can be read —
the **local relay** `tatolabd` runs as a child for every machine, and the hosted ones a
machine joins for an internet address. Never the runtime. _Avoid_: "gateway", "tower",
"router", "endpoint".

**Codec block**: one of the codec built-ins — encoder or decoder inside the runtime
(`H264Encoder`, `OpusDecoder`, ...), configured like any built-in; its per-frame path never
enters an interpreter. There is no muxer: a stream's last mile is a MoQ client's. The next
codec follows the built-in criterion and is not a codec block by default. _Avoid_: "codec
processor" (user-authored shape), "codec plugin" (pre-pivot).

**Conversion**: rewriting a pre-pivot consumer from scratch in the current idiom — the
old directory mined for logic only and deleted in the same PR. _Avoid_: "upgrade",
"port" (both imply editing the old form in place).

**Placement**: settled, not an axis — every Python processor runs in its own helper
process (own interpreter, own GIL), spawned by the engine as an exec of the stream's own
venv interpreter, with the runtime's lend directory first on `PYTHONPATH`. There is no second
placement and no choice:
in-process hosting of a Python processor does not exist. Native built-ins running in
the app process ("app-process" code) are not a placement decision. _Avoid_:
"in-process placement", "both placements", "placement policy", "placement heuristic",
"transparent move".

**App-process**: the process that runs the entry file, the engine, the control plane,
and the native built-ins — and hosts no Python processor. Use this word for the
legitimate in-that-process senses so "in-process" stops doing double duty. _Amended by
one-runtime-per-machine: the pivot's word is **runtime process**; this one names the shipped
tree until the rename re-spells it._

**Message** _(crosses)_: one thing a link carries — an **envelope** the engine owns (stamp,
length, the group flag) and a **payload**, bytes the engine passes through unread; at a
relay, one object. _Avoid_: "bag" (retired 2026-10-16 — it named the msgpack map that was the whole
payload), "frame" for anything but a video frame.

**Payload**: a message's bytes, in whatever encoding the writer chose and its port's catalog
entry names — msgpack through the stream package's helper, a codec's media frame, the
author's own. _Avoid_: "bag", "body".

**Cast object**: the typed object `read(port, into=T)` constructs from a payload through
`T.from_bytes` — the consumer's view of a message. A cast type that claims its surface is
also the tensor-protocol producer for that frame. _Avoid_: "typed bag", "frame object" (a
cast type need not be a frame).

**Catalog entry** _(crosses)_: what an output port's declaration says about what it writes,
in the layout the stack's players read — the section (`video`, `audio`, `text`, `json`,
`binary`) and that section's own fields — written by the node that owns the port, merged by
the publisher into a broadcast's catalog as text and rendered by `graph`; documentation a
reader follows, never a contract `connect` compares. _Avoid_: "catalog fragment" (retired
2026-10-17 — it named a `kind`/`encoding`/`cast` triple in the runtime's own words),
"schema", "port type", "shape declaration".

**Local API** _(engine)_: the runtime's per-machine control surface over a local socket — for
observing, inspecting and changing the live graphs of its streams — which the CLI, MCP hosts
and an external control client call, reachable only on its own machine. An MCP host reaches it
by launching the CLI's `mcp` verb, which forwards messages to the socket unchanged. Embedding
happens by importing the runtime package, never through the local API. _Avoid_: "control
plane" (retired), "API server" as the concept; "relay" or "bridge" for the `mcp` verb
("relay" is the program a machine joins for an internet address).

**Node catalog** _(user)_: what a runtime reports it can add — every registered node class
with its description, config schema and ports — served over the local API. _Avoid_:
"registry" for the served view (the registry is the process-global table behind it),
"processor catalog" (retired).

**Node** _(user)_: a step in a stream — the class a person writes (`@node` in Python,
`#[node]` in Rust) and each placement of it under a name; "node instance" when the
distinction matters, as for any class. The class is identified by its fully-qualified import
path (a built-in by its class's path in `tatolab.stream`), so a class the project writes must
live in an importable, side-effect-safe module — a class defined in
`stream.py` (`__main__:<Type>`) is a wiring error. A placed node is what a log line, a crash
and a process id belong to. _Avoid_: "processor" on a user surface, "operator", "element",
"stage", "step", "instance" alone; the live-runtime sense of "node" is retired.

**Node reference** _(user)_: what `stream_builder.add` hands back — a handle carrying the node's name
and its `output()` / `input()` port references; the node itself exists once the stream runs.
_Avoid_: "the instance" for the handle.

**Name** _(crosses)_: a machine's, stream's, node's or port's one-chunk address part, cast to
lowercase RFC 3986 unreserved characters (`Front Camera` → `front-camera`) whatever the author
wrote; a node's defaults to its class's short name (`camerasource`); never an identity. _Avoid_: "display
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

**Control client** _(engine)_: an external component that plugs into the runtime through the
pivot's extension point, for example to hand it a relay's address and credential; the
runtime runs complete without one, and how it plugs in is OPEN. _Avoid_: "connector",
"sidecar", "agent".

**Port**: a processor's named attachment point for a link. Declares name, description,
and — on an input — delivery profile; never a type. _Avoid_: "channel" for the port
itself.

**Link**: one wired connection, output port → input port, in one stream, carrying messages. _Avoid_:
"edge", "connection", "pipe".

**Delivery profile**: the consuming input port's read policy — `newest` (drain to the
most recent message) or `ordered` (receive messages in publication order). Declared explicitly on
every input port; there is no default. Names a read policy only: both drop under
pressure, and depth and overflow are engine-chosen. _Avoid_: "QoS", "channel mode",
"queue"; and never a word implying guaranteed delivery — `lossless` and `every_sample`
are retired for exactly that (see [[delivery-profile-vocabulary]]).

**Dropped message**: a message a port discarded under pressure, counted by that port and
reported in `graph` (`dropped_bags_by_link`), and told to the reader's next read from that link
as `lost_since_last_read`.

**Monotonic clock**: the machine's boot-relative clock (`CLOCK_MONOTONIC` /
`mach_absolute_time`) — the epoch of every data-plane timestamp and of the V4L2 and ALSA
driver stamps. One per machine: two stamps from two machines are
readings of two unrelated clocks and subtracting them means nothing. The default; anything a processor stamps or compares
uses it. _Avoid_: "media clock" for the epoch (`MediaClock` is the Rust naming seam, not a
second clock), "timestamp" unqualified where the epoch matters, "one clock" where the
machine matters.

**Wall clock**: UNIX time — permitted only on the five surfaces the plan lists (log
`host_ts`, log `source_ts`, log file naming, the local relay certificate's validity, a relay
token's expiry), because they correlate with the outside world. Never on the data plane, never compared against a
monotonic timestamp. _Avoid_: "system time", "real time".

**Engine primitive**: a hardware capability the engine owns and exposes through its
handle-shaped surface — GPU memory import/export (DMA-BUF / OPAQUE_FD), texture rings, codec sessions, the audio clock, color resolution. Built-ins and
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

**Present target** and **Node-owned window**: retired 2026-10-16 with every window — the
runtime opens none, and a stream is seen through the **preview page**.

**Kernel**: a GPU program the engine compiles and runs on its device — compute,
graphics, or ray-tracing. _Avoid_: "shader" for the whole object (that is its source
text), "pipeline" (the Vulkan-internal object it builds).

**AudioBlock**: the audio message and its cast — a timestamped run of interleaved samples
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
by name. _Avoid_: "target", "backend" (a floor is the promise, not the driver), "floor" for
what the engine requires of a device (that is a device the engine can build on).

**Software device** _(engine)_: the CPU-run Vulkan driver the Linux runtime unit carries so
a device always exists — taken only when no hardware device builds. _Avoid_: "software
rendering", "CPU mode", "fallback mode", "headless mode" (it is a device, not a mode).

**Absent tier** _(engine)_: a capability the device the runtime built on does not
implement — refused by name at the node that asks for it, never a start failure. _Avoid_:
"unsupported", "missing feature", "degraded".

**Cross-floor check**: the source-reading check that names what binds a Python processor
to one floor and the portable spelling for it. _Avoid_: "portability check" (that word
belongs to the **portability gate**).

**Portability gate**: the test that a native binary Tatolab ships — the runtime portion's
`_engine`, an extension's module — links nothing the host may not supply. _Avoid_: using it
for code that runs on only one floor — that is the **cross-floor check**.

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
live-runtime sense — say **runtime**; **Runtime name** — a
runtime is addressed by its **machine**; **Remote link** — a link spelled by two addresses;
**Control plane** — say **local API**; **Processor catalog** — say **node catalog**; **Graph
description** — say **graph**; "helper" and "helper process" — say **processor interpreter**;
"processor" on a user surface — say **node**. **Host** stays retired: the pivot's sentences
said "host" for what the glossary calls the **runtime**. The older entries that still use the
retired words are facts about the shipped tree, read through the plan's reading rule until the
rename change re-spells them.

Retired by the 2026-10-04 moq-on-the-tailnet pivot (see `docs/decisions/moq-on-the-tailnet.md`):
**Runtime mesh**, **Mesh name** and **Mesh peer** — no mesh exists; **Stream map** — not
built, exposure being the whole of what the runtime enforces; **Relay** as a role of the
runtime — a relay is a separate program. Entries that still use these words are facts about
the shipped tree until the removal change ships.

Retired by the 2026-10-16 sharing-step align (see `docs/decisions/moq-relays.md`): **MoQ on
the tailnet** — the step is **MoQ relays**; **Viewer page** — say **preview page**;
**Bag** — say **message**; **remote_output** and **remote link** — a graph reads a relay
through the **relay source node**; **Tailnet HTTP** and `tailscale serve` — the runtime's HTTP
is loopback only; **Window**, **DisplayWindow**, **present loop**, **event pump** — the
runtime opens no windows; **EncodedVideoFrame** and **EncodedAudioPacket** — an encoder's
port carries the stack's media frame as bytes.

Retired by the 2026-10-17 realignment of the sharing step (see `docs/decisions/moq-relays.md`):
**Catalog fragment** — say **catalog entry**; **Snapshot form** and `/snapshot` — a frame is
seen through the preview page or any MoQ client; **Mp4Sink** — a stream's last mile is a MoQ
client's; **tap** and **exchange** as verification doors — verification is a tool outside the
runtime reading a relay.

Retired by the 2026-10-02 package split (see `docs/decisions/package-split-and-lend.md`):
**The wheel** — say **stream package** for `tatolab-stream` and **runtime portion** for
`tatolab.runtime`; older entries that still say "the wheel" mean the runtime portion and the
PyO3 crate it is built from. **Capability extension** and **Support hook** — no package
extends the engine: a package sets itself up where its nodes run, and an engine-grade
capability enters as a **built-in**.
