<div align="center">

# Tatolab

**Turn anything into a live stream.**<br>
Wire cameras, models and your own code into a stream. Expose an output, and apps, agents and your other machines read it by URL.

[![website](https://img.shields.io/badge/tatolab.com-0ea5e9?label=website)](https://tatolab.com)
[![license](https://img.shields.io/badge/license-BUSL--1.1-0ea5e9)](LICENSE)
[![python](https://img.shields.io/badge/python-3.10%20%E2%80%93%203.13-0ea5e9)](#install)
[![platform](https://img.shields.io/badge/platform-linux%20x86__64%20%7C%20macOS%20apple%20silicon-64748b)](#what-ships-today)
[![gpu](https://img.shields.io/badge/GPU-Vulkan-64748b)](#gpu-without-the-vendor-lock)

[Install](#install) · [Quickstart](#quickstart) · [Writing a stream](#writing-a-stream) · [The `tatolab` CLI](#the-tatolab-cli) · [Inspect a running stream](#inspect-a-running-stream) · [How it works](#how-it-works) · [What ships today](#what-ships-today) · [Troubleshooting](#troubleshooting) · [License](#license)

</div>

<!-- Demo GIF slot. Generate on the rig with `vhs docs/assets/demo.tape`, commit the
     result, then replace this comment with:
     <div align="center"><img src="docs/assets/demo.gif" alt="Inspecting a running stream" width="900"></div> -->

---

- **A stream is a Python function.** `@stream` wires nodes together, and a node is a `@node` class
  in a module of its own. There is no manifest, no registration file, and no build step between an
  edit and a run.
- **Every Python node runs in its own process**, with its own interpreter started from your stream's
  venv. A node that hangs or crashes is one node, named in the log; it cannot stall the others.
- **Frames stay on the GPU.** Capture lands in device memory and reaches torch through DLPack with no
  CPU copy, and a pixel effect is one GLSL function. Reading pixels on the CPU is an explicit
  `frame.cpu()`.
- **Native built-ins for the parts with deadlines**: camera, test pattern, microphone,
  speaker, H.264 / H.265 / Opus codecs, and a virtual camera on Linux — native code inside the
  runtime, configured from Python.
- **Agents work on a running stream.** The runtime serves MCP, so an agent can read the live graph,
  sample what a link carries, look at a frame, read the logs, and add, connect or remove nodes while
  the stream runs — the same operations the `tatolab` CLI uses.
- **A fast edit loop.** `tatolab dev` restarts the stream on every saved edit, and keeps the running
  one when an edit fails to compile.

> **Alpha.** APIs will change. On Linux one command installs the runtime; on macOS you build it
> from a checkout of this repository for now. One runtime per machine runs every stream on it. An
> exposed output is read by the streams and agents on its own machine today; reading it from your
> other machines is not built yet. See [what ships today](#what-ships-today).

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
machine's streams, and `tatolab`, the CLI that loads, manages and inspects them — into
`~/.local/share/tatolab/`, links both into `~/.local/bin`, and runs `tatolabd` as the systemd user
service `tatolabd.service`, restarted if it fails. Re-run it to upgrade. `sh -s -- --uninstall` in
place of `sh` removes it and leaves your streams' records and logs.

**Your stream** is an ordinary Python project. Its venv holds `tatolab-stream` — the Python API,
`import tatolab.stream` — and whatever your nodes import, and never the runtime. `tatolab-stream` is
on PyPI:

```bash
uv add tatolab-stream
```

`tatolab new` writes a `pyproject.toml` that depends on `tatolab-stream>=0.41` and `numpy>=2.1`, so
`uv sync` is the whole setup. Nothing is generated, compiled or downloaded at run time.

### Building the runtime from source

On macOS, until the app ships, and for working on the runtime itself, build it from a checkout of
this repository:

```bash
cargo xtask build-runtime --release              # omit --release for a debug build
export PATH="$PWD/target/tatolab-runtime/bin:$PATH"
```

That lays out `target/tatolab-runtime/` as an install prefix: `bin/tatolabd`, `bin/tatolab`,
`bin/moq-relay` — the machine's local relay, which `tatolabd` runs as two children, one on
loopback port 4443 and one on every interface at 4445 — `lib/tatolab/lend/`, the runtime's own
Python half (`tatolab.runtime`) that each Python node's interpreter borrows, and `lib/tatolab/watch/`,
the preview page `tatolabd` serves at `http://127.0.0.1:4446/watch/<machine>/<stream>`. Keep
`bin/tatolabd` beside `lib/` and `bin/moq-relay`: `tatolabd` finds all three relative to its own
executable.

The build needs a Rust toolchain and [uv](https://docs.astral.sh/uv/) on `PATH` (it runs the
pinned maturin through `uvx`), the [bun](https://bun.sh) `runtime/tatolab-watch/package.json` pins
in its `packageManager` field (it bundles the preview page, and refuses any other bun), plus a
native toolchain. On Debian or Ubuntu that is:

```bash
sudo apt install pkg-config protobuf-compiler libclang-dev libvulkan-dev glslc python3-dev cmake ninja-build
```

On Linux the build first stages a Vulkan loader and Mesa's lavapipe into the lend, built inside the
release's pinned manylinux image, so it also needs Docker. On macOS it is `glslc` (from shaderc),
CMake and Ninja, and the build first stages a Vulkan loader and MoltenVK into the lend, so the Mac
that runs the result needs no Vulkan SDK.

A runtime built from source has no service: start it with `tatolabd` in a terminal of its own.

## Quickstart

The installer already started this machine's runtime. Built from source, start it first:

```bash
tatolabd                    # in a terminal of its own: this machine's runtime
```

```bash
tatolab new my-stream       # test pattern → GPU effect, plus a CPU meter, wired and working
cd my-stream
uv sync                     # the stream's venv: tatolab-stream and numpy
tatolab dev                 # the test pattern through a GPU lens, encoded and shared, its brightness logged
```

`tatolabd` is the one runtime on this machine, and every stream runs inside it; no `tatolab` verb
starts one. `dev` asks it to load the project in the working directory: the runtime compiles
`stream.py`'s one `@stream` function to the stream's graph in the project's `.venv`, loads it and
starts it, and `dev` prints the stream's id and its records in your terminal. Save an edit and
`dev` runs the stream again: the runtime compiles the edit and only then replaces the running
stream, so an edit that fails to compile prints the refusal and leaves the running stream in place
until a save that compiles. Ctrl-C ends `dev` and leaves the stream running. Every stream the runtime
loads is recorded: it runs until `tatolab stop` or `tatolab rm`, and the runtime loads it again each
time it starts. `tatolab run` loads the stream once, without watching for edits, prints its id and
returns; `tatolab logs -f` follows its records.

## Writing a stream

`stream.py` is wiring and nothing else:

```python
from tatolab.stream import (
    Exposure,
    H264Encoder,
    StreamBuilder,
    TestPatternSource,
    stream,
)

from nodes.brightness_meter import BrightnessMeter
from nodes.halftone_lens_effect import HalftoneLensEffect


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The sample animation through the lens, encoded and shared; brightness logged each second."""
    # Point it at your camera: `stream_builder.add(CameraSource)`, imported
    # from `tatolab.stream`, publishes on the same `video` port.
    source = stream_builder.add(TestPatternSource)
    effect = stream_builder.add(HalftoneLensEffect)
    meter = stream_builder.add(BrightnessMeter)
    encoder = stream_builder.add(H264Encoder)
    stream_builder.connect(source.output("video"), effect.input("video_from_upstream"))
    stream_builder.connect(
        effect.output("video_to_downstream"), meter.input("video_from_upstream")
    )
    stream_builder.connect(effect.output("video_to_downstream"), encoder.input("video"))
    stream_builder.expose(encoder.output("encoded_video"), Exposure.PUBLIC)
```

The effect's output has two readers: the meter measures each frame, and the encoder turns it into
H.264, whose output `expose(…, Exposure.PUBLIC)` shares as a track a browser plays, on this machine
or off it. `add` names each node after its class, lowercased (`halftonelenseffect`), unless you
pass `name=`. Swap `TestPatternSource` for `CameraSource`, as the comment says, to run your camera
through the same effect. A built-in's `config` is a `TypedDict` — `CameraSourceConfig`,
`TestPatternSourceConfig` — so a type checker catches a misspelled key. `expose` without a level
makes an output private, readable by other streams and code on this machine.

Another stream, on this machine or another, reads that output through a `RelayTrackSource` node,
which subscribes to the port's track at the relay; a link's two ends are always in one stream.

Pixels stay on the GPU. `nodes/halftone_lens_effect.py` is one shader function, with a lens that
drifts around the frame and turns what it covers into dots:

```python
from tatolab.stream import (
    GlslPixelEffect,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    msgpack,
    node,
)

# The whole effect: the output pixel for the source pixel at `at`, with each
# channel in 0.0-1.0. A lens drifts around the frame and turns what it covers
# into dots. `dials` are the values `process()` hands it each frame. Edit it
# while `tatolab dev` runs to see the change.
HALFTONE_LENS_GLSL = """
// How big the drifting lens is, as a share of the frame's height.
const float LENS_RADIUS = 0.3;
// How many pixels apart the dots sit under the lens.
const float DOT_CELL_SIZE = 8.0;
// How far the gaps between the dots fade toward grey.
const float GAP_FADE = 0.35;

vec4 effect(vec4 source, ivec2 at) {
    vec2 pixel = vec2(at) + 0.5;
    vec2 lens_centre = dials.frame_size * vec2(
        0.5 + 0.3 * sin(dials.seconds * 0.4),
        0.5 + 0.25 * sin(dials.seconds * 0.55)
    );
    float distance_from_lens_centre = length(pixel - lens_centre) / dials.frame_size.y;
    float under_the_lens =
        1.0 - smoothstep(LENS_RADIUS * 0.92, LENS_RADIUS, distance_from_lens_centre);

    // Each cell keeps a round dot of the picture and fades the gap around it
    // toward grey, so the dots show on every colour, black included.
    vec2 cell_centre = (floor(pixel / DOT_CELL_SIZE) + 0.5) * DOT_CELL_SIZE;
    float dot_radius = DOT_CELL_SIZE * 0.42;
    float in_the_dot = 1.0 - smoothstep(dot_radius - 1.0, dot_radius, length(pixel - cell_centre));
    vec3 gap = mix(source.rgb, vec3(0.5), GAP_FADE);
    vec3 dotted = mix(gap, source.rgb, in_the_dot);

    return vec4(mix(source.rgb, dotted, under_the_lens), source.a);
}
"""

NANOSECONDS_PER_SECOND = 1_000_000_000


@node
class HalftoneLensEffect:
    """Turns what a slowly drifting lens covers into dots on the GPU, and passes each frame on."""

    @node.input(delivery_profile="newest")
    def video_from_upstream(self) -> VideoFrame: ...

    @node.output()
    def video_to_downstream(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.halftone_lens_pixel_effect = GlslPixelEffect.compile(
            ctx.gpu_full_access,
            effect_glsl=HALFTONE_LENS_GLSL,
            dials={"seconds": "float", "frame_size": "vec2"},
        )
        self.first_frame_at_ns: int | None = None

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
        if frame is None:
            return
        # `ctx.time` is the monotonic clock every node shares, in ns.
        if self.first_frame_at_ns is None:
            self.first_frame_at_ns = ctx.time
        halftone_frame_record = self.halftone_lens_pixel_effect.apply_to_frame(
            ctx.gpu_limited_access,
            frame,
            dials={
                "seconds": (ctx.time - self.first_frame_at_ns) / NANOSECONDS_PER_SECOND,
                "frame_size": (frame.width, frame.height),
            },
        )
        ctx.outputs.write("video_to_downstream", msgpack.pack(halftone_frame_record))
```

Logic runs on the CPU. `nodes/brightness_meter.py` reads each frame back through an explicit
`frame.cpu()` view and logs its mean brightness once a second. It sits on a fan-out in its own
process, so it never slows the picture — it is the node you replace with your model call.

What a node declares:

- **Ports**, with `@node.input` and `@node.output` on marker methods. The return annotation is for
  you and your type checker; the runtime never compares types. Every input names its
  [delivery profile](#how-it-works), `newest` or `ordered`.
- **When it runs**, with `@node(execution=…)`: `reactive` (the default once it has an input),
  `manual` (driven by a callback it owns), or `continuous` with `interval_ms`. A source has nothing
  to react to, so it must name one. `scheduling` asks for `realtime`, `high` or `normal` priority.
- **Its configuration**, as the annotation on `__init__`'s `config` parameter — a `TypedDict`, a
  dataclass or a pydantic model — passed as `stream_builder.add(MyNode, config={...})`.

A node class lives in a module of its own, never in `stream.py`: its process imports the class by
name, and `stream.py` is not importable under that name. `add` refuses such a class with a message
naming the fix.

A file may define several streams. With more than one, name the one to run:
`tatolab dev stream.py:<function>`, or `tatolab run <module>:<function>` for a stream in an
installed package.
`-f FILE` runs another entry file in place of `stream.py`, `--dir DIR` resolves against another
project directory, and `--name NAME` shows the stream under a display name other than its
function's; a load without it keeps the display name the stream has, and `tatolab rename` changes
it.
A stream is keyed and addressed by its **stream id**, `<folder>-<function>-<hash>`, which the
runtime computes from where its code lives: the project directory, resolved, and the stream
function as `module:function`. Two checkouts of one project get two ids, and running a project
again replaces its stream under the same id.

## The `tatolab` CLI

```text
tatolab new DIRECTORY
tatolab run [TARGET] [-f FILE] [--dir DIR] [--name NAME]
tatolab dev [TARGET] [-f FILE] [--dir DIR] [--name NAME]

tatolab streams
tatolab stop [STREAM]
tatolab start [STREAM]
tatolab rm [STREAM]
tatolab rename [STREAM] DISPLAY_NAME
tatolab expose [STREAM] NODE PORT [--public | --remove]

tatolab relays
tatolab relay add ADAPTER [--settings-file PATH]
tatolab relay set RELAY_ID [KEY=VALUE ...] [--secret FIELD (--from-file PATH | --from-stdin)]
tatolab relay rm RELAY_ID

tatolab graph [--stream STREAM]
tatolab logs [--stream STREAM | RUNTIME_ID-STREAM] [-f | --follow] [--processor ID] [--pipeline ID]
             [--rhi] [--level trace|debug|info|warn|error] [--source rust|python] [--intercepted-only]
tatolab logs --list
tatolab mcp

tatolab enable-virtual-camera [--print]
```

- **`new`** writes `stream.py`, `nodes/`, `pyproject.toml`, `.python-version` (3.12) and
  `.gitignore` into `DIRECTORY`, and refuses rather than overwrite a file already there.
- **`run`** asks the runtime to compile the project's stream in its `.venv`, load it and record it,
  then prints `<id> running as "<display name>" (project <dir>, <module:function>)` and returns
  0. A stream whose code fails is not refused: the runtime records it failed and `run` prints
  `<id> failed: <reason>` with its traceback and exits 1, as it does naming why when the load is
  refused. The runtime loads the stream again at each of its own starts until `stop` or `rm`,
  and running the same project and function again replaces it under the same id — a failed one
  included. `TARGET` is `<file>.py[:<function>]` or
  `<module>:<function>`. The id it prints is the one the other verbs take.
- **`dev`** is `run`, then the stream's records followed, run again on every saved edit to a `.py`
  file or `pyproject.toml` in the project; the runtime replaces the running stream, and an edit
  whose code fails unloads it and records it failed, its traceback printed, until the next save. Ctrl-C ends `dev` and leaves the stream running. When the runtime goes away, `dev`
  says so, waits for it to load the stream again from its record, and follows it again.
- **`streams`** lists the streams the runtime holds: each one's id and display name, whether it is
  running, stopped or failed, its node count, its project and its stream function, and why each
  failed one failed. A
  stream implicated in the runtime's last two crashes in a row, or one that cannot re-load at the
  runtime's start (its venv deleted, say), is failed: the runtime skips it at its starts and
  brings every other stream back.
- **`STREAM`**, in `stop`, `start`, `rm`, `rename`, `expose` and `logs --stream`, is a stream id,
  or a display name exactly one stream holds. Omitted, it is the one stream loaded from the
  working directory, stopped and failed ones included. An argument several streams match, or none,
  is refused listing each candidate's id and display name. `graph --stream` is the exception: it
  takes a stream id, and omitted it means every loaded stream.
- **`stop`** unloads a stream, which stays stopped across the runtime's restarts until
  **`start`** loads it again. `start` also retries a failed stream. **`rm`** unloads a stream and
  forgets it. **`rename`** shows a stream under another display name — any text, never part of
  its address. The stream's record holds it, across the runtime's restarts and re-loads without
  `--name`.
- **`expose`** sets how far one output port is readable, live: private (readable by this machine's
  other streams and agents), `--public` (readable off the machine too) or `--remove` (internal to
  its stream). A reader the new level no longer allows is cut at once. The level is recorded and
  wins over what the stream function exposes, across restarts.
- **`relays`** lists the relays the runtime joins — the local relay's two listeners and each one
  added — with each one's adapter, connection state and settings, and each secret setting as set
  or unset, never its value. **`relay add`** joins a relay through an adapter, from a JSON file of
  its settings; **`relay set`** replaces settings, or reads one secret from a file or stdin, never
  an argument; **`relay rm`** leaves a relay and forgets its secrets. A secret is kept in the OS
  secret store, never in `relays.json`.
- **`graph`** prints every loaded stream's nodes, ports, links and exposed ports as JSON, with each
  node's and link's state and counters, under the machine's name as `machine`; `--stream` prints
  one stream's graph alone, in the shape a load takes. The machine's name is its hostname, less a
  trailing mDNS `.local`, cast to lowercase URL-safe.
- **`logs`** reads a loaded stream's records from the runtime — `--stream`'s, or the one loaded
  from the working directory — rendered the way the runtime prints them; `--follow` keeps reading
  as records land until Ctrl-C, which leaves the stream running, and ends with exit 1 naming the
  runtime's log when the runtime goes away. `--list` and `RUNTIME_ID-STREAM` read a stream's JSONL log on disk instead. Run
  them in the stream's project directory, where the runtime writes each stream's log: `--list`
  shows the stream logs there, each named `<runtime_id>-<stream>`, and `RUNTIME_ID-STREAM` renders
  one. The other flags filter what either shows.
- **`mcp`** connects an MCP host to the runtime over its own stdin and stdout; see below.
- **`enable-virtual-camera`** installs, once, the permission a `VirtualCameraSink` needs for its
  loopback camera: the `v4l2loopback` module loaded with no devices, and its control node handed to
  the logged-in user through udev. It is one privileged step behind your desktop's password prompt
  (`sudo` in a headless shell), Linux only; `--print` writes the files and commands for a hand
  install and changes nothing.

Every verb but `new`, `enable-virtual-camera` and `logs --list` or `logs RUNTIME_ID-STREAM` (which
read the JSONL on disk) talks to the runtime through its local socket, at one fixed path per user. With no runtime
running, each refuses at once, naming the socket and `tatolabd` as the way to start one.

## Inspect a running stream

Run these on the machine the stream runs on. They reach the runtime through its local socket — a
Unix socket only your user can open — so control is reachable only on that machine.

```console
$ tatolab graph | jq -r .machine
desk

$ curl -s http://127.0.0.1:4446/
{"machine":"desk","streams":[{"stream":"hello-main-3f2a9c","display_name":"main","exposed_output_ports":[{"node":"h264encoder","port":"encoded_video","level":"public",...}]}]}
```

`graph` shows every node's state and counters, so it says whether data is moving; `logs` reads
what every node logged, Rust and Python alike. What a port carries is read the way anyone reads it:
an exposed port is the track `<node>/<port>` of the broadcast `<machine>/<stream id>.hang` at the
machine's local relay, which answers on this machine at `http://127.0.0.1:4443`.
`http://127.0.0.1:4446/watch` lists the loaded streams by display name and id, and
`http://127.0.0.1:4446/watch/desk/hello-main-3f2a9c` plays the stream's video in a browser, and
any MoQ client reads its tracks — this repository's `tatolab-read-track` writes a track's objects
to files:

```console
$ cargo run -p tatolab-read-track -- http://127.0.0.1:4443 desk/hello-main-3f2a9c.hang h264encoder/encoded_video --count 30 --out frames/
```

An `H264Encoder`'s track files, concatenated, are an H.264 stream any decoder plays; an
`H265Encoder`'s are an HEVC one. A port that is not exposed is internal to its stream: `tatolab
expose hello-main-3f2a9c <node> <port>` makes it a track on this machine. A raw frame's port carries a GPU
surface's id, which means nothing outside the runtime, so to see a mid-graph frame, connect an
encoder to it and expose the encoder.

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
`remove_node` to change a running graph; `run_stream`, `stop_stream`, `start_stream`,
`remove_stream`, `rename_stream`, `list_streams` and `expose_port` to manage the streams the runtime holds; and
`add_relay`, `configure_relay`, `remove_relay` and `list_relays` to manage the relays it joins. Every tool
about one stream names it with `stream`, and `connect` links two ports of that one stream. Beside
them the runtime serves the node catalog — every
node type it can add, with its description, config schema and ports — and the live graph as
resources, and four prompts whose every step is a tool call. An agent can write a
node class into a module beside `stream.py`, or `uv add` a package that ships one, then add it by
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
Support/Tatolab/` on macOS: a record of each stream it runs — its graph, project, interpreter,
whether it is stopped, and the levels `expose` set — and its own log. At each start it loads every
recorded stream that is neither stopped nor failed.

`tatolab run` asks it to load a project's stream. The runtime runs `tatolab.stream`'s compile entry
in the project's `.venv` interpreter, which imports `stream.py`, calls the `@stream` function and
hands back the graph it built; the CLI compiles nothing. The runtime loads that graph, starts it and
records it, and `tatolab run` returns.

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
node's class lives in an importable module rather than in `stream.py`.

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
WHEP play, `WhipPublisher` and `WhepPlayer`, whose native code runs in those nodes' own processes.
No package extends the runtime — there is no plugin ABI, no manifest and no lockfile of ours, only
an ordinary `pyproject.toml`.

**It costs you** a small set of built-ins. Camera, test pattern, microphone, speaker, the
H.264 / H.265 / Opus codec pairs and the virtual camera ship inside the runtime because their
per-frame paths have deadlines a separate process cannot meet, need primitives only the
runtime has, or present a device to other applications; everything else is a package or a node you
write.

</details>

<details>
<summary><b>Device memory, not a copy</b> — handing frames to torch</summary>

<br>

```python
frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
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
@node.input(delivery_profile="newest")     # drains to the most recent bag, older ones passed over
@node.input(delivery_profile="ordered")    # bags in publication order, may fall behind
```

A profile names a read policy and nothing more. Neither promises delivery: both drop under
sustained pressure, no link ever blocks a producer, and a dropped bag is counted on its link in
`graph`. One output can feed a `newest` and an `ordered` reader at once.

What crosses a link is a message whose payload is bytes, which the runtime never encodes or
decodes. No schema registry, no negotiation, no versions, no code-generation step, and the
runtime never compares one node's types against another's. The built-ins carry msgpack maps, and
`tatolab.stream.msgpack` packs and unpacks them. Strictness is a dial you turn at your own read:
`ctx.inputs.read(port)` hands you the bytes, and `read(port, into=T)` hands them to
`T.from_bytes` — `into=msgpack_typed_dict_cast(T)` types a `TypedDict` for free, while a
dataclass or pydantic model taking `MsgpackCast` raises on a payload that doesn't fit.

**It costs you** compile-time safety. A mismatch surfaces as a decode failure at the consumer while
running, not when you wire the graph.

</details>

<details id="gpu-without-the-vendor-lock">
<summary><b>GPU without the vendor lock</b> — and what that costs</summary>

<br>

Every GPU operation in the runtime goes through Vulkan. CUDA appears only at the edge, as the tensor
a frame hands to torch. On Linux, `libcuda` and the Vulkan loader are opened at run time, never
linked, so one build runs on any system that has them; the runtime also carries its
own Vulkan loader and Mesa's lavapipe, a CPU Vulkan driver, for a machine with no GPU driver. On
macOS the runtime carries its own Vulkan loader and MoltenVK and links only the system. It also carries its own GLSL compiler,
so writing a pixel effect or a kernel needs no shader toolchain.

**It costs you** any CUDA-specific fast path inside the runtime, permanently — a vendor trick that
would help is expressed through Vulkan interop or not at all. And portability in the design is not
portability in practice: NVIDIA on Linux x86_64 and Apple Silicon are what the project tests on.
Other vendors are untested rather than validated.

</details>

## What ships today

A stream on one machine — sources in, GPU work, your model, display or recording out — plus
observation and live changes from the CLI or an agent. The built-in nodes are native code inside the
runtime, configured from Python, and their per-frame paths never enter an interpreter:

| Built-in | |
|---|---|
| `CameraSource` | V4L2 on Linux (zero-copy DMA-BUF when the device exports it), AVFoundation on macOS (zero-copy IOSurface import). |
| `TestPatternSource` | Color bars, for a machine with no camera. |
| `MicrophoneSource`, `SpeakerSink` | The machine's audio backend, as timestamped sample blocks. |
| `H264Encoder`, `H264Decoder`, `H265Encoder`, `H265Decoder` | Hardware codecs: Vulkan Video on Linux, VideoToolbox on macOS. |
| `OpusEncoder`, `OpusDecoder` | libopus. |
| `VirtualCameraSink` | Linux only: a camera any other application can select, through v4l2loopback or PipeWire. |

From Python you also get `GlslPixelEffect`, compute, graphics and ray-tracing kernels, a
model-input tensor kernel, and the monotonic clock every node shares.

**Platforms.** Two floors, the same API on both:

- **Linux x86_64** with a Vulkan GPU and driver; NVIDIA is what the project tests on.
- **Apple Silicon, macOS 15 or newer**, on the Vulkan driver the runtime carries; CI runs a
  scaffolded stream there. Ray-tracing kernels, `VirtualCameraSink` and the fd-shaped raw handles
  refuse by name on macOS.

A stream's venv runs CPython 3.10 to 3.13, GIL-enabled builds; the scaffold pins 3.12.

Not built yet: the macOS app, serving an exposed port off the machine, and Windows.

## Troubleshooting

- **`error: no runtime is running on this machine: nothing answers at …`** — installed on Linux,
  `systemctl --user status tatolabd` says why the service is down and `systemctl --user start
  tatolabd` starts it; built from source, start `tatolabd` in a terminal of its own. When another user's `tatolabd` holds the machine, the refusal names it: it
  serves that user's socket, not yours.
- **`another runtime holds this machine: …`** — `tatolabd` is already running; the refusal names
  its user, pid and executable.
- **`No display surface available: … neither WAYLAND_DISPLAY nor WAYLAND_SOCKET nor DISPLAY is
  set`** — the runtime started with no desktop session to give it a display, and its log said so at
  the start. Logging in to one restarts the installed service with it; a runtime started by hand
  takes `systemctl --user restart tatolabd`, or a restart from a terminal inside the session.
- **``… has no .venv/bin/python; run `uv sync` in …``** — the runtime compiles a stream in the
  `.venv` of its project directory: the working directory, or `--dir`. Create it with `uv sync`.
- **``… cannot import `tatolab.stream`: add `tatolab-stream` to the project's dependencies``** —
  the project does not depend on `tatolab-stream` yet: `uv add tatolab-stream`.
- **A node defined in `stream.py` is refused** — move the class into a module beside `stream.py` and
  import it from there.
- **`<id> failed: <traceback>`** from `run`, or **`tatolab dev: <id> is failed — fix it and save
  again to load it`** — the stream's code raised, or its load was refused; the runtime lists it
  `failed` with that reason. Fix the code and run or save again; `tatolab start <id>` retries it.
  A `stream.py` that does not import fails every stream of the project, or with none lists the
  project as `<folder>-<hash>`.
- **`tatolab dev: no stream is loaded — fix it and save again`** — the runtime refused the load
  itself, with nothing running before it; its refusal is printed just above.
- **`No usable Vulkan driver (ICD) was found` or `No Vulkan loader library could be opened`**
  (Linux) — install your GPU vendor's Vulkan driver (the proprietary NVIDIA driver, or
  `mesa-vulkan-drivers` for AMD and Intel) and the loader (`libvulkan1`), then check that
  `vulkaninfo` runs.
- **No camera, or the wrong one** — the stream `tatolab new` writes starts from
  `TestPatternSource` and needs none. `CameraSource` takes the first capture device unless its `device_id` config names one (a
  V4L2 path such as `/dev/video2` on Linux, a camera's unique ID on macOS); a named device that
  cannot be opened is refused by name.
- **No picture on macOS** — the first run asks for camera access without waiting: the stream starts,
  and frames begin once you allow it. macOS asks on behalf of the application that started the
  runtime — your terminal — so a refusal names that application and the Camera setting to change.
- **`tatolab logs --list` shows nothing** — the runtime writes each stream's log under that stream's
  project directory; run `tatolab logs` there, or read a loaded stream's records with
  `tatolab logs --stream`.
- **A `VirtualCameraSink` cannot create its loopback camera** — run `tatolab enable-virtual-camera`
  once. It needs the `v4l2loopback` module for your kernel (`v4l2loopback-dkms` on Debian and
  Ubuntu). Until then the sink's default door registers a PipeWire camera instead.

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
