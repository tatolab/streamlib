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
several; its Input and Output nodes are its interface. The function builds it through the `StreamBuilder` it is handed.
Once loaded it persists until stopped or removed. _Avoid_: "app", "pipeline", "dataflow",
"workflow", "graph" for the unit; "attached stream", "kept stream", "detach" (every loaded
stream persists).

**Project** _(user)_: a self-contained directory holding `tatolab-project.json`, its streams' graph
documents, its assets, its node code, its `pyproject.toml` and its imports, so a copied or zipped
folder opens to the same state anywhere; `tatolab new` and `create_project` make one, its
scaffold holding one stream. Loading a project loads every stream in it, each its own unit.
_Avoid_: "stream project" (a project holds many streams), "app".

**Stream action** _(crosses)_: one queued load or unload of a project's streams — a run, a
start, a create, a stop, a remove, or the runtime's re-load at start — answered at once with its
**action id** and carried out in the background, one at a time per project. It ends done or
failed or cancelled; `describe_stream_action` reads it and `cancel_stream_action` ends it
early. A project's actions form one serial queue. `rename_stream` sets an
attribute of the record and is not an action. _Avoid_: "job", "task", "load" for a stop or a
remove.

**Project log** _(user)_: a project's one log under `<project>/.tatolab/logs/`, keyed by the
project alone, each record carrying its stream action's id and, when a stream wrote it, the
stream id as attributes; `logs` reads it by project, narrowed to a stream or an action. _Avoid_: "stream log" (a project defines many streams, and its venv
and compile belong to none of them).

**Location** _(crosses)_: what a stream is — its project directory and its stream function.
Loading the same location again replaces the stream; a moved directory is a new stream.
_Avoid_: "source" (a source node's word), "origin" (MoQ's), "path" alone.

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

**Built-in**: a first-party native node shipped inside the runtime, never on pip, added from
Python through the class `tatolab.stream` carries generated for it, its type that class's import
path, never where its native code lives; its per-frame path never enters an interpreter. The
runtime ships no media node and no transport: camera, audio, codecs, the test pattern, the
virtual camera and the MoQ nodes are modules. _Avoid_: "built-in" for a media node, a transport
or an optional capability (that is an **extension wheel** or a module).

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

**Edge I/O processor**: a source or sink node that ingests or egresses an external-world
stream — WebRTC, raw UDP, and the **gateway nodes**. Nothing leaves a stream except through one
an author added. _Avoid_: "transport processor", "network transport".

**Gateway node** _(user)_: a node in `tatolab-moq` that holds one **connection** to a relay and
publishes one broadcast it names; its **track nodes** declare the tracks in it. One gateway implementation per relay kind — `moq-relay`, Cloudflare, moq.pro —
carries what differs: dialect, credential, root, catalog cadence. _Avoid_: "relay adapter"
(retired 2026-11-06), "publisher", "sink".

**Connection** _(user)_: what a gateway node holds open to one relay — the live session, made
from the URL and credential in the node's own config. _Avoid_:
"credentials" for the whole.

**Track node** _(user)_: a node that declares one outgoing track on a gateway, with its catalog
entry, held as a graph variable and named by the send nodes that write to it. _Avoid_: "track
address", "channel".

**Track send node** / **Track receive node** _(user)_: `MoqTrackSend` writes its input's
messages to the track node it names by reference — payload bytes as written, groups from
`starts_group`; `MoqTrackReceive` reads one remote track through a gateway and writes its
payloads out as received, the one way a graph reads a relay. _Avoid_: "relay source node"
(retired 2026-11-06), "remote link", "subscriber node".

**Reference edge** _(crosses)_: a config value that names another node by the reference `add`
returned — a track naming its gateway, a send node its track — recorded in the graph, checked
at load and rendered by `graph`; it carries no data and is never a link. _Avoid_: "link" for
it, "handle".

**Variable** _(user)_: a value a `SetVariable` node names and `GetVariable` nodes read with no
drawn wire, as in Unreal Blueprints — a port's output, resolved to a link, or a node reference,
resolved to a reference edge. A get reads the setter on its own level, the stream's or a
fragment's, or else the nearest enclosing one. _Avoid_: "global", "bus".

**Broadcast** / **Track**: MoQ's words, used as they are — a gateway publishes one broadcast at a
relay, each of its track nodes one track in it. A receive node names what it reads by them
alone, `broadcast` and `track`, each read exactly as its publisher wrote it: the runtime casts
the names it gives, never a name it reads (owner, 2026-10-18). _Avoid_: "channel" (iceoryx2's
word), "topic", "track address" / `track_address` (retired 2026-10-18 — an invented term for what
MoQ already names).

**Tailnet** _(user)_: the Tailscale network a machine may belong to. A docs word only: the
runtime reads, configures and requires nothing of Tailscale, and a tailnet reaches a relay as any
network does. _Avoid_: "mesh" (retired), "VPN", "network" unqualified.

**Runtime** _(crosses)_: the one program per machine that runs streams — the engine, the
accelerator when one is present, the transports, the local API; installed once, never
constructed by a stream. Its program is `tatolabd`, as Tailscale's is `tailscaled`; the word
users read stays "runtime". _Avoid_: "host", "daemon", "server", "node", "engine" on a user
surface (the engine is the library inside it).

**Machine** _(user)_: what a runtime is addressed as — the first chunk of an address: its
hostname, cast; a containerised runtime is its own machine. _Avoid_: "host", "device", "runtime name" (retired).

**Address** _(crosses)_: the path `<machine>/<stream id>/<node>/<port>`, the same string on
every surface that names a port — `graph`, the logs, a viewer. No broadcast or track is derived
from it. _Avoid_: "channel name", "mesh address", "service name", "remote link" (retired).

**Interface** _(user)_: a graph's `GraphInput` and `GraphOutput` nodes — the ports it offers,
at every level, whether it runs alone or as a **subgraph**; Tato Studio shows a graph's Outputs.
_Avoid_: "API", "public ports", "expose" (retired 2026-11-06).

**Subgraph** _(user)_: a **fragment** placed in a stream, its Input and Output nodes its ports,
its extra parameters promoted config, flattened at compile under the reference's name. _Avoid_:
"component", "COMP", "macro", "nested stream".

**Viewer** _(user)_: a Tato Studio view of any port of any node on the machine, by its family —
a thumbnail for Image, statistics or points for Array, a waveform for Channels, keyframes for
Packets, a table for Records — read straight from the runtime, never through a relay. _Avoid_:
"preview page" (retired 2026-11-06), "dashboard", "console".

**Relay** _(crosses)_: a separate program a gateway node connects to so a stream's tracks can be
read — `moq-relay` run by a person or a rig, or a hosted one. Never the runtime, and never run by
it. _Avoid_: "tower", "router", "endpoint".

**Codec block**: an encoder or decoder node — the video codecs as the `tatolab-h264` and
`tatolab-h265` modules (`H264Encoder`, `H265Decoder`, ...), the Opus pair as `tatolab-opus` —
configured like any node; its per-frame path never enters an interpreter. There is no muxer: a stream's last mile is a MoQ client's. The next
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
length, the group flag), a payload descriptor — its port's **family**'s for a family write,
`Bytes` for a by-name write — and a **payload**; at a relay, one object. _Avoid_: "bag" (retired 2026-10-16 — it named the msgpack map that was the whole
payload), "frame" for anything but a video frame.

**Payload**: a message's bytes, laid out by its port's family: empty for an Image frame, the
planar samples of a Channels block, a codec's unit for Packets, a Records dict the runtime
encodes, an Array's attribute table and CPU data, or a by-name write's bytes as written.
_Avoid_: "bag", "body".

**Cast object**: the typed object a read returns — the family's type through a port
attribute, or what `read(port, into=T)` constructs through `T.from_bytes` — the consumer's
view of a message. A cast type that claims its surface is
also the tensor-protocol producer for that frame. _Avoid_: "typed bag", "frame object" (a
cast type need not be a frame).

**Catalog entry** _(crosses)_: what an output port's declaration says about what it writes,
in the layout the stack's players read — the section (`video`, `audio`, `text`, `json`,
`binary`) and that section's own fields — written by the node that owns the port, merged by
a gateway node into its broadcast's catalog as text and rendered by `graph`; documentation a
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

**Node** _(user)_: a step in a stream — the class a person writes (`@node.process` in Python,
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

**Graph** _(crosses)_: a stream's nodes, links and reference edges as one JSON shape — what a
stream asset's graph document saves, which the runtime alone writes through transactions and renders live. _Avoid_: "graph description", "stream
description", "snapshot", "manifest", "pipeline file".

**Project** _(crosses)_: a versioned repository of assets and node code with its own history,
which the runtime writes. _Avoid_: "app", "workspace" for one project.

**Import** _(crosses)_: another project used by this one, copied once, whole, into
`imports/<folder>/` and read there in place; an update replaces the folder whole. Tato Studio
may call it a module. _Avoid_: "linked dependency", "fork", "fetch" for an import.

**Asset** _(crosses)_: one file a project tracks by UUID — a stream's graph document, media, an
image, USD, an FMU — referenced by UUID, never by path. _Avoid_: "resource", "file" when the UUID
is meant.

**Graph document** _(crosses)_: a stream asset's canonical JSON — nodes by UUID, config, pin
values, studio fields, links, fragments, groups — and the stream's source of truth. _Avoid_: "stream file", "scene", "`stream.py`".

**Group** _(crosses)_: a labelled group of one level's nodes in a graph document — a UUID, a title,
a description and a colour — that a node joins by naming its UUID as its `studio.group`; it changes
nothing that runs. _Avoid_: "fragment", "subgraph" for a group.

**Fragment** _(crosses)_: a `@graph_generator` function's output stored in a graph document as one
read-only container with its own node UUIDs, refreshed when the generator re-runs. _Avoid_:
"nested stream", "group".

**Stream fragment** _(crosses)_: a `tatolab.project:StreamFragment` node holding only a reference,
`{"stream": {"asset", "from"?}}`, to a stream of this project or of an import, whose graph each
load places in it as that stream's document is on disk. _Avoid_: "copy", "paste" for one.

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

**Port**: a processor's named attachment point for a link, declared as a class attribute
(`Input(…)` or `Output(…)`). Declares name, **family**, description, and — on an input —
delivery profile. _Avoid_: "channel" for the port itself.

**Family** _(crosses)_: which of five engine-owned layouts a port carries — **Image**, **Array**,
**Channels**, **Packets** or **Records** — compared by equality at `connect`. Names inside a
family (channels, attributes, record keys, codec strings) stay open. _Avoid_: "type",
"schema", "caps", "kind".

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
handle-shaped surface — GPU memory import/export (DMA-BUF / OPAQUE_FD), texture rings, the audio clock, color resolution. Built-ins and
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
runtime opens none, and a port is seen in a **viewer**.

**Kernel**: a GPU program the engine compiles and runs on its device — compute,
graphics, or ray-tracing. _Avoid_: "shader" for the whole object (that is its source
text), "pipeline" (the Vulkan-internal object it builds).

**ChannelsBlock**: what a Channels read returns — a run of planar `[channel][sample]` samples
borrowed in its sample (`start_sample`, rate, channel and per-channel sample counts, `f32` or
`f64`), its stamp the first sample's instant, CPU-resident, never surface-backed. _Avoid_:
"AudioBlock" (the msgpack cast it replaced), "AudioFrame" (in device APIs a frame is one
sample across channels), "audio chunk".

**Window contract**: an audio input port's declared rate / channels / dtype / window /
hop — window and hop in samples at the declared rate — the engine resamples, mixes
down, and frames to it natively, so `process()` receives exact-size blocks.
_Avoid_: "windower" as an object name (it is a port declaration, not a graph node).

**Conditioning**: the engine-internal AEC / noise-suppression / AGC chain between an
audio device and its published Channels block, declared on the built-ins and bypassable.
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

Retired by the 2026-11-06 gateway-nodes pivot (see `docs/decisions/gateway-nodes.md`):
**Publisher**, **Exposure**, **Exposed port**, **Routing policy** and `expose` — nothing leaves a
stream on its own, and a graph's interface is its Input and Output nodes; **Relay adapter** —
say **gateway node**; **Relay source node** — say
**track receive node**; **Local relay** — the runtime runs no relay; **Preview page**, `/watch`
and **Form** — say **viewer**.

Retired by the 2026-10-02 package split (see `docs/decisions/package-split-and-lend.md`):
**The wheel** — say **stream package** for `tatolab-stream` and **runtime portion** for
`tatolab.runtime`; older entries that still say "the wheel" mean the runtime portion and the
PyO3 crate it is built from. **Capability extension** and **Support hook** — no package
extends the engine: a package sets itself up where its nodes run, and an engine-grade
capability enters as a **built-in**.
