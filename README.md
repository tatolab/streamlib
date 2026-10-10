<div align="center">

# Tatolab

**Live streams as pipes, written in Python.** Wire a camera, a GPU effect, your model and a window<br>
in a few lines; a native runtime runs it on your machine, and you and your agents can look inside while it runs.

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
- **Native built-ins for the parts with deadlines**: camera, window, test pattern, microphone,
  speaker, H.264 / H.265 / Opus codecs, MP4 recording, and a virtual camera on Linux — native code
  inside the runtime, configured from Python.
- **Agents work on a running stream.** The runtime serves MCP, so an agent can read the live graph,
  sample what a link carries, look at a frame, read the logs, and add, connect or remove nodes while
  the stream runs — the same operations the `tatolab` CLI uses.
- **A fast edit loop.** `tatolab dev` restarts the stream on every saved edit, and keeps the running
  one when an edit fails to compile.

> **Alpha.** APIs will change. The runtime has no installer yet — you build it from a checkout of
> this repository — and one runtime runs one stream. See [what ships today](#what-ships-today).

## Built for

Tatolab is a simple SDK for defining live streams as pipes and sharing them with people and agents.
Today the pipes run on one machine, and you and your agents work on them there.

- **Live video and audio with a model in the loop** — a camera into torch on the GPU, the result in a
  window, in an MP4, or on a virtual camera that other applications can pick.
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

There are two halves, and they install separately.

**The runtime** — `tatolabd`, the native program that hosts this machine's streams, and `tatolab`,
the CLI that loads, manages and inspects them — has no installer yet. Build it from a checkout of
this repository:

```bash
cargo xtask build-runtime --release              # omit --release for a debug build
export PATH="$PWD/target/tatolab-runtime/bin:$PATH"
```

That lays out `target/tatolab-runtime/` as an install prefix: `bin/tatolabd`, `bin/tatolab`, and
`lib/tatolab/lend/`, the runtime's own Python half (`tatolab.runtime`) that each Python node's
interpreter borrows. Keep `bin/tatolabd` beside `lib/`: `tatolabd` finds `lib/tatolab/lend/`
relative to its own executable.

The build needs a Rust toolchain and [uv](https://docs.astral.sh/uv/) on `PATH` (it runs the
pinned maturin through `uvx`), plus a native toolchain. On Debian or Ubuntu that is:

```bash
sudo apt install pkg-config protobuf-compiler libclang-dev libvulkan-dev glslc python3-dev cmake ninja-build
```

On macOS it is `glslc` (from shaderc), CMake and Ninja, and the build first stages a Vulkan loader
and MoltenVK into the lend, so the Mac that runs the result needs no Vulkan SDK.

**Your stream** is an ordinary Python project. Its venv holds `tatolab-stream` — the Python API,
`import tatolab.stream` — and whatever your nodes import, and never the runtime. `tatolab-stream` is
on PyPI:

```bash
uv add tatolab-stream
```

`tatolab new` writes a `pyproject.toml` that depends on `tatolab-stream>=0.41` and `numpy>=2.1`, so
`uv sync` is the whole setup. Nothing is generated, compiled or downloaded at run time.

## Quickstart

```bash
tatolabd                    # in a terminal of its own: this machine's runtime
```

```bash
tatolab new my-stream       # camera → GPU effect → window, plus a CPU meter, wired and working
cd my-stream
uv sync                     # the stream's venv: tatolab-stream and numpy
tatolab dev                 # your camera, live and inverted, in a window
```

No camera on this machine? `tatolab new my-stream --test-pattern` wires the built-in test pattern
instead.

`tatolabd` is the one runtime on this machine, and every stream runs inside it; no `tatolab` verb
starts one. `dev` asks it to load the project in the working directory: the runtime compiles
`stream.py`'s one `@stream` function to the stream's graph in the project's `.venv`, loads it and
starts it, and `dev` prints the stream's records in your terminal. The stream is attached: Ctrl-C
unloads it. Save an edit and `dev` runs the stream again: the runtime compiles the edit and only
then replaces the running stream, so an edit that fails to compile prints the refusal and leaves the
running stream in place until a save that compiles. `tatolab run` does the
same once, without watching for edits, and `tatolab run -d` hands the stream to the runtime to keep:
it runs on after the command returns, and the runtime loads it again each time it starts.

## Writing a stream

`stream.py` is wiring and nothing else:

```python
from tatolab.stream import CameraSource, DisplayWindow, StreamBuilder, stream

from nodes.brightness_meter import BrightnessMeter
from nodes.inverting_effect import InvertingEffect


@stream
def main(stream_builder: StreamBuilder) -> None:
    """Camera, inverted, in a window; brightness logged once a second."""
    source = stream_builder.add(CameraSource)
    effect = stream_builder.add(InvertingEffect)
    meter = stream_builder.add(BrightnessMeter)
    window = stream_builder.add(
        DisplayWindow, config={"title": "Tatolab", "scaling": "fit"}
    )
    stream_builder.connect(source.output("video"), effect.input("video_from_upstream"))
    stream_builder.connect(effect.output("video_to_downstream"), window.input("video"))
    stream_builder.connect(
        effect.output("video_to_downstream"), meter.input("video_from_upstream")
    )
    stream_builder.expose(effect.output("video_to_downstream"))
```

One output, two readers: the window shows the frame, the meter measures it. `add` names each node
after its class, lowercased (`invertingeffect`), unless you pass `name=`. A built-in's `config` is
a `TypedDict` — `CameraSourceConfig`, `DisplayWindowConfig` — so a type checker catches a
misspelled key. `expose` makes an output private, readable by other streams and code on this
machine; `expose(output, Exposure.PUBLIC)`, with `Exposure` imported from `tatolab.stream`, makes
it public, readable off the machine too. Reading an exposed port from outside its stream is not
built yet.

Pixels stay on the GPU. `nodes/inverting_effect.py` is one shader function:

```python
from tatolab.stream import (
    GlslPixelEffect,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    node,
)

INVERT_GLSL = """
vec4 effect(vec4 source, ivec2 at) {
    // Color channels only — inverting alpha would erase the picture.
    return vec4(1.0 - source.rgb, source.a);
}
"""


@node
class InvertingEffect:
    """Inverts every frame's colors on the GPU and passes it on."""

    @node.input(delivery_profile="newest")
    def video_from_upstream(self) -> VideoFrame: ...

    @node.output()
    def video_to_downstream(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.inverting_pixel_effect = GlslPixelEffect.compile(
            ctx.gpu_full_access, effect_glsl=INVERT_GLSL
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
        if frame is None:
            return
        ctx.outputs.write(
            "video_to_downstream",
            self.inverting_pixel_effect.apply_to_frame(ctx.gpu_limited_access, frame),
        )
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
project directory, and `--name NAME` loads the stream under a name other than its function's.

## The `tatolab` CLI

```text
tatolab new DIRECTORY [--test-pattern]
tatolab run [TARGET] [-f FILE] [--dir DIR] [--name NAME] [-d | --detach]
tatolab dev [TARGET] [-f FILE] [--dir DIR] [--name NAME]

tatolab streams
tatolab stop STREAM
tatolab start STREAM
tatolab rm STREAM
tatolab expose STREAM NODE PORT [--public | --remove]

tatolab graph [--stream STREAM]
tatolab tap CHANNEL --stream STREAM [--count N] [--max-bag-bytes BYTES]
tatolab logs (--stream STREAM | RUNTIME_ID-STREAM) [-f | --follow] [--processor ID] [--pipeline ID]
             [--rhi] [--level trace|debug|info|warn|error] [--source rust|python] [--intercepted-only]
tatolab logs --list
tatolab exchange SURFACE_ID --out DIR
tatolab exchange --channel CHANNEL --stream STREAM [--count N] [--every N] [--field NAME] --out DIR
tatolab mcp

tatolab enable-virtual-camera [--print]
```

- **`new`** writes `stream.py`, `nodes/`, `pyproject.toml`, `.python-version` (3.12) and
  `.gitignore` into `DIRECTORY`, and refuses rather than overwrite a file already there.
- **`run`** asks the runtime to compile the project's stream in its `.venv` and load it, attached:
  the stream's records in your terminal, and the stream lives as long as the command — Ctrl-C, a
  closed terminal or a killed `tatolab` unloads it. `-d` hands it to the runtime to keep instead:
  `run` prints one line and returns, and the runtime loads the stream again at each of its own
  starts until `stop` or `rm`. `TARGET` is `<file>.py[:<function>]` or `<module>:<function>`. A
  name already loaded or kept is refused naming the project that holds it; `--name` loads the
  stream under another.
- **`dev`** is `run` attached, run again on every saved edit to a `.py` file or `pyproject.toml`
  in the project; the runtime replaces the running stream only once the edit compiles.
  When the runtime goes away, `dev` waits for it and loads the stream again.
- **`streams`** lists the streams the runtime holds: each one's name, whether it is attached, kept,
  stopped or failed, its node count and its project, and why each failed one failed. A kept
  stream implicated in the runtime's last two crashes in a row, or one that cannot re-load at the
  runtime's start (its venv deleted, say), is failed: the runtime skips it at its starts and
  brings every other stream back.
- **`stop`** unloads a stream; a kept one stays stopped across the runtime's restarts until
  **`start`** loads it again. `start` also retries a failed stream. **`rm`** unloads a stream and
  forgets it.
- **`expose`** sets how far one output port is readable, live: private (readable by this machine's
  other streams and agents), `--public` (readable off the machine too) or `--remove` (internal to
  its stream). A reader the new level no longer allows is cut at once. On a kept stream the level
  is recorded and wins over what the stream function exposes, across restarts.
- **`graph`** prints every loaded stream's nodes, ports, links and exposed ports as JSON, with each
  node's and link's state and counters, under the runtime's name as `runtime_name`; `--stream`
  prints one stream's graph alone, in the shape a load takes. The runtime's name defaults to
  `<host>-<the directory tatolabd started in>-<id>`; `STREAMLIB_RUNTIME_NAME` in `tatolabd`'s
  environment sets it.
- **`tap`** samples the raw bags one output port of `--stream` carries. The channel is
  `<runtime_name>/<node>/<port>`, spelled as `graph` names them. The sample is bounded and never
  blocks the producer: a quiet port returns a partial sample rather than hanging, and
  `--max-bag-bytes` raises the per-bag cap when a bag comes back flagged as truncated.
- **`logs --stream`** reads a loaded stream's records from the runtime, rendered the way the runtime
  prints them; `--follow` keeps reading as records land. Without `--stream` it reads a stream's
  JSONL log on disk. Run it in the stream's project directory, where the runtime writes each
  stream's log: `--list` shows the stream logs there, each named `<runtime_id>-<stream>`, and
  `RUNTIME_ID-STREAM` renders one. The other flags filter what either shows.
- **`exchange`** turns a published surface id (`<slot>#<generation>`, as a bag carries it) into that
  frame's exact, full-resolution PNG in `--out`, and prints each written path on stdout. With
  `--channel` and `--stream` it taps the port, reads the id from each sampled bag (the `surface_id`
  field unless `--field` names another) and exchanges `--count` frames, every `--every`th bag — no
  window in the graph and no display server in the path.
- **`mcp`** connects an MCP host to the runtime over its own stdin and stdout; see below.
- **`enable-virtual-camera`** installs, once, the permission a `VirtualCameraSink` needs for its
  loopback camera: the `v4l2loopback` module loaded with no devices, and its control node handed to
  the logged-in user through udev. It is one privileged step behind your desktop's password prompt
  (`sudo` in a headless shell), Linux only; `--print` writes the files and commands for a hand
  install and changes nothing.

Every verb but `new`, `enable-virtual-camera` and `logs` without `--stream` (which reads the JSONL
on disk) talks to the runtime through its local socket, at one fixed path per user. With no runtime
running, each refuses at once, naming the socket and `tatolabd` as the way to start one.

## Inspect a running stream

Run these on the machine the stream runs on. They reach the runtime through its local socket — a
Unix socket only your user can open — so control is reachable only on that machine.

```console
$ tatolab graph | jq -r .runtime_name
desk-home-8kq3

$ tatolab tap desk-home-8kq3/invertingeffect/video_to_downstream --stream main --count 3
{
  "channel": "desk-home-8kq3/invertingeffect/video_to_downstream",
  "requested": 3,
  "received": 3,
  "window_ms": 500,
  "dropped_bags": 0,
  ...
  "bags": [
    { "byte_len": 214, "hex_preview": "84aa73...", "hex_truncated": false },
    ...
  ]
}
```

`tap` returns what the link really carried.
`tatolab exchange --channel <that channel> --stream main --count 2 --out frames/` writes two of those frames as
PNGs and prints their paths, so you see the pixels of a mid-graph port without adding a window to
the graph. `logs` reads what every node logged, Rust and Python alike.

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
runtime and comes and goes with it, and a stream the host loads attached lives as long as the
host's connection.

The tools are `graph`, `tap`, `logs` and `exchange` to observe; `add_node`, `connect`, `disconnect`
and `remove_node` to change a running graph; and `run_stream`, `stop_stream`, `start_stream`,
`remove_stream`, `list_streams` and `expose_port` to manage the streams the runtime holds. Every tool
about one stream names it with `stream`. Beside them the runtime serves the node catalog — every
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
Support/Tatolab/` on macOS: a record of each kept stream — its graph, project, interpreter, whether
it is stopped, and the levels `expose` set — and its own log. At each start it loads every kept
stream that is not stopped.

`tatolab run` asks it to load a project's stream. The runtime runs `tatolab.stream`'s compile entry
in the project's `.venv` interpreter, which imports `stream.py`, calls the `@stream` function and
hands back the graph it built; the CLI compiles nothing. The runtime loads that graph and starts it.
Attached, the stream lives as long as `tatolab run`'s connection to the runtime; with `-d`, the
runtime keeps it.

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

**It costs you** a small set of built-ins. Camera, window, test pattern, microphone, speaker, the
H.264 / H.265 / Opus codec pairs, an MP4 sink and the virtual camera ship inside the runtime because
their per-frame paths have deadlines a separate process cannot meet, need primitives only the
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

What crosses a link is a self-describing named map. No schema registry, no negotiation, no
versions, no code-generation step, and the runtime never compares one node's types against
another's. Strictness is a dial you turn at your own read: `ctx.inputs.read(port)` hands you a
mapping, and `read(port, into=T)` constructs and validates — a `TypedDict` casts for free, a
dataclass or pydantic model raises on a payload that doesn't fit.

**It costs you** compile-time safety. A mismatch surfaces as a decode failure at the consumer while
running, not when you wire the graph.

</details>

<details id="gpu-without-the-vendor-lock">
<summary><b>GPU without the vendor lock</b> — and what that costs</summary>

<br>

Every GPU operation in the runtime goes through Vulkan. CUDA appears only at the edge, as the tensor
a frame hands to torch. On Linux, `libcuda`, the Vulkan loader and the window system are opened at
run time, never linked, so one build runs on any system that has them. On macOS the runtime carries
its own Vulkan loader and MoltenVK and links only the system. It also carries its own GLSL compiler,
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
| `DisplayWindow` | A vsync'd window; add as many as the stream needs. |
| `TestPatternSource` | Color bars, for a machine with no camera. |
| `MicrophoneSource`, `SpeakerSink` | The machine's audio backend, as timestamped sample blocks. |
| `H264Encoder`, `H264Decoder`, `H265Encoder`, `H265Decoder` | Hardware codecs: Vulkan Video on Linux, VideoToolbox on macOS. |
| `OpusEncoder`, `OpusDecoder` | libopus. |
| `Mp4Sink` | Encoded video and audio to one fragmented MP4, one track per inbound link. |
| `VirtualCameraSink` | Linux only: a camera any other application can select, through v4l2loopback or PipeWire. |

From Python you also get `GlslPixelEffect`, compute, graphics and ray-tracing kernels, a
model-input tensor kernel, node-owned windows, and the monotonic clock every node shares.

**Platforms.** Two floors, the same API on both:

- **Linux x86_64** with a Vulkan GPU and driver; NVIDIA is what the project tests on.
- **Apple Silicon, macOS 15 or newer**, on the Vulkan driver the runtime carries; CI runs a
  scaffolded stream there. Ray-tracing kernels, `VirtualCameraSink` and the fd-shaped raw handles
  refuse by name on macOS.

A stream's venv runs CPython 3.10 to 3.13, GIL-enabled builds; the scaffold pins 3.12.

Not built yet: an installer, serving an exposed port off the machine, and Windows.

## Troubleshooting

- **`error: no runtime is running on this machine: nothing answers at …`** — start `tatolabd` in a
  terminal of its own. When another user's `tatolabd` holds the machine, the refusal names it: it
  serves that user's socket, not yours.
- **`another runtime holds this machine: …`** — `tatolabd` is already running; the refusal names
  its user, pid and executable.
- **`the machine runtime lock cannot be taken: /Library/Application Support/Tatolab … does not
  exist`** (macOS) — Tatolab.app creates the lock at first launch; without it, run the `sudo`
  commands the refusal names, once.
- **``… has no .venv/bin/python; run `uv sync` in …``** — the runtime compiles a stream in the
  `.venv` of its project directory: the working directory, or `--dir`. Create it with `uv sync`.
- **``… cannot import `tatolab.stream`: add `tatolab-stream` to the project's dependencies``** —
  the project does not depend on `tatolab-stream` yet: `uv add tatolab-stream`.
- **A node defined in `stream.py` is refused** — move the class into a module beside `stream.py` and
  import it from there.
- **`tatolab dev: no stream is loaded — fix it and save again`** — the edit failed to compile
  with no earlier save running, or the earlier save could not be loaded again; the runtime's
  refusal, with its traceback, is printed just above.
- **`No usable Vulkan driver (ICD) was found` or `No Vulkan loader library could be opened`**
  (Linux) — install your GPU vendor's Vulkan driver (the proprietary NVIDIA driver, or
  `mesa-vulkan-drivers` for AMD and Intel) and the loader (`libvulkan1`), then check that
  `vulkaninfo` runs.
- **No camera, or the wrong one** — `tatolab new … --test-pattern` wires `TestPatternSource`
  instead. `CameraSource` takes the first capture device unless its `device_id` config names one (a
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
and link infrastructure — as your product.

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
