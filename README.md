# StreamLib

**A domain-agnostic, multimodal sensor processing platform — Python authoring, a Rust engine,
GPU acceleration without vendor lock-in.**

StreamLib runs applications that take live sensor data — cameras, microphones, lidar, radar,
anything that produces samples on a deadline — move it across the GPU, run your kernels and
models over it, and act on the result. You write one Python class per stage of the pipeline.
The engine schedules them, owns the GPU memory, and keeps pixels out of the interpreter.

It is built like a game engine: one core system per concern — graph, scheduler, GPU context,
media I/O, control plane — and ordinary Python on top of all of it.

```bash
pip install streamlib --index-url https://tatolab.github.io/streamlib/simple/
streamlib new my-app && cd my-app
streamlib dev            # camera → your effect → a window
```

## The bets

The category was defined by NVIDIA Holoscan, and on NVIDIA hardware Holoscan is the more
complete product today. StreamLib is aimed at the same problem with four different bets.

| | The bet |
|---|---|
| **Vendor-neutral GPU** | Every GPU operation goes through a Vulkan RHI. CUDA is an interop adapter for handing buffers to torch/cupy, never a requirement of the engine. NVIDIA on Linux is the tested floor today; nothing in the design is bound to it. |
| **No shared GIL, ever** | Every Python processor runs in its own child process, with its own interpreter and its own GIL. One slow processor cannot stall another. This is the reason the library exists — not a tuning option, and there is no in-process mode to fall back to. |
| **No media-stack inheritance** | No GStreamer, no FFmpeg, no libav, no DeepStream. Capture, present, and color are engine primitives written against V4L2, Vulkan, and the platform's own APIs. |
| **Operable by agents** | A running node hosts one control plane speaking HTTP, WebSocket, and **MCP**. The same verbs — `nodes`, `graph`, `tap`, `logs` — serve the CLI, your dashboards, and an AI agent pointed at the node's URL. |

## Writing a pipeline

An app is a normal Python codebase: an entry file, a `pyproject.toml`, one venv. No manifest,
no `main()`, no schema files, nothing StreamLib-specific on disk.

```python
# app.py — `streamlib dev` finds setup(rt) by convention
from processors.inverting_effect import InvertingEffect
from streamlib import CameraSource, DisplayWindow, Runtime


def setup(rt: Runtime) -> None:
    camera = rt.add(CameraSource)
    effect = rt.add(InvertingEffect)
    window = rt.add(DisplayWindow, config={"title": "StreamLib", "scaling": "fit"})

    rt.connect(camera.output("video"), effect.input("video_from_upstream"))
    rt.connect(effect.output("video_to_downstream"), window.input("video"))
```

A processor is a decorated class in an importable module. It declares ports — a name, a
description, and on an input the delivery profile that says which samples it wants — and gets a
capability-typed context in every lifecycle hook.

```python
# processors/inverting_effect.py
from streamlib import RuntimeContextLimitedAccess, VideoFrame, input, output, processor


@processor
class InvertingEffect:
    """Reads each frame, inverts its colors, and passes it on."""

    @input(delivery_profile="latest")
    def video_from_upstream(self) -> None: ...

    @output()
    def video_to_downstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("video_from_upstream")
        if bag is None:
            return
        frame = VideoFrame.from_bag(bag)
        with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
            surface.lock(read_only=False)
            pixels = surface.as_numpy()
            edited = pixels.copy()
            edited[:, :, :3] = 255 - edited[:, :, :3]
            pixels[...] = edited
            surface.unlock()
        ctx.outputs.write("video_to_downstream", bag)
```

That is the whole authoring surface. Re-run `streamlib dev` to see an edit; warm restart is
sub-second by construction.

## What the engine gives you

- **Frames as handles, not bytes.** A frame crosses into Python as a surface id. You resolve it
  and choose how to touch it: a mapped CPU view as numpy, a DMA-BUF export, or a DLPack capsule
  handed straight to torch or cupy on the device. Pixels never become Python objects, and
  nothing is copied behind your back.
- **Bring your own models.** StreamLib ships no inference stack and does not want one. Your
  model runtime is an ordinary pip dependency in your venv; the engine's job is to get device
  memory to it without a round trip through the host.
- **Per-port delivery policy.** Each input port declares `latest`, `every_sample`, or
  `lossless` — explicitly, with no default to guess wrong. Back-pressure and drop behavior are
  decided at the consumer, where the deadline actually lives.
- **Schema-free links.** A link carries a self-describing msgpack bag. The engine has no type
  layer to negotiate, no registry to keep in sync, and no version to bump; typing is your
  language's, and `read(port, into=T)` is the opt-in strictness dial when you want validation.
- **One clock.** Every data-plane timestamp is the machine's monotonic clock — the same epoch
  the V4L2 and ALSA driver stamps carry — so samples from different devices and different
  processes are directly comparable. Wall clock exists only on observability surfaces.
- **Native built-ins.** Camera, display, and a test-pattern source are Rust processors compiled
  into the wheel. Their per-frame paths never enter an interpreter, and they are written against
  the same handle-shaped primitives third-party code gets.

## Observing a running node

Any node started by `streamlib run` or `streamlib dev` registers itself and can be inspected
live, from another terminal or another machine:

```bash
streamlib nodes                               # what is running on this box
streamlib graph                               # processors, ports, links, states, metrics — JSON
streamlib tap CameraSource/video --count 10   # a bounded sample of real bags off a link
streamlib logs <runtime_id> --follow          # JSONL logs, filterable by processor and level
```

The CLI is a thin client of the node's control plane. Point an MCP host at the same URL and an
agent gets the identical vocabulary — inspection only: code is the source of truth, and the edit
loop is `dev`, not live graph mutation.

## Rust authoring

The engine is a Rust library and stays one. A Rust app is a plain cargo project depending on the
`streamlib` crate — no wrapper generation, no manifest, no special build. Third-party Rust
processors are ordinary cargo dependencies, compiled from source. The wheel and the crate are
one version, released together.

## Platform support

| Platform | Status |
|---|---|
| Linux x86_64, NVIDIA GPU | ✅ The supported floor — CI-tested, wheels published |
| Linux, other Vulkan GPUs | 🚧 Nothing in the design excludes them; not yet exercised |
| Linux aarch64 (Jetson-class) | 📋 Planned — no wheel published yet |
| macOS | 🚧 Engine paths are cross-compiled; capture (AVFoundation) is undesigned |
| Windows | 📋 Planned |

Python 3.10+ (abi3, GIL-enabled builds). The wheel dlopens the Vulkan loader, the window system,
and libcuda at runtime rather than linking them, so it stays manylinux-portable.

## Status

Alpha. The APIs on this page work today and will still change.

Honest gaps, so you can judge the fit:

- **Audio has no backend yet.** The clock primitive is settled; PipeWire-vs-ALSA is an open
  decision, so there is no audio built-in in the wheel.
- **GPU kernels are Rust-side for now.** Compute, graphics, and ray-tracing kernels exist in the
  engine; the Python surface that lets you pass GLSL and dispatch it is in flight.
- **DMA-BUF export works from Python; import does not.** A foreign fd cannot yet be handed to a
  processor's graph.
- **Networking between nodes is undesigned.** Cross-language, cross-machine interop is decided
  to happen on the wire as bags — the transport itself is not chosen.

## Project structure

```
streamlib/
├── runtime/     # the engine: streamlib-engine, media-builtins, api-server, consumer-rhi,
│                #   ipc-types, surface-client, moq
├── sdk/         # what authors compile against: the Python wheel (API + CLI + engine),
│                #   the streamlib crate, macros, error types
├── adapters/    # GPU interop: vulkan, cuda, opengl, cpu-readback
├── vendor/      # vendored third-party forks (Apache-2.0, never edited in place)
├── docs/        # plan, decisions, learnings, architecture
└── examples/    # example apps (standalone, not workspace members)
```

Build and test:

```bash
cargo build --workspace
cargo test -p streamlib-engine
cd sdk/streamlib-python-wheel && maturin develop && pytest
```

## License

StreamLib is licensed under the [Business Source License 1.1](LICENSE), and converts to
[Apache 2.0](LICENSES/Apache-2.0.txt) on **January 1, 2029**.

The split, in one line: **what you build is yours; reselling the runtime itself needs a
license.**

| | |
|---|---|
| Build processors, apps, and products on StreamLib — commercial, private, or open source | **Free** |
| Sell processors you wrote; keep their source closed | **Free**, you keep 100% |
| Personal, educational, and research use | **Free** |
| Sell, host, white-label, or sublicense **the runtime** as the product | Commercial license |

"The runtime" means the engine — graph compiler, scheduler, processor execution, GPU context,
link infrastructure. It does not mean the processors you write against the authoring API.

[Commercial licensing](docs/license/COMMERCIAL-LICENSING.md) ·
[Partner licensing](docs/license/PARTNER-LICENSING.md) ·
[Contributor agreement](docs/license/CLA.md)

## Contributing

Pull requests are welcome and are licensed under the same BUSL-1.1 terms; see
[CLA.md](docs/license/CLA.md).

Work is tracked as a dependency graph over GitHub issues, driven with
[`amos`](https://github.com/tatolab/amos):

```bash
amos focus "<milestone>"   # scope to a milestone
amos next                  # what is ready to start
amos blocked               # what is gated, and by what
```

## Contact

Jonathan Fontanez · fontanezj1@gmail.com · <https://github.com/tatolab/streamlib>
