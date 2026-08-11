# StreamLib

**The perception and control loop your machine actually runs — written in Python, isolated by
the kernel, and observable while the machine is still moving.**

`0.17.1` · alpha, APIs will change · Linux + NVIDIA, x86_64 · BUSL-1.1, converting to Apache 2.0
on 2029-01-01

---

An inspection drone follows a transmission line. A picking arm reaches into a bin of parts it has
never seen. A ground vehicle runs a warehouse aisle at 3am with nobody in the building.

The work that fills your quarter is not writing the model. It is explaining why the new one
regressed when the old one didn't, on a rig you cannot reproduce at your desk. It is finding out
that the frames you trained on were never quite the frames the robot saw. It is having devices
deployed somewhere inconvenient and no way to ask them what they are doing right now. And it is
all of that under a power budget, on a network you cannot count on, in a place nobody wants to
drive to.

StreamLib is the runtime under that loop — the part you would otherwise spend a year building
before your model ever sees a real frame.

**Scope, plainly, before you read further.** StreamLib runs the loop on *one machine*: sensors in,
GPU work, your model, display or actuation out — plus remote observation of that machine while it
runs. Fleet orchestration, device-to-device transport, and over-the-air deployment are the
direction this is built toward; none of them exist today, and neither does any ROS integration.
[What ships today](#what-ships-today-and-what-does-not) names every gap.

## The data you capture is the data that ran

When a model regresses, the question is what changed in the input, and the honest answer is
usually that nobody knows. The offline pipeline drifted from the online one. The interesting
failure was never recorded. Recording it would have perturbed the thing you were trying to watch,
so you sampled less, so you caught less.

`streamlib tap` pulls the real payloads a live graph carried — verbatim, off a node running right
now, bounded, and without ever blocking the producer. What lands on your disk is what the machine
actually processed, not a reconstruction of what you believe it processed. A parked capture on a
moving vehicle cannot back-pressure the camera.

Timestamps are what make that capture worth training on. Every timestamp on the data path is the
machine's own monotonic clock — the same epoch the V4L2 and ALSA drivers already stamp their
buffers with, shared by every sensor, every process, and every stage on the host. A camera
driver's stamp, a frame three stages downstream in another process, and a reading your own code
takes are comparable by subtraction. Camera and IMU agree on when something happened because they
were never in different epochs, not because someone fitted an offset after the fact.

## A stage that wedges takes itself down, not the vehicle

A perception model hangs on a malformed frame. A vendor's driver leaks until it stalls. On a
Python stack that is a whole-system event, because every stage shares one interpreter — and the
symptom is never "the detector is stuck," it is "everything got slow," at 3am, on a machine in a
warehouse.

In StreamLib a failing stage fails alone. Every processor runs in its own OS process with its own
interpreter, on its own dedicated thread at a priority you declare — isolated by the kernel, not
by convention and everyone's good behavior. There is no in-process mode to fall back into: not a
default, not an optimization, not something that quietly kicks in under load. What you get
instead of a mystery is a fault with an address — scoped to a named stage, visible from outside
the machine, and recoverable without restarting the loop around it.

**It costs you** a process boundary on every link crossing into Python, and one authoring rule: a
stage's class lives in an importable module rather than in your entry file, because the child
process imports it by name. `rt.add` rejects the mistake with a message naming the fix.

## Your laptop, a device somewhere else, four commands

The device is in a field, a tunnel, a rack, an aisle. Normally that means logs after the fact,
assuming the logs caught the thing that went wrong, which they did not.

Every running StreamLib node hosts its own control surface, so a machine you cannot physically
reach is still a machine you can ask. Add `--url http://<host>:9000` to any of these and you are
debugging the rig instead of your desk:

```console
$ streamlib nodes
RUNTIME_ID                 CONTROL_URL            PID  ALIVE?  HINT
Rq1w8xk3m2v0pz7ny4tbd6hsf  http://0.0.0.0:9000  48212  yes     streamlib (/home/you/my-rig)
```

```bash
streamlib graph                              # its stages, links, states, metrics — as JSON
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

That is the deployed build, unmodified: no redeploy, no debugger attached, no code change to make
it observable. It already was. The same surface speaks MCP at `POST /mcp` on that same port —
mounted with the node, sharing its lifecycle, no bridge process — so an AI agent handed a
device's URL can do all of this itself:

```json
{"mcpServers": {"streamlib": {"type": "http", "url": "http://rig-04:9000/mcp"}}}
```

The tools are `graph`, `tap`, `logs`, and `shutdown`. Nothing on that surface mutates the graph:
the pipeline is defined by the code on the device, so what you read off a machine always matches
your source. The CLI above is a pure client of exactly this surface.

**It costs you** an unauthenticated port. A node binds all interfaces and does not authenticate
callers — narrow it with `--host` on any network you don't control.

## Start here

```bash
pip install streamlib --index-url https://tatolab.github.io/streamlib/simple/

streamlib new my-rig        # camera → effect → window, wired and working
cd my-rig
streamlib dev               # your camera, live, in a window
```

No camera on this machine? `streamlib new my-rig --test-pattern` uses the built-in test source.
Nothing is generated, compiled, or downloaded at run time — one wheel carries the Python API, the
CLI, and the engine. (That index is a static PEP 503 index served from this repo's releases; PyPI
publication is pending a project rename.)

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

## Your model gets device memory, not a copy

Frames do not cross into Python as pixels, and they do not round-trip the host bus to be looked
at. A stage resolves the frame it was handed and gives the memory straight to torch, on the
device:

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
use is an ordinary pip dependency in your venv, upgraded on your schedule. Putting device memory
in front of it is the engine's job; running it is not.

## Back-pressure is decided where the deadline is

A controller that must never miss a command and a display that should always show the newest frame
want opposite things from the same producer. Each input says which it is, and saying so is
required — there is no default to inherit by accident:

```python
@input(delivery_profile="latest")          # newest wins, stale samples dropped
@input(delivery_profile="every_sample")    # every sample in order, may fall behind
@input(delivery_profile="lossless")        # never dropped
```

What crosses a link is a self-describing named map. There is no schema registry, no negotiation,
no versions, no code-generation step, and nothing in the engine ever compares one stage's types
against another's. Strictness is a dial you turn at your own read: `ctx.inputs.read(port)` hands
you a mapping, and `read(port, into=T)` constructs and validates — a `TypedDict` casts for free, a
dataclass or pydantic model raises on a payload that doesn't fit.

**It costs you** compile-time safety. A mismatch surfaces as a decode failure at the consumer
while running, not when you wire the graph.

## Your compute supplier stays your choice

A stack welded to one vendor's compute API decides your supplier, your unit cost, and your thermal
envelope — exactly where you have the least slack. Every GPU operation in the engine goes through
Vulkan; CUDA appears only as an interop adapter, the thing that hands a tensor to torch. `libcuda`,
the Vulkan loader, and the window system are dlopen'd at run time, never linked.

**It costs you** any CUDA-specific fast path inside the engine, permanently — a vendor trick that
would help is expressed through Vulkan interop or not at all. And portability in the design is not
portability in practice: NVIDIA on Linux x86_64 is what CI tests. Other vendors are untested
rather than validated.

Extension stays inside package managers you already have. A Rust app is a plain cargo project
depending on the `streamlib` crate, released at the wheel's version. Third-party native code —
closed-source included — ships as an ordinary Python package that exposes handles and is wrapped
by a Python stage; it never links the engine, and the CPython ABI is the only binary boundary.
There is no plugin system, no ABI, no manifest, and no lockfile.

## Where this sits

StreamLib is not a ROS 2 replacement. ROS 2 is middleware and an ecosystem; StreamLib is the
compute substrate *inside* a node that has a deadline and a GPU. There is no ROS integration of
any kind today — no bridge, no message conversion. If you need one, it is a stage you would write.

## What ships today, and what does not

Alpha. The on-device loop, and remote observation of one node, are what exist. `CameraSource`
(V4L2), `DisplayWindow`, and `TestPatternSource` are native Rust stages compiled into the wheel,
configured from Python, whose per-frame paths never enter an interpreter. These do not exist:

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
