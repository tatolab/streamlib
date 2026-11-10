<div align="center">

# Tatolab

**Turn anything into a live stream.**<br>
Wire cameras, models and your own code into a stream. Send an output to a MoQ relay, and apps, agents and your other machines read it there.

[![website](https://img.shields.io/badge/tatolab.com-0ea5e9?label=website)](https://tatolab.com)
[![license](https://img.shields.io/badge/license-BUSL--1.1-0ea5e9)](LICENSE)
[![python](https://img.shields.io/badge/python-3.12%20%E2%80%93%203.13-0ea5e9)](#install)
[![platform](https://img.shields.io/badge/platform-linux%20x86__64%20%7C%20macOS%20apple%20silicon-64748b)](#what-ships-today)
[![gpu](https://img.shields.io/badge/GPU-Vulkan-64748b)](#gpu-without-the-vendor-lock)

[Install](#install) · [Quickstart](#quickstart) · [Writing a stream](#writing-a-stream) · [The `tatolab` CLI](#the-tatolab-cli) · [Inspect a running stream](#inspect-a-running-stream) · [How it works](#how-it-works) · [What ships today](#what-ships-today) · [Troubleshooting](#troubleshooting) · [License](#license)

</div>

<!-- Demo GIF slot. Generate on the rig with `vhs docs/assets/demo.tape`, commit the
     result, then replace this comment with:
     <div align="center"><img src="docs/assets/demo.gif" alt="Inspecting a running stream" width="900"></div> -->

---

- **A stream is a graph document.** `streams/<name>.graph.json` wires nodes together, and a Python
  node is a `@node.process` class in a module of its own. There is no registration file and no build step
  between an edit and a run.
- **Every Python node runs in its own process**, with its own interpreter started from your stream's
  venv. A node that hangs or crashes is one node, named in the log; it cannot stall the others.
- **Frames stay on the GPU.** Capture lands in device memory and reaches torch through DLPack with no
  CPU copy, and a pixel effect is one Slang kernel. Reading pixels on the CPU is an explicit
  `frame.cpu()`.
- **Media nodes are modules, built the way you would build one.** The camera, microphone,
  speaker, H.264 / H.265 and Opus codecs, the test pattern and the Linux virtual camera are
  packages on PyPI; the ones with deadlines move their frames and samples on native threads.
- **Agents work on a running stream.** The runtime serves MCP, so an agent can read the live graph,
  sample what a link carries, look at a frame, read the logs, and add, connect or remove nodes while
  the stream runs — the same operations the `tatolab` CLI uses.
- **A fast edit loop.** `tatolab dev` restarts the stream on every saved edit; an edit that fails
  to load records the stream failed, with why in the log, until the next save.

> **Alpha.** APIs will change. On Linux one command installs the runtime; on macOS you build it
> from a checkout of this repository for now. One runtime per machine runs every stream on it. See
> [what ships today](#what-ships-today).

> **License.** Source-available under [BUSL-1.1](LICENSE), converting to Apache 2.0 on January 1,
> 2029. Building nodes, applications and products on Tatolab is free, commercial use included. Selling,
> hosting as a managed service, white-labelling or sublicensing the runtime itself, or building a
> competing runtime from it, needs a [commercial license](docs/license/COMMERCIAL-LICENSING.md). See
> [License](#license).

## Built for

Tatolab turns anything into a live stream: cameras, models and your own code, wired together in
Python and shared with people and agents. Today a stream runs on one machine, and you and your
agents work on it there.

- **Live video and audio with a model in the loop** — a camera into torch on the GPU, the result on
  a virtual camera that other applications can pick.
- **Agents that watch and change a running stream** — over MCP, on the machine itself, or from
  another machine over ssh.
- **Tailscale users, first.** Reaching a stream on another machine of your tailnet is
  `ssh <machine> tatolab mcp`; Tatolab builds nothing Tailscale already does.

## What it doesn't replace

| You keep | Tatolab's part |
|---|---|
| **Your model runtime** — torch, ONNX Runtime, TensorRT, MLX | Hands it the frame as device memory and gets out of the way. No inference stack ships, and none is planned. |
| **Your Python packages** | A node imports whatever your stream's venv holds; `uv add` is how a dependency arrives. |
| **Your network** — Tailscale, ssh | Control is reachable only on the machine the runtime runs on. Reaching another machine is ssh's job, across your tailnet or not. |
| **Your robotics stack** — ROS, Zenoh | Out of scope. Tatolab never replaces it. |

**The honest limit:** composing today means *on the same machine, through Python*. No bridges ship;
where you need one, it is a node you write against the library or binding you already have.

## Install

```bash
curl -fsSL https://get.tatolab.com/install.sh | sh
```

On Linux x86_64 that installs **the runtime** — `tatolabd`, the native program that hosts this
machine's streams, and `tatolab`, the CLI that loads, manages and inspects them — into a folder
of its own per build under `~/.local/share/tatolab/`, and links both into `~/.local/bin`. It
starts nothing: run `tatolabd` in a terminal of its own, and every `tatolab` verb talks to it.
Re-run it to upgrade: it keeps the build before, and a runtime already running keeps its build
until you restart it. `sh -s -- --uninstall` in place of `sh` removes it and leaves your streams'
records and logs. On a desktop, Tato Studio carries the runtime and runs it while the app is open.

### Keep the runtime running as a service

To keep the runtime up on a machine nobody sits at, run it under a service manager of your own.
As a systemd user service, save this as `~/.config/systemd/user/tatolabd.service`:

```ini
[Unit]
Description=Tatolab runtime
After=graphical-session.target

[Service]
ExecStart=%h/.local/share/tatolab/current/bin/tatolabd
Restart=on-failure
# tatolabd stops its own Python processes on SIGTERM; the rest is killed only if it outlives it.
KillMode=mixed

[Install]
WantedBy=default.target
```

then `systemctl --user enable --now tatolabd`, and `loginctl enable-linger $USER` to keep it
running while you are logged out. After an upgrade, `systemctl --user restart tatolabd` runs the
new build.

**Your stream** is an ordinary Python project. Its venv holds `tatolab-stream` — the Python API,
`import tatolab.stream` — and whatever your nodes import, and never the runtime. `tatolab-stream` is
on PyPI:

```bash
uv add tatolab-stream
```

A new project's `pyproject.toml` depends on `numpy>=2.1` and on `tatolab-stream` at least as new as
the runtime that created it.
The runtime carries its own uv and makes a project's `.venv` with it whenever `.venv/bin/python`
does not run, fetching the Python `.python-version` pins when the machine lacks it, so there is no
setup step.

### Building the runtime from source

On macOS, until the app ships, and for working on the runtime itself, build it from a checkout of
this repository:

```bash
cargo xtask build-runtime --release              # omit --release for a debug build
export PATH="$PWD/target/tatolab-runtime/bin:$PATH"
```

That lays out `target/tatolab-runtime/` as an install prefix: `bin/tatolabd`, `bin/tatolab` and
`lib/tatolab/lend/`, the runtime's own Python half (`tatolab.runtime`) that each Python node's
interpreter borrows. Keep `bin/tatolabd` beside `lib/`: `tatolabd` finds the lend relative to its
own executable.

The build needs a Rust toolchain and [uv](https://docs.astral.sh/uv/) on `PATH` (it runs the
pinned maturin through `uvx`), plus a native toolchain. On Debian or Ubuntu that is:

```bash
sudo apt install pkg-config protobuf-compiler libclang-dev libvulkan-dev python3-dev cmake ninja-build
```

On Linux the build first stages a Vulkan loader, Mesa's lavapipe and the Slang shader compiler into
the lend, built inside the release's pinned manylinux image, so it also needs Docker. On macOS it is
CMake and Ninja, and the build first stages a Vulkan loader, MoltenVK and the Slang shader compiler
into the lend, so the Mac that runs the result needs no Vulkan SDK. A `cargo build` or `cargo test`
of the engine on its own compiles the engine's shaders through the Slang compiler
`bash scripts/stage_slang_compiler_libraries.sh` stages under `target/slang_compiler/`; run it once
first.

Start a runtime built from source with `tatolabd` in a terminal of its own.

## Quickstart

The installer already started this machine's runtime. Built from source, start it first:

```bash
tatolabd                    # in a terminal of its own: this machine's runtime
```

```bash
tatolab new my-stream       # the runtime writes a test pattern → GPU effect, plus a CPU meter, and runs it
cd my-stream
tatolab dev                 # the test pattern through a GPU lens, its brightness logged
```

`tatolab new` asks the runtime to create the project: it writes the files, makes the `.venv` and
runs the stream under the folder's name, and `new` prints the action doing it and returns. With
no directory, the runtime creates the project in a new folder under `~/streams` named
`<adjective>-<noun>`.

`tatolabd` is the one runtime on this machine, and every stream runs inside it; no `tatolab` verb
starts one. `dev` queues a load of the project in the working directory and follows the project's
log: the runtime syncs the project's `.venv` with the uv it carries, reads every graph document
in the project, loads each one as its own stream and starts it, and `dev` prints every record in
your terminal. Save an edit and `dev` queues another load, and the runtime replaces the running
streams with the edit. An edit that fails unloads its stream and records it failed: the reason
lands in the log `dev` prints, and the next save loads it again. An edit that leaves
`tatolab-project.json` unreadable fails the load itself and leaves the running streams as they
were. Ctrl-C ends `dev`
and leaves the streams running. A project runs and stops as a whole: every stream the runtime
loads is recorded, the project runs until `tatolab stop` or `tatolab rm`, and the runtime loads it
again each time it starts. `tatolab run`
queues the load once, without watching for edits, prints the action's id and returns at once;
`tatolab logs -f` follows the load and the streams' records. `tatolab cancel <action>` takes an
action back: a queued one is dropped, and a running load stops at the step it is on, its uv sync
killed, leaving the streams it had not reached as they were. A stop or `rm` that has
begun unloading runs to the end.

## Writing a stream

A stream is a graph document, `streams/<name>.graph.json`: its nodes and the links between
them, and nothing that runs. `tatolab new` writes `streams/main.graph.json`:

```json
{
  "asset": "6f1c2e9a-3b4d-4c5e-8f60-71a2b3c4d5e6",
  "links": [
    {
      "source": {"node": "0b7c1d2e-5a3f-4b6c-9d8e-1f2a3b4c5d6e", "port": "video"},
      "target": {"node": "4e5f6a7b-8c9d-4e0f-a1b2-c3d4e5f6a7b8", "port": "video_from_upstream"}
    },
    {
      "source": {"node": "4e5f6a7b-8c9d-4e0f-a1b2-c3d4e5f6a7b8", "port": "video_to_downstream"},
      "target": {"node": "8c9d0e1f-2a3b-4c4d-8e5f-6a7b8c9d0e1f", "port": "video_from_upstream"}
    }
  ],
  "nodes": [
    {"config": {}, "id": "0b7c1d2e-5a3f-4b6c-9d8e-1f2a3b4c5d6e", "name": "testpatternsource", "type": "tatolab.testpattern:TestPatternSource"},
    {"config": {}, "id": "4e5f6a7b-8c9d-4e0f-a1b2-c3d4e5f6a7b8", "name": "halftonelenseffect", "type": "nodes.halftone_lens_effect:HalftoneLensEffect"},
    {"config": {}, "id": "8c9d0e1f-2a3b-4c4d-8e5f-6a7b8c9d0e1f", "name": "brightnessmeter", "type": "nodes.brightness_meter:BrightnessMeter"}
  ]
}
```

`asset` is the stream's UUID and each node's `id` is its own, which the runtime generates when it
places the node and never reuses; a link end names its node by that UUID. A node may also hold
`pins`, the values on its unlinked inputs, and `studio`, how Tato Studio draws it. Reading the
document runs no code, but a load imports each node's module in the project's interpreter to
describe its ports, so a module's import-time code runs at every load.

The meter reads the effect's output and logs its brightness each second; open that output in
Tato Studio's viewer to watch it. Swap `tatolab.testpattern:TestPatternSource` for
`tatolab.camera:CameraSource` (`uv add tatolab-camera` in the project), which publishes on the same `video`
port, to run your camera through the same effect.

To send an output off the stream, encode it and add the `tatolab.moq` gateway nodes
(`uv add tatolab-moq`): a `MoqGateway` dials a relay and publishes one broadcast, a `MoqTrack`
declares a track of it, and a `MoqTrackSend` writes a port's messages to that track. Another
stream, on this machine or another, reads the track through a `MoqTrackReceive` node naming a
gateway on the same relay; a link's two ends are always in one stream.

Pixels stay on the GPU. `nodes/halftone_lens_effect.py` is one Slang compute kernel, with a lens
that drifts around the frame and turns what it covers into dots:

```python
import struct

from tatolab.stream import (
    Image,
    ImageFrameToWrite,
    Input,
    NodeOutputTextureRing,
    Output,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    node,
)

# The whole effect, as a Slang compute kernel: one invocation per output pixel.
# A lens drifts around the frame and turns what it covers into dots. `dials`
# are the values `process()` pushes each frame. Edit it while `tatolab dev`
# runs to see the change.
HALFTONE_LENS_SLANG = """
[[vk::binding(0, 0)]] Sampler2D source_frame;
[[vk::binding(1, 0)]] [format("rgba8")] RWTexture2D<float4> halftone_frame;

struct HalftoneLensDials {
    float2 frame_size;
    float seconds;
};
[[vk::push_constant]] ConstantBuffer<HalftoneLensDials> dials;

// How big the drifting lens is, as a share of the frame's height.
static const float LENS_RADIUS = 0.3;
// How many pixels apart the dots sit under the lens.
static const float DOT_CELL_SIZE = 8.0;
// How far the gaps between the dots fade toward grey.
static const float GAP_FADE = 0.35;

[numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    if (any(float2(invocation.xy) >= dials.frame_size)) {
        return;
    }
    float4 source = source_frame.Load(int3(invocation.xy, 0));
    float2 pixel = float2(invocation.xy) + 0.5;
    float2 lens_centre = dials.frame_size * float2(
        0.5 + 0.3 * sin(dials.seconds * 0.4),
        0.5 + 0.25 * sin(dials.seconds * 0.55)
    );
    float distance_from_lens_centre = length(pixel - lens_centre) / dials.frame_size.y;
    float under_the_lens =
        1.0 - smoothstep(LENS_RADIUS * 0.92, LENS_RADIUS, distance_from_lens_centre);

    // Each cell keeps a round dot of the picture and fades the gap around it
    // toward grey, so the dots show on every colour, black included.
    float2 cell_centre = (floor(pixel / DOT_CELL_SIZE) + 0.5) * DOT_CELL_SIZE;
    float dot_radius = DOT_CELL_SIZE * 0.42;
    float in_the_dot = 1.0 - smoothstep(dot_radius - 1.0, dot_radius, length(pixel - cell_centre));
    float3 gap = lerp(source.rgb, float3(0.5), GAP_FADE);
    float3 dotted = lerp(gap, source.rgb, in_the_dot);

    halftone_frame[invocation.xy] = float4(lerp(source.rgb, dotted, under_the_lens), source.a);
}
"""

# `HalftoneLensDials` as the push constants lay it out: two floats, then one.
HALFTONE_LENS_DIALS = struct.Struct("<fff")

WORKGROUP_TILE_SIZE_IN_PIXELS = 8

NANOSECONDS_PER_SECOND = 1_000_000_000


def workgroups_covering(pixels: int) -> int:
    return (pixels + WORKGROUP_TILE_SIZE_IN_PIXELS - 1) // WORKGROUP_TILE_SIZE_IN_PIXELS


@node.process
class HalftoneLensEffect:
    """Turns what a slowly drifting lens covers into dots on the GPU, and passes each frame on."""

    video_from_upstream = Input(Image, delivery="newest")
    video_to_downstream = Output(Image)

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.halftone_lens_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=HALFTONE_LENS_SLANG,
            push_constant_size=HALFTONE_LENS_DIALS.size,
            bindings={
                "source_frame": "sampled_texture",
                "halftone_frame": "storage_image",
            },
        )
        # A frame arrives as a pixel buffer, which a kernel cannot sample, so
        # each one is copied into a texture first.
        self.source_frame_textures = NodeOutputTextureRing(
            "rgba8_unorm", ["texture_binding"], depth=1
        )
        self.halftone_frame_textures = NodeOutputTextureRing(
            "rgba8_unorm", ["storage_binding", "texture_binding"]
        )
        self.first_frame_at_ns: int | None = None

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read(self.video_from_upstream)
        if frame is None:
            return
        # `ctx.time` is stream time in ns, on the clock the stream runs on.
        if self.first_frame_at_ns is None:
            self.first_frame_at_ns = ctx.time
        seconds = (ctx.time - self.first_frame_at_ns) / NANOSECONDS_PER_SECOND

        gpu = ctx.gpu_limited_access
        source_frame = self.source_frame_textures.next_texture_for_this_frame(
            gpu, frame.width, frame.height
        )
        gpu.copy_surface_to_surface(frame.surface_id, source_frame)
        halftone_frame = self.halftone_frame_textures.next_texture_for_this_frame(
            gpu, frame.width, frame.height
        )
        self.halftone_lens_kernel.dispatch(
            bindings={"source_frame": source_frame, "halftone_frame": halftone_frame},
            group_count=(
                workgroups_covering(frame.width),
                workgroups_covering(frame.height),
                1,
            ),
            push_constants=HALFTONE_LENS_DIALS.pack(frame.width, frame.height, seconds),
        )
        ctx.outputs.write(
            self.video_to_downstream,
            ImageFrameToWrite.of_texture(halftone_frame, frame.colour),
            timestamp_ns=frame.timestamp_ns,
        )
```

Logic runs on the CPU. `nodes/brightness_meter.py` reads each frame back through an explicit
`frame.cpu()` view and logs its mean brightness once a second. It sits on a fan-out in its own
process, so it never slows the picture — it is the node you replace with your model call.

What a node declares:

- **Ports**, as `Input` and `Output` class attributes, each of one family: `Image`, `Array`,
  `Channels`, `Packets` or `Records`. The attribute name is the port name, and a read through it
  returns the family's type. Every input names its [delivery](#how-it-works), `newest` or
  `ordered`.
- **When it runs**, with `@node.process(execution=…)`: `reactive` (the default once it has an input),
  `manual` (driven by a callback it owns), or `continuous` with `interval_ms`. A source has nothing
  to react to, so it must name one. `scheduling` asks for `realtime`, `high` or `normal` priority.
- **Its configuration**, as the annotation on `__init__`'s `config` parameter — a `TypedDict`, a
  dataclass or a pydantic model — that the node's `config` in the graph document fills.

A node class lives in a module of its own, which the graph document names as `module:ClassName`:
its process imports the class by that name.

A project may hold several graph documents, and a load of the project loads each one as its own
stream; a document that is gone unloads and forgets its stream. `--dir DIR` names another project
directory than the working one. A stream first loaded by `run` or `dev` shows under its document's
name, and the one `tatolab new` creates under the project folder's name, until `tatolab rename`
gives it another display name, which a load of the project keeps. A stream is keyed and addressed
by its **stream id**, `<project name>-<stream name>-<hash>`, the hash over the project's UUID in
`tatolab-project.json` and the stream asset's UUID in its document. Once recorded, a stream keeps
its id when the project moves or its document is renamed, since the runtime finds its record by
those two UUIDs; a document first loaded after a rename takes its id from the new name. Running a
project again replaces its stream under the same id. A
second copy of a project, the same project UUID in another directory, is refused while the first
holds the stream.

## The `tatolab` CLI

```text
tatolab new [DIRECTORY]
tatolab run [--dir DIR]
tatolab dev [--dir DIR]

tatolab streams
tatolab stop [--dir DIR]
tatolab pause [--dir DIR]
tatolab resume [--dir DIR]
tatolab rm [--dir DIR]
tatolab cancel ACTION
tatolab rename [STREAM] DISPLAY_NAME
tatolab clock [STREAM] --scale SCALE

tatolab graph [--stream STREAM]
tatolab logs [--dir DIR] [--stream STREAM] [--action ID] [-f | --follow] [--processor ID]
             [--pipeline ID] [--rhi] [--level trace|debug|info|warn|error] [--source rust|python]
             [--intercepted-only]
tatolab mcp

tatolab enable-virtual-camera [--print]
```

- **`new`** has the runtime create a project in `DIRECTORY`, resolved against the working
  directory, or with none in a folder `<adjective>-<noun>` it draws under `~/streams`, and
  prints `<action> queued: create of <project>` and `follow it: tatolab logs -f --dir
  <project>`, then returns 0 at once. The action writes `tatolab-project.json`,
  `streams/main.graph.json`, `nodes/`, `pyproject.toml`, `.python-version` (3.12) and
  `.gitignore`, makes the project's first commit, then loads the project as `run` does, showing
  its stream under the folder's name. A directory already holding one of those files fails the
  action with nothing written.
- **`run`** queues a load of the project on the project's queue and prints
  `<action> queued: load of <project>` and how to follow it, then returns 0 at once. The runtime
  syncs the project's `.venv`, reads every graph document in the project, and loads and
  records each one; `tatolab logs --action <action>` and `tatolab streams` show how it went. A
  stream whose document does not load is not refused: the runtime records it failed, with why in
  the project's log. `run` exits 1 only when the load is not queued: the runtime refuses it or
  cannot be reached, or `--dir` names no directory. The runtime
  loads each stream again at each of its own starts until `stop` or `rm`, and loading the same
  project again replaces its streams under the same ids — failed ones included.
- **`dev`** is `run`, then the project's log followed from where it ended, with another load
  queued on every saved edit to a `.py` file, a `.graph.json` document, `pyproject.toml` or
  `tatolab-project.json` in the project; the runtime replaces the running streams, and an edit
  that fails unloads its stream and records it failed, why printed from the log, until the next
  save; an edit that leaves `tatolab-project.json` missing or unreadable fails the load and leaves
  the running streams as they were. Ctrl-C ends `dev` and leaves
  the streams running. When the runtime goes away, `dev` says so, waits for it to answer again,
  and follows the project's log on from where it read up to.
- **`streams`** lists the streams the runtime holds: each one's id and display name, whether it is
  running, stopped or failed, its node count, its project and its stream asset, and why each
  failed one failed. A
  stream implicated in the runtime's last two crashes in a row, or one that cannot re-load at the
  runtime's start (its venv deleted, say), is failed: the runtime skips it at its starts and
  brings every other stream back.
- **`STREAM`**, in `rename`, `clock` and `logs --stream`, is a stream id,
  or a display name exactly one stream holds. Omitted, it is the one stream loaded from the
  working directory, stopped and failed ones included. An argument several streams match, or none,
  is refused listing each candidate's id and display name. `graph --stream` is the exception: it
  takes a stream id, and omitted it means every loaded stream.
- **`stop`** unloads every stream of the project in `--dir`, or the working directory, and the
  project stays stopped across the runtime's restarts until **`run`** runs it again; `run` also
  retries a failed stream. **`rm`** unloads every stream of the project and forgets them. Each
  queues its action on the project's queue and prints its id at once: `tatolab streams` shows the
  streams `stopping` until it has run, and a runtime that crashes first finishes it at its next
  start.
- **`cancel`** takes back the action whose id `run`, `dev`, `stop` or `rm` printed. A
  queued action is dropped, its streams' records left as they were, and `cancel` prints
  `<action> cancelled`. A running load stops at the step it is on: its uv sync is
  killed and a half-made `.venv` removed, a stream it was loading is recorded failed,
  and the streams it had not reached stay as they were; `cancel` prints `<action> cancelling`.
  A running stop or `rm` has begun unloading and cannot be cut short, so `cancel` is refused and
  the action ends done.
- **`rename`** shows a stream under another display name — any text, never part of
  its address. The stream's record holds it, across the runtime's restarts and the project's
  loads.
- **`clock`** runs a started stream's scaled clock at another `--scale`, live: stream time carries
  on from where it stands, and every node's time, write stamps and continuous pace follow it. A
  stream on the real or a stepped clock is refused, as is a scale above 1. The stream's next load
  runs the clock its graph document names.
- **`graph`** prints every loaded stream's nodes, ports and links as JSON, with each
  node's and link's state and counters, under the machine's name as `machine`; `--stream` prints
  one stream's graph alone, in the shape a load takes. The machine's name is its hostname, less a
  trailing mDNS `.local`, cast to lowercase URL-safe.
- **`logs`** reads a project's log — `--dir`'s, or the working directory's — through the runtime,
  oldest record first, rendered the way the runtime prints them: every record its streams, their
  venv step and their load wrote, kept in `<project>/.streamlib/logs/project.jsonl` across
  restarts. `--stream` narrows it to one stream, by its id or a display name only it holds, and a
  removed stream's id still reads its records; `--action` narrows it to one stream action.
  `--follow` keeps reading as records land until Ctrl-C, which leaves every stream running, and
  ends with exit 1 naming the runtime's log when the runtime goes away. The other flags filter
  what it shows.
- **`mcp`** connects an MCP host to the runtime over its own stdin and stdout; see below.
- **`enable-virtual-camera`** installs, once, the permission a `VirtualCameraSink` needs for its
  loopback camera: the `v4l2loopback` module loaded with no devices, and its control node handed to
  the logged-in user through udev. It is one privileged step behind your desktop's password prompt
  (`sudo` in a headless shell), Linux only; `--print` writes the files and commands for a hand
  install and changes nothing.

Every verb but `enable-virtual-camera` and `logs --list` or `logs RUNTIME_ID-STREAM` (which
read the JSONL on disk) talks to the runtime through its local socket, at one fixed path per user. With no runtime
running, each refuses at once, naming the socket and `tatolabd` as the way to start one.

## Inspect a running stream

Run these on the machine the stream runs on. They reach the runtime through its local socket — a
Unix socket only your user can open — so control is reachable only on that machine.

```console
$ tatolab graph | jq -r .machine
desk
```

`graph` shows every node's state and counters, so it says whether data is moving; `logs` reads
what every node logged, Rust and Python alike. To watch a node's output, open it in Tato Studio's
viewer, which reads the port on this machine with no copy. To read what a port carries from
anywhere else, send it to a relay with the `tatolab.moq` gateway nodes: encode it, link the
encoder into a `MoqTrackSend`, and the track is `<track>` of the broadcast `<broadcast>.hang` that
the stream's `MoqGateway` publishes. Any MoQ client reads it there — this repository's
`tatolab-read-track` writes a track's objects to files:

```console
$ cargo run -p tatolab-read-track -- http://127.0.0.1:4443 desk.hang camera --count 30 --out frames/
```

An `H264Encoder`'s track files, concatenated, are an H.264 stream any decoder plays; an
`H265Encoder`'s are an HEVC one. A raw frame's port carries a GPU surface's id, which means nothing
outside the runtime, so to send a mid-graph frame, connect an encoder to it and send the encoder's
output.

<details>
<summary><b>The same operations speak MCP, so an agent can do this itself</b></summary>

<br>

```console
$ claude mcp add tatolab -- tatolab mcp
```

The host launches `tatolab mcp`, which sends one upgrade request on the runtime's local socket and
then copies bytes between its own stdin and stdout and the runtime's MCP server, reading none of
them. To reach a runtime on another machine, launch it over ssh instead, with `tatolab` on that
machine's `PATH`: `claude mcp add tatolab -- ssh <machine> tatolab mcp`. The server belongs to the
runtime and comes and goes with it. A stream the host runs is recorded by the runtime and outlives
the host's connection: an agent's next session finds it in `list_streams`.

The tools are `graph`, `logs` and `describe_runtime` — the runtime's version, build id, uptime
and the folder of its own log — to observe; `add_node`, `connect`, `disconnect` and
`remove_node` to change a running graph; `run_project`, which queues a run of a project and
answers at once with an action `describe_stream_action` follows and `cancel_stream_action` ends
early, `create_project`, which creates a project as `tatolab new` does and queues its run the
same way, and `list_assets`, `describe_asset`, `import_asset`, `move_asset`, `rename_asset` and
`delete_asset` over the project's assets; `stop_project`, which stops a whole project until
`run_project` runs it again, `pause_project` and `resume_project`, which pause and resume a
whole project at once, `remove_project`, `rename_stream`, `set_stream_clock_scale` and
`list_streams` to manage the streams the runtime holds; and `open_viewer` and `close_viewer` to hold a node's output port open for a viewer on this
machine, naming the node by the `id` `graph` lists. Every tool
about one stream names it with `stream`, and `connect` links two ports of that one stream. Beside
them the runtime serves the node catalog — every
node type it can add, with its description, config schema and ports — and the live graph as
resources, and four prompts whose every step is a tool call. An agent can write a
node class into a module of the project, or `uv add` a package that ships one, then add it by
its `module:ClassName` path and connect it into the running stream. The runtime never imports it:
the class runs in its own process, started from the stream's venv, like every other Python node.

**Control is reachable only on its machine.** A runtime opens no network port for it. Whoever can
open its local socket — only processes running as your user — can observe and rewire it.

</details>

## How it works

<details>
<summary><b><code>tatolab</code>, <code>tatolabd</code> and your venv</b> — what runs where</summary>

<br>

`tatolabd` is the machine's runtime, started in a terminal and never by a verb. It takes the
machine's lock, so a second `tatolabd` is refused naming the first one's user, pid and executable,
and serves its local socket at a fixed path. It keeps its state in
`$XDG_STATE_HOME/tatolab/` (else `~/.local/state/tatolab/`), or `~/Library/Application
Support/Tatolab/` on macOS: a record of each stream it runs — its project and stream asset,
interpreter, and whether it is stopped — and its own log. At each start it loads every
recorded stream that is neither stopped nor failed.

`tatolab run` queues a load of a project. The runtime syncs the project's `.venv`, reads
`tatolab-project.json` and each graph document in the project, then loads each graph, starts it
and records it, writing every step to the project's log;
`tatolab run` returns as soon as the load is queued.

`tatolabd` is native: the engine, the built-in nodes, and the local socket the CLI and MCP hosts
talk to. No Python runs in its process, and it never imports your code — it learns a Python node's
ports by asking your venv's interpreter to describe the class. Each Python node runs in an
interpreter started from your venv, with the runtime's own Python half, `tatolab.runtime` in
`lib/tatolab/lend/`, put first on its `PYTHONPATH`; it checks that what it borrowed is the exact
build that started it. So nothing of the runtime enters your venv or your `pyproject.toml`, and
nothing in a stream names a runtime version.

</details>

<details>
<summary><b>Node isolation</b> — why a wedged model can't stall the stream</summary>

<br>

Every Python node runs in its own OS process with its own interpreter, at a scheduling priority you
declare. A model that deadlocks on a malformed frame, a C extension that segfaults, a vendor library
that leaks — each takes down one node and is reported under that node's name, instead of a
whole-stream slowdown with no address. The boundary is enforced by the kernel, not by convention.
No mode runs a Python node inside the runtime's process: not a default, not a fallback, not
something that kicks in under load.

**It costs you** a process boundary on every link crossing into Python, and one authoring rule: a
node's class lives in an importable module of its own.

</details>

<details>
<summary><b>Any source, not just cameras</b> — the extension model</summary>

<br>

Nothing in the runtime is specific to video. A source is a node that produces without consuming —
running `continuous` at an interval, or `manual` when driven by a callback it owns. A sensor, a
socket, a file, an SDK with a Python binding: if you can read it from Python, it is a source you
write, with the same isolation, the same clock and the same observability as everything that ships.

Native code comes in the same door. A third-party driver — closed-source included — ships as an
ordinary Python package that exposes handles (file descriptors, exportable allocations, buffers)
and is wrapped by a node you write. It never links the runtime, and the CPython ABI is the only
binary boundary. First-party optional capabilities take that same door: `tatolab-webrtc`
(`uv add tatolab-webrtc`) is an ordinary package with Rust inside and two nodes for WHIP publish and
WHEP play, `WhipPublisher` and `WhepPlayer`, whose native code runs in those nodes' own processes;
`tatolab-opus` (`uv add tatolab-opus`) carries libopus the same way, as `OpusEncoder` and
`OpusDecoder`.
No package extends the runtime — there is no plugin ABI, no manifest and no lockfile of ours, only
an ordinary `pyproject.toml`.

**It costs you** no media node in the runtime: its built-ins only reshape data between families
(`ChannelsToRecords`, `ImageToArray` and the like). Every media node is a package, the MoQ gateway
nodes (`tatolab-moq`) among them, and anything else is a package or a node you write.

</details>

<details>
<summary><b>Device memory, not a copy</b> — handing frames to torch</summary>

<br>

```python
frame = ctx.inputs.read(self.video_from_upstream)
if frame is not None:
    pixels = torch.from_dlpack(frame)   # H×W×4 uint8, on the device the frame already lives on
```

On Linux, with a CUDA runtime present, that is a CUDA tensor; on macOS it is a Metal tensor over the
frame's own IOSurface, which torch 2.10 or newer imports as `mps` and MLX 0.32 or newer as an array.
Without a device side it is the host mapping.
The tensor is valid while the frame object lives, and the runtime keeps the frame's memory from being
reused until then. `frame.writable()` is the GPU write door — a scope whose DLPack view a GPU package
edits in place, ordered ahead of the runtime's next read — and `frame.cpu()` the host door, a numpy
array whose name is the warning.

For the allocation itself, `ctx.gpu_full_access.export_dma_buf(surface)` hands native code a
DMA-BUF fd and `ctx.gpu_full_access.export_opaque_fd(surface)` the OPAQUE_FD flavour, with the
metadata a foreign Vulkan or CUDA import needs — both on Linux. On macOS
`ctx.gpu_full_access.export_iosurface(surface)` hands over a Mach send right to the surface's
IOSurface instead, and each flavour refuses by name on the other platform. A raw handle names the
allocation, never the frame: take it once at setup on a surface you own, and keep per-frame reach on
frames and the tensor doors above.

Stated honestly, this is zero-**CPU**-copy, not copy-free: on Linux a tiled texture reaches a linear
tensor through one GPU blit into an exportable staging buffer, because DLPack expresses strided
linear memory only.

No inference stack ships and none is planned. torch, ONNX Runtime, TensorRT, MLX — whatever you
already use is an ordinary dependency in your stream's venv, upgraded on your schedule.

</details>

<details>
<summary><b>The read policy is decided at the consumer</b> — delivery profiles and payloads</summary>

<br>

A logger that wants its bags in the order they were sent and a display that should always show the
newest frame want opposite things. Each input says which it is, and saying so is required — there
is no default to inherit by accident:

```python
video = Input(Image, delivery="newest")       # drains to the most recent bag, older ones passed over
events = Input(Records, delivery="ordered")   # bags in publication order, may fall behind
```

A profile names a read policy and nothing more. Neither promises delivery: both drop under
sustained pressure, no link ever blocks a producer, and a dropped bag is counted on its link in
`graph`. One output can feed a `newest` and an `ordered` reader at once.

What crosses a link is a message of one family. `connect` refuses a link whose output family,
names or codec the input does not accept, naming both ends. Past that there is no schema
registry, no negotiation, no versions and no code-generation step. An Image, Array or Channels
read is a fixed layout the runtime describes; a Packets read is the unit's bytes; a Records write
takes a plain `dict` and a read returns one. By name, `ctx.inputs.read(port_name)` hands you the
raw bytes, and `read(port_name, into=T)` hands them to `T.from_bytes`.

**It costs you** checking inside a record. A Records port checks only that its declared names are
present, so a value of the wrong type surfaces at the consumer while running, not when you wire
the graph.

</details>

<details id="gpu-without-the-vendor-lock">
<summary><b>GPU without the vendor lock</b> — and what that costs</summary>

<br>

Every GPU operation in the runtime goes through Vulkan. CUDA appears only at the edge, as the tensor
a frame hands to torch. On Linux, `libcuda` and the Vulkan loader are opened at run time, never
linked, so one build runs on any system that has them; the runtime also carries its
own Vulkan loader and Mesa's lavapipe, a CPU Vulkan driver, for a machine with no GPU driver. On
macOS the runtime carries its own Vulkan loader and MoltenVK and links only the system. It also carries the Slang shader
compiler, so writing a pixel effect or a kernel needs no shader toolchain.

**It costs you** any CUDA-specific fast path inside the runtime, permanently — a vendor trick that
would help is expressed through Vulkan interop or not at all. And portability in the design is not
portability in practice: NVIDIA on Linux x86_64 and Apple Silicon are what the project tests on.
Other vendors are untested rather than validated.

</details>

## What ships today

A stream on one machine — sources in, GPU work, your model, display or recording out — plus
observation and live changes from the CLI or an agent. The runtime ships no media nodes; each is a
package on PyPI:

| Module | |
|---|---|
| `tatolab-testpattern` | `TestPatternSource`: the sample animation, for a machine with no camera. |
| `tatolab-ops` | The small function nodes (`Lfo`, `Remap`, `Clamp`, `Gate`, …), compiled to native code and run inside the node that reads them; every new project depends on it. |
| `tatolab-virtualcamera` | `VirtualCameraSink`, Linux only: a v4l2loopback camera any other application can select. |

The microphone and speaker are modules: `pip install tatolab-microphone tatolab-speaker`, then
`from tatolab.microphone import MicrophoneSource` and `from tatolab.speaker import SpeakerSink`.
Each runs in its own helper process and moves its samples on a native thread over the
engine's audio backend, so no interpreter sits in the sample path either. The camera is one too:
`pip install tatolab-camera`, then `from tatolab.camera import CameraSource` — V4L2 on Linux and
AVFoundation on macOS, each frame imported to the GPU with no CPU copy. So are the hardware
video codecs: `pip install tatolab-h264 tatolab-h265`, then `from tatolab.h264 import H264Encoder,
H264Decoder` and `from tatolab.h265 import H265Encoder, H265Decoder` — Vulkan Video on a device
the helper owns on Linux, VideoToolbox on macOS. A scaffolded project depends on `tatolab-h264`.

Opus is a package: `uv add tatolab-opus` gives `tatolab.opus.OpusEncoder` and `OpusDecoder`,
libopus linked statically into the wheel.

From Python you also get compute, graphics and ray-tracing kernels written in Slang, a
model-input tensor kernel, and the stream clock every node reads as `ctx.time`.

**Platforms.** Two floors, the same API on both:

- **Linux x86_64** with a Vulkan GPU and driver; NVIDIA is what the project tests on.
- **Apple Silicon, macOS 26 or newer**, on the Vulkan driver the runtime carries; CI runs a
  scaffolded stream there. Ray-tracing kernels, `VirtualCameraSink` and the fd-shaped raw handles
  refuse by name on macOS.

A stream's venv runs CPython 3.12 to 3.13, GIL-enabled builds; the scaffold pins 3.12.

Not built yet: the macOS app and Windows.

## Troubleshooting

- **`error: no runtime is running on this machine: nothing answers at …`** — open Tato Studio,
  or start `tatolabd` in a terminal of its own. When another user's `tatolabd` holds the machine,
  the refusal names it: it serves that user's socket, not yours.
- **`another runtime holds this machine: …`** — `tatolabd` is already running; the refusal names
  its user, pid and executable.
- **`No display surface available: … neither WAYLAND_DISPLAY nor WAYLAND_SOCKET nor DISPLAY is
  set`** — the runtime started with no desktop session to give it a display, and its log said so at
  the start. Restart it from a terminal inside the session.
- **`<id> failed: … uv sync did not make …/.venv`** — the runtime makes a project's `.venv` with
  its own uv whenever `.venv/bin/python` does not run, and uv could not: the reason quotes uv's
  own words, usually a dependency that does not resolve. Fix `pyproject.toml` and run or save
  again. **`… the runtime unit carries no uv`** — the runtime was built without it; rebuild with
  `cargo xtask build-runtime`.
- **``… cannot import `tatolab.stream`: add `tatolab-stream` to the project's dependencies``** —
  the project does not depend on `tatolab-stream` yet: `uv add tatolab-stream`.
- **``the stream `<id>` failed: <traceback>``** in the project's log, shown by `dev` and
  `tatolab logs` — the stream's graph document does not load, a node raised, or its load was
  refused; the runtime lists it `failed` with that reason. Fix it and run or save again;
  `tatolab run` retries it. A folder with no `tatolab-project.json` is no project: its
  load fails saying so.
- **`tatolab dev: <refusal>; save again to queue another load`** — the runtime refused to queue
  the load at all; its refusal names why.
- **`No usable Vulkan driver (ICD) was found` or `No Vulkan loader library could be opened`**
  (Linux) — install your GPU vendor's Vulkan driver (the proprietary NVIDIA driver, or
  `mesa-vulkan-drivers` for AMD and Intel) and the loader (`libvulkan1`), then check that
  `vulkaninfo` runs.
- **No camera, or the wrong one** — the stream `tatolab new` writes starts from
  `TestPatternSource` and needs none. `tatolab.camera.CameraSource` takes the first capture device unless its `device_id` config names one (a
  V4L2 path such as `/dev/video2` on Linux, a camera's unique ID on macOS); a named device that
  cannot be opened is refused by name.
- **No picture on macOS** — the first run asks for camera access without waiting: the stream starts,
  and frames begin once you allow it. macOS asks on behalf of the application that started the
  runtime — your terminal — so a refusal names that application and the Camera setting to change.
- **`tatolab logs` shows nothing** — the runtime writes a project's log under that project's
  directory; run `tatolab logs` there, or name it with `--dir`.
- **A `VirtualCameraSink` cannot create its loopback camera** — run `tatolab enable-virtual-camera`
  once. It needs the `v4l2loopback` module for your kernel (`v4l2loopback-dkms` on Debian and
  Ubuntu). Until then the sink refuses at setup, naming the command.

## License

Licensed under [BUSL-1.1](LICENSE), converting automatically to
[Apache 2.0](LICENSES/Apache-2.0.txt) on **January 1, 2029**. [LICENSE](LICENSE) states the
parameters — the Licensor, the Licensed Work and the Additional Use Grant.

**What you build is yours; reselling the runtime itself needs a license.**

Free, no permission needed: building nodes, applications, robots, and products on it — commercial,
private, or open source; selling nodes you wrote with their source closed; personal, educational,
and research use.

A commercial license is required to sell, host as a managed service, white-label, or sublicense
**the runtime itself** — the engine, graph compiler, scheduler, processor execution, GPU context,
and link infrastructure — as your product, or to build a competing runtime from it.

Tatolab also distributes third-party code. Each dependency's copyright notice and licence
text, as of that file's last regeneration, is reproduced in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md), which the runtime carries beside
`tatolab.runtime` in its lend's `.dist-info/licenses/`.

[Commercial licensing](docs/license/COMMERCIAL-LICENSING.md) ·
[Partner licensing](docs/license/PARTNER-LICENSING.md) · [CLA](docs/license/CLA.md)

---

<div align="center">

[tatolab.com](https://tatolab.com) · [hello@tatolab.com](mailto:hello@tatolab.com)

</div>
