# StreamLib

**One perception and control runtime that runs on the hardware itself** — an embedded board, a
drone, a robot already running ROS, or the laptop you develop on. You write each stage of the loop
as a Python class; a Rust engine runs it on the device, schedules it, and owns the GPU memory.

`0.17.1` · alpha, APIs will change · Linux x86_64 wheel · BUSL-1.1, converting to Apache 2.0 on
2029-01-01

- **Every stage is its own OS process** — a model that wedges takes down one stage, not the machine.
- **Frames reach your model as device memory** — DLPack straight to torch, a DMA-BUF fd, or a
  mapped numpy view. Pixels never round-trip the host bus to be looked at.
- **One clock across every sensor** — the same monotonic epoch V4L2 and ALSA stamp their own
  buffers with, so multi-sensor data lines up by subtraction.
- **Capture what actually ran** — tap a live link for the real payloads it carried, without
  blocking or perturbing the producer.
- **Inspect a deployed device from your laptop** — every node serves HTTP, WebSocket, and MCP. No
  redeploy, no debugger, no code change.
- **Any sensor** — a source is a Python class you write; nothing in the engine is video-specific.
- **No CUDA dependency** — all GPU work goes through Vulkan, so your compute supplier stays a
  decision you can revisit.

**Not yet:** fleet orchestration, device-to-device transport, over-the-air deployment, ROS
integration, aarch64/Jetson wheels, or control-plane authentication. See
[what ships today](#what-ships-today-and-what-does-not).

## Install

```bash
pip install streamlib --index-url https://tatolab.github.io/streamlib/simple/

streamlib new my-rig        # camera → effect → window, wired and working
cd my-rig
streamlib dev               # your camera, live, in a window
```

No camera on this machine? `streamlib new my-rig --test-pattern` uses the built-in test source.
Nothing is generated, compiled, or downloaded at run time — one wheel carries the Python API, the
CLI, and the engine. (A static PEP 503 index served from this repo's releases; PyPI publication is
pending a project rename.)

## The loop you write

`streamlib new` writes exactly this. `app.py` is wiring and nothing else:

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
    @input(delivery_profile="latest")
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

No manifest, no `main()`, no registration file. `dev` imports `app.py` from the working directory
and calls `setup(rt)`; edit a stage and re-run `dev`. Each stage runs `reactive` (the default once
it has an input), `manual`, or `continuous` at an interval you set.

## Stage isolation

Every processor runs in its own OS process with its own interpreter, on its own dedicated thread
at a priority you declare. A model that deadlocks on a malformed frame, a C extension that
segfaults, a vendor driver that leaks — each takes down one stage and becomes a named,
restartable event instead of a whole-system slowdown with no address. The boundary is enforced by
the kernel, not by convention. There is no in-process mode: not a default, not a fallback, not
something that kicks in under load.

**It costs you** a process boundary on every link crossing into Python, and one authoring rule: a
stage's class lives in an importable module rather than in your entry file, because the child
process imports it by name. `rt.add` rejects the mistake with a message naming the fix.

## Any sensor, not just cameras

Nothing in the engine is specific to video. A source is a stage that produces without consuming —
running `continuous` at an interval, or `manual` when driven by a callback it owns. Lidar, radar,
thermal, encoders, a CAN bus, a proprietary SDK with a Python binding: if you can read it, it is a
source you write, and it gets the same isolation, the same clock, and the same observability as
everything that ships.

Native code comes in the same door. A third-party driver — closed-source included — ships as an
ordinary Python package that exposes handles (file descriptors, exportable allocations, buffers)
and is wrapped by a stage you write. It never links the engine, and the CPython ABI is the only
binary boundary. There is no plugin system, no ABI, no manifest, and no lockfile.

**It costs you** the built-ins you don't get. V4L2 capture, a display window, and a test pattern
are what ship native; every other sensor is yours to wrap.

## Your model gets device memory, not a copy

```python
with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
    surface.lock(read_only=False)
    tensor = torch.from_dlpack(surface)          # H×W×4 uint8, on the CUDA device
    tensor[:, :, :3] = 255 - tensor[:, :, :3]    # runs on the GPU
    torch.cuda.synchronize()
    surface.unlock()                             # publishes the device-side write
```

Other doors on the same handle: `surface.as_numpy()` for a mapped host view,
`numpy.from_dlpack(surface, device="cpu")` for that memory as a capsule, `export_dma_buf` for a
file descriptor to hand to something else. CUDA Array Interface is available through the cuda
adapter, and lifetimes are engine-owned — a tensor pins its frame.

Stated honestly, this is zero-**CPU**-copy, not copy-free: a tiled engine texture reaches a linear
tensor through one GPU blit into an exportable staging buffer, because DLPack expresses strided
linear memory only.

No inference stack ships and none is planned. torch, ONNX Runtime, TensorRT — whatever you already
use is an ordinary pip dependency in your venv, upgraded on your schedule.

## Inspecting a device that is already running

Add `--url http://<host>:9000` to any of these and you are debugging the rig instead of your desk.

```console
$ streamlib nodes
RUNTIME_ID                 CONTROL_URL            PID  ALIVE?  HINT
Rq1w8xk3m2v0pz7ny4tbd6hsf  http://0.0.0.0:9000  48212  yes     streamlib (/home/you/my-rig)
```

```bash
streamlib graph                              # stages, links, states, metrics — as JSON
streamlib tap CameraSource/video --count 5   # what's actually flowing, verbatim
streamlib logs --follow --level warn         # structured, per-stage, per-severity
```

`tap` hands back what the link really carried, bounded and non-blocking — a quiet link returns a
partial sample instead of hanging:

```console
$ streamlib tap CameraSource/video --count 3
{"channel": "CameraSource/video", "requested": 3, "window_ms": 500, "dropped_bags": 0,
 "bags": [{"byte_len": 214, "hex_preview": "84aa73...", "hex_truncated": false}, ...]}
```

That is the deployed build, unmodified — it was already observable. This is also how an eval or
training set comes off the machine that produced it, rather than off an offline pipeline that has
already drifted from it.

The same surface speaks MCP at `POST /mcp` on that port — mounted with the node, sharing its
lifecycle, no bridge process — so an agent handed a device's URL can do all of this itself:

```json
{"mcpServers": {"streamlib": {"type": "http", "url": "http://rig-04:9000/mcp"}}}
```

The tools are `graph`, `tap`, `logs`, and `shutdown`. Nothing on that surface mutates the graph:
the pipeline is defined by the code on the device, so what you read off a machine always matches
your source. The CLI is a pure client of exactly this surface.

**It costs you** an unauthenticated port. A node binds all interfaces and does not authenticate
callers — narrow it with `--host` on any network you don't control.

## Back-pressure is decided at the consumer

A controller that must never miss a command and a display that should always show the newest frame
want opposite things from the same producer. Each input says which it is, and saying so is
required — there is no default to inherit by accident:

```python
@input(delivery_profile="latest")          # newest wins, stale samples dropped
@input(delivery_profile="every_sample")    # every sample in order, may fall behind
@input(delivery_profile="lossless")        # never dropped
```

What crosses a link is a self-describing named map. No schema registry, no negotiation, no
versions, no code-generation step, and nothing in the engine ever compares one stage's types
against another's. Strictness is a dial you turn at your own read: `ctx.inputs.read(port)` hands
you a mapping, and `read(port, into=T)` constructs and validates — a `TypedDict` casts for free, a
dataclass or pydantic model raises on a payload that doesn't fit.

**It costs you** compile-time safety. A mismatch surfaces as a decode failure at the consumer
while running, not when you wire the graph.

## GPU without the vendor lock

Every GPU operation in the engine goes through Vulkan. CUDA appears only as an interop adapter —
the thing that hands a tensor to torch. `libcuda`, the Vulkan loader, and the window system are
dlopen'd at run time, never linked, so the wheel stays portable across systems that have them.

**It costs you** any CUDA-specific fast path inside the engine, permanently — a vendor trick that
would help is expressed through Vulkan interop or not at all. And portability in the design is not
portability in practice: NVIDIA on Linux x86_64 is what CI tests. Other vendors are untested
rather than validated.

Rust authoring is first-class: a plain cargo project depending on the `streamlib` crate, released
at the wheel's version. PyPI and cargo are the package systems.

## Where this sits

StreamLib is not a ROS 2 replacement. ROS 2 is middleware and an ecosystem; StreamLib is the
compute substrate *inside* a node that has a deadline and a GPU, running on the same box as the
rest of your stack. There is no ROS integration of any kind today — no bridge, no message
conversion. If you need one, it is a stage you would write.

## What ships today, and what does not

Alpha. What exists is the on-device loop — sensors in, GPU work, your model, actuation or display
out — plus remote observation of that device. `CameraSource` (V4L2), `DisplayWindow`, and
`TestPatternSource` are native Rust stages compiled into the wheel, configured from Python, whose
per-frame paths never enter an interpreter.

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

The short version: **what you build is yours; reselling the runtime itself needs a license.**

Free, no permission needed: building stages, applications, robots, and products on StreamLib —
commercial, private, or open source; selling stages you wrote with their source closed; personal,
educational, and research use.

A commercial license is required to sell, host as a managed service, white-label, or sublicense
**the runtime itself** — the engine, graph compiler, scheduler, processor execution, GPU context,
and link infrastructure — as your product.

[Commercial licensing](docs/license/COMMERCIAL-LICENSING.md) ·
[Partner licensing](docs/license/PARTNER-LICENSING.md) · [CLA](docs/license/CLA.md)

## Contact

Jonathan Fontanez — fontanezj1@gmail.com — <https://github.com/tatolab/streamlib>
