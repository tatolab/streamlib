<div align="center">

<img src="docs/assets/streamlib-logo.svg" alt="StreamLib" width="520">

**One perception and control runtime that runs on the hardware itself** — an embedded board, a drone,<br>
a robot already running ROS, or the laptop you develop on. Write each stage in Python; a Rust engine runs it on the device.

For teams shipping physical AI: humanoids, autonomous vehicles, self-piloting drones,<br>
and the data-collection rigs that train them.

[![release](https://img.shields.io/github/v/release/tatolab/streamlib?color=0ea5e9&label=release)](https://github.com/tatolab/streamlib/releases)
[![website](https://img.shields.io/badge/tatolab.com-0ea5e9?label=website)](https://tatolab.com)
[![license](https://img.shields.io/badge/license-BUSL--1.1-0ea5e9)](LICENSE)
[![python](https://img.shields.io/badge/python-3.10%20%E2%80%93%203.13-0ea5e9)](#install)
[![platform](https://img.shields.io/badge/platform-linux%20x86__64-64748b)](#what-ships-today)
[![gpu](https://img.shields.io/badge/GPU-Vulkan-64748b)](#gpu-without-the-vendor-lock)
[![tests](https://github.com/tatolab/streamlib/actions/workflows/test.yml/badge.svg)](https://github.com/tatolab/streamlib/actions/workflows/test.yml)

[Install](#install) · [Quickstart](#quickstart) · [Inspect a live device](#inspect-a-device-thats-already-running) · [How it works](#how-it-works) · [What ships today](#what-ships-today) · [License](#license) · [tatolab.com](https://tatolab.com)

</div>

<!-- Demo GIF slot. Generate on the rig with `vhs docs/assets/demo.tape`, commit the
     result, then replace this comment with:
     <div align="center"><img src="docs/assets/demo.gif" alt="Inspecting a running node" width="900"></div> -->

---

- **Real-time processing on commodity hardware.** Deadline-driven stages on dedicated OS threads at
  a priority you declare — an off-the-shelf GPU and a Linux box, not a proprietary accelerator or a
  vendor runtime you have to buy into.
- **GPU acceleration for video, built in.** Capture lands in device memory and stays there — imported
  zero-copy where the device exports it, transparently uploaded where it doesn't, with no
  configuration dial in between. Your model gets the frame where it already sits.
- **Open, and extendable to hardware nobody has heard of.** Any sensor is a stage you write, and a
  proprietary driver ships as an ordinary Python package. Optional capabilities — networking
  first — ship the same way, as extension wheels with Rust inside: pip installs the wheel, the
  engine discovers its support code, and your app adds its processors with `rt.add` like any
  other.
  No plugin ABI, no framework headers, no vendor allowlist deciding what you're allowed to
  plug in.
- **The execution graph is code, not a config file.** You compose it in Python at startup, so it can
  branch on the sensors actually present, the mission profile, or the tier of hardware it booted on
  — the same source deployed across a heterogeneous fleet.
- **AI-first, not AI-bolted-on.** A control policy is a stage like any other: a VLA or world model
  receives frames as device memory and emits actions on the same clock as the sensors that fed it.
  The control plane speaks MCP, so an agent can inspect a running machine on its own.
- **Built to survive the field.** A stage that wedges takes down one stage and names itself; every
  sensor shares one clock; and a deployed device stays inspectable from your laptop without a
  redeploy.

> **Alpha.** APIs will change. There is no fleet orchestration, device-to-device transport, OTA
> deployment, ROS integration, aarch64/Jetson wheel, or control-plane authentication —
> see [what ships today](#what-ships-today).

## Built for

- **Humanoid and manipulation teams** collecting demonstration data for VLA training, where the
  value of an episode depends entirely on camera, proprioception, and action sharing one timeline.
- **Drone and autonomous-vehicle stacks** running a deadline-bound perception loop on the vehicle,
  across cameras, lidar, radar, and IMU that all arrive at different rates.
- **On-device policy inference** — a VLA or world model, Cosmos- or GR00T-class, that needs the
  frame as device memory rather than as a numpy array that already cost you two copies.
- **Data-collection rigs** where time-aligned multi-sensor capture *is* the product, not a
  supporting detail.
- **Eval on the machine that will run it** — proving a checkpoint against what the robot actually
  sees, instead of a replay pipeline that has quietly drifted from the online one.

## What it doesn't replace

StreamLib is the substrate for the loop on the device. It is deliberately narrow, and it composes
with what you already run rather than asking you to move.

| You keep | StreamLib's part |
|---|---|
| **Your model runtime** — torch, TensorRT, ONNX Runtime | Hands it device memory and gets out of the way. No inference stack ships, and none is planned. |
| **Your middleware** — ROS 2 and its ecosystem | Runs on the same box. StreamLib is the compute inside a node with a deadline and a GPU, not a bus or a package ecosystem. |
| **Your accelerated pipelines** — Holoscan, DeepStream, vendor SDKs | Nothing stops them coexisting on the machine; StreamLib does not want the whole box. |
| **Your training and cloud stack** | StreamLib is the on-device half — it produces the aligned data your training consumes. |
| **Your sensors and drivers** | Anything with a Python binding, a file descriptor, or an exportable allocation composes as a stage you write. |

**Honest limit on all of that:** composing today means *on the same machine, through Python*. There
are no bridges shipping — no ROS node, no Holoscan operator, no device-to-device transport. Where
you need one, it is a stage you write against the driver or binding you already have.

## Install

```bash
pip install streamlib --index-url https://tatolab.github.io/streamlib/simple/
```

A static PEP 503 index served from this repo's releases — PyPI publication is pending a project
rename, and the artifact is identical either way. One wheel carries the Python API, the CLI, and
the engine; nothing is generated, compiled, or downloaded at run time.

## Quickstart

```bash
streamlib new my-rig        # camera → effect → window, wired and working
cd my-rig
streamlib dev               # your camera, live, in a window
```

No camera on this machine? `streamlib new my-rig --test-pattern` uses the built-in test source.

`app.py` is wiring and nothing else — no manifest, no `main()`, no registration file. `dev` imports
it from the working directory and calls `setup(rt)`:

```python
from processors.inverting_effect import InvertingEffect
from streamlib import CameraSource, DisplayWindow, Runtime


def setup(rt: Runtime) -> None:
    source = rt.add(CameraSource)
    effect = rt.add(InvertingEffect)
    window = rt.add(DisplayWindow, config={"title": "StreamLib", "scaling": "fit"})

    rt.connect(source.output("video"), effect.input("video_from_upstream"))
    rt.connect(effect.output("video_to_downstream"), window.input("video"))
```

`processors/inverting_effect.py` is the stage — the file you replace with your model:

```python
from streamlib import RuntimeContextLimitedAccess, VideoFrame, input, output, processor


@processor
class InvertingEffect:
    @input(delivery_profile="newest")
    def video_from_upstream(self) -> None: ...

    @output()
    def video_to_downstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("video_from_upstream")
        if bag is None:
            return
        frame = VideoFrame.from_bag(bag)
        # The frame arrives as a handle, not pixels: resolve it and open
        # CPU access to the engine's own memory.
        with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
            surface.lock(read_only=False)
            pixels = surface.as_numpy()
            edited = pixels.copy()
            edited[:, :, :3] = 255 - edited[:, :, :3]
            pixels[...] = edited
            surface.unlock()
        ctx.outputs.write("video_to_downstream", bag)
```

Edit a stage, re-run `dev`. Each stage runs `reactive` (the default once it has an input),
`manual`, or `continuous` at an interval you set.

## Inspect a device that's already running

Add `--url http://<host>:9000` to any of these and you're debugging the rig instead of your desk.

```console
$ streamlib nodes
RUNTIME_ID                 CONTROL_URL            PID  ALIVE?  HINT
Rq1w8xk3m2v0pz7ny4tbd6hsf  http://0.0.0.0:9000  48212  yes     streamlib (/home/you/my-rig)

$ streamlib tap CameraSource/video --count 3
{"channel": "CameraSource/video", "requested": 3, "window_ms": 500, "dropped_bags": 0,
 "bags": [{"byte_len": 214, "hex_preview": "84aa73...", "hex_truncated": false}, ...]}
```

`graph` dumps stages, links, states and metrics as JSON; `logs` streams structured records
filtered by stage and severity. `tap` returns what the link really carried, bounded and
non-blocking — a quiet link gives a partial sample instead of hanging. That's the deployed build,
unmodified, and it's also how an eval or training set comes off the machine that produced it
rather than off an offline pipeline that has already drifted from it.

<details>
<summary><b>The same surface speaks MCP, so an agent can do this itself</b></summary>

<br>

```json
{"mcpServers": {"streamlib": {"type": "http", "url": "http://rig-04:9000/mcp"}}}
```

Served at `POST /mcp`, mounted with the node and sharing its lifecycle — there is no bridge
process to run. The tools are `graph`, `tap`, `logs`, and `shutdown`. Nothing on that surface
mutates the graph: the pipeline is defined by the code on the device, so what you read off a
machine always matches your source. The CLI is a pure client of exactly this surface.

**It costs you** an unauthenticated port. A node binds all interfaces and does not authenticate
callers — narrow it with `--host` on any network you don't control.

</details>

## How it works

<details>
<summary><b>Stage isolation</b> — why a wedged model can't take the machine down</summary>

<br>

Every processor runs in its own OS process with its own interpreter, on its own dedicated thread
at a priority you declare. A model that deadlocks on a malformed frame, a C extension that
segfaults, a vendor driver that leaks — each takes down one stage and becomes a named, restartable
event instead of a whole-system slowdown with no address. The boundary is enforced by the kernel,
not by convention. There is no in-process mode: not a default, not a fallback, not something that
kicks in under load.

**It costs you** a process boundary on every link crossing into Python, and one authoring rule: a
stage's class lives in an importable module rather than in your entry file, because the child
process imports it by name. `rt.add` rejects the mistake with a message naming the fix.

</details>

<details>
<summary><b>Any sensor, not just cameras</b> — the extension model</summary>

<br>

Nothing in the engine is specific to video. A source is a stage that produces without consuming —
running `continuous` at an interval, or `manual` when driven by a callback it owns. Lidar, radar,
thermal, encoders, a CAN bus, a proprietary SDK with a Python binding: if you can read it, it's a
source you write, and it gets the same isolation, the same clock, and the same observability as
everything that ships.

Native code comes in the same door. A third-party driver — closed-source included — ships as an
ordinary Python package that exposes handles (file descriptors, exportable allocations, buffers)
and is wrapped by a stage you write. It never links the engine, and the CPython ABI is the only
binary boundary. First-party optional capabilities take that same door — an extension wheel is
an ordinary PyPI package with Rust inside, depending on `streamlib` as a binary. Its processors
are added with `rt.add` like any other and call the wheel's own Rust directly; its support code
is declared by a standard entry point that pip records and the engine runs once at startup, the
way a driver is loaded. There is no plugin ABI, no StreamLib manifest and no StreamLib
lockfile — an extension wheel is an ordinary Python project with an ordinary `pyproject.toml`.

**It costs you** a small set of built-ins. Camera, display, test pattern, microphone, speaker,
the H.264 / H.265 / Opus codec pairs and an MP4 sink ship inside the wheel because their
per-frame paths have deadlines a helper process cannot meet or sit on engine-only primitives,
and each had a consumer that asked for it; everything else is an extension wheel or a stage
you write.

</details>

<details>
<summary><b>Device memory, not a copy</b> — handing frames to torch</summary>

<br>

```python
with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
    surface.lock(read_only=False)
    tensor = torch.from_dlpack(surface)          # H×W×4 uint8, on the CUDA device
    tensor[:, :, :3] = 255 - tensor[:, :, :3]    # runs on the GPU
    torch.cuda.synchronize()
    surface.unlock()                             # publishes the device-side write
```

Other doors on the same handle: `surface.as_numpy()` for a mapped host view, and
`numpy.from_dlpack(surface, device="cpu")` for that memory as a capsule. CUDA Array Interface is
available through the cuda adapter, and lifetimes are engine-owned — a tensor pins its frame.

For the handle itself, `ctx.gpu_full_access.export_dma_buf(surface)` hands native code a DMA-BUF
fd, and `ctx.gpu_full_access.export_opaque_fd(surface)` the OPAQUE_FD flavour (HDR kernel
outputs) with the metadata a foreign Vulkan/CUDA import needs. A raw handle names the allocation,
never the frame: take it once at setup on a surface you own — per-frame reach stays with surface
ids and the tensor doors above.

Stated honestly, this is zero-**CPU**-copy, not copy-free: a tiled engine texture reaches a linear
tensor through one GPU blit into an exportable staging buffer, because DLPack expresses strided
linear memory only.

No inference stack ships and none is planned. torch, ONNX Runtime, TensorRT — whatever you already
use is an ordinary pip dependency in your venv, upgraded on your schedule.

</details>

<details>
<summary><b>The read policy is decided at the consumer</b> — delivery profiles and payloads</summary>

<br>

A logger that wants its bags in the order they were sent and a display that should always show the
newest frame want opposite things. Each input says which it is, and saying so is required — there
is no default to inherit by accident:

```python
@input(delivery_profile="newest")     # drains to the most recent bag, older ones passed over
@input(delivery_profile="ordered")    # bags in publication order, may fall behind
```

A profile names a read policy and nothing more. Neither promises delivery: both drop under
sustained pressure, and no link ever blocks a producer. One output port carries one policy — every
consumer wired to it declares the same profile, and consumers that want different ones are fanned
out through distinct output ports.

What crosses a link is a self-describing named map. No schema registry, no negotiation, no
versions, no code-generation step, and nothing in the engine ever compares one stage's types
against another's. Strictness is a dial you turn at your own read: `ctx.inputs.read(port)` hands
you a mapping, and `read(port, into=T)` constructs and validates — a `TypedDict` casts for free, a
dataclass or pydantic model raises on a payload that doesn't fit.

**It costs you** compile-time safety. A mismatch surfaces as a decode failure at the consumer
while running, not when you wire the graph.

</details>

<details id="gpu-without-the-vendor-lock">
<summary><b>GPU without the vendor lock</b> — and what that costs</summary>

<br>

Every GPU operation in the engine goes through Vulkan. CUDA appears only as an interop adapter —
the thing that hands a tensor to torch. `libcuda`, the Vulkan loader, and the window system are
dlopen'd at run time, never linked, so the wheel stays portable across systems that have them.

**It costs you** any CUDA-specific fast path inside the engine, permanently — a vendor trick that
would help is expressed through Vulkan interop or not at all. And portability in the design is not
portability in practice: NVIDIA on Linux x86_64 is what CI tests. Other vendors are untested
rather than validated.

Rust authoring is first-class: a plain cargo project depending on the `streamlib` crate, released
at the wheel's version. PyPI and cargo are the package systems — a Rust app compiles an extension
from source, a Python app pip-installs its wheel.

</details>

## What ships today

The on-device loop — sensors in, GPU work, your model, actuation or display out — plus remote
observation of that device. `CameraSource` (V4L2), `DisplayWindow`, and `TestPatternSource` are
native Rust stages compiled into the wheel, configured from Python, whose per-frame paths never
enter an interpreter.

These do not exist yet:

| | |
|---|---|
| **Fleet & networking** | No device-to-device transport, no orchestration, no OTA. Undesigned. The one decision made: cross-machine interop happens on the wire, never in-graph. |
| **ROS** | No integration of any kind. |
| **Jetson / aarch64** | No wheel published. x86_64 only today. |
| **Control-plane auth** | Undesigned. A node binds all interfaces and does not authenticate callers. |
| **Audio** | No backend ships. The clock primitive is settled; PipeWire-vs-ALSA is an open decision. |
| **GPU kernels from Python** | Compute, graphics, ray tracing, and acceleration structures exist Rust-side. The Python kernel API is in flight, not shipped. |
| **DMA-BUF import** | Export from Python works; importing a foreign fd into a graph does not yet. |

**Platform floor.** Linux + NVIDIA, which is what CI tests: abi3 wheel, CPython 3.10+, GIL-enabled
builds, manylinux_2_28, x86_64, V4L2 the only capture backend. The wheel carries its own GLSL
compiler, so there is no system toolchain to install. macOS engine paths cross-compile but Apple
capture is undesigned; Windows is unbuilt.

## License

StreamLib is [BUSL-1.1](LICENSE), converting automatically to
[Apache 2.0](LICENSES/Apache-2.0.txt) on **January 1, 2029**.

**What you build is yours; reselling the runtime itself needs a license.**

Free, no permission needed: building stages, applications, robots, and products on StreamLib —
commercial, private, or open source; selling stages you wrote with their source closed; personal,
educational, and research use.

A commercial license is required to sell, host as a managed service, white-label, or sublicense
**the runtime itself** — the engine, graph compiler, scheduler, processor execution, GPU context,
and link infrastructure — as your product.

StreamLib also distributes third-party code. Each dependency's copyright notice and licence
text, as of that file's last regeneration, is reproduced in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md), which the wheel ships in its
`.dist-info/licenses/`.

[Commercial licensing](docs/license/COMMERCIAL-LICENSING.md) ·
[Partner licensing](docs/license/PARTNER-LICENSING.md) · [CLA](docs/license/CLA.md)

---

<div align="center">

Built by [Tatolab](https://tatolab.com) — sensory infrastructure for AI.

[tatolab.com](https://tatolab.com) · [hello@tatolab.com](mailto:hello@tatolab.com)

</div>
