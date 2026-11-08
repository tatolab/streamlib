# camera-virtual-camera

One capture device in, **two cameras out**. While this stream runs, Chrome, OBS,
Zoom and `v4l2-ctl` all see two extra cameras on the machine — "Tatolab Camera"
showing the capture device untouched, and "Tatolab Camera Inverted" showing the
same picture after a Python node has inverted its colours. Stop the stream and
both are gone.

## The model this example teaches

**A camera is a node.** `VirtualCameraSink`, from the `tatolab-virtualcamera`
module, takes video
on an input port and has no output port, because its output is a camera other
applications open. Each instance is one camera that exists exactly as long as
its node runs — created at `setup()`, removed at `teardown()` — so from every
other application's point of view a stream starting is a USB camera being
plugged in, and Ctrl-C is it being pulled back out.

**As many as the stream adds.** Two cameras is two sink nodes in
`streams/main.graph.json` and one more link; there is no registry, no device number to reserve, and no limit in
the module:

```json
{
  "nodes": [
    {"name": "camerasource", "type": "tatolab.camera:CameraSource", "config": {}},
    {"name": "invertingeffect", "type": "nodes.inverting_effect:InvertingEffect", "config": {}},
    {"name": "passthrough-virtual-camera", "type": "tatolab.virtualcamera:VirtualCameraSink",
     "config": {"name": "Tatolab Camera", "door": "auto"}},
    {"name": "inverted-virtual-camera", "type": "tatolab.virtualcamera:VirtualCameraSink",
     "config": {"name": "Tatolab Camera Inverted", "door": "auto"}}
  ],
  "links": [
    {"source": {"node": "camerasource", "port": "video"}, "target": {"node": "passthrough-virtual-camera", "port": "video"}},
    {"source": {"node": "camerasource", "port": "video"}, "target": {"node": "invertingeffect", "port": "video_from_upstream"}},
    {"source": {"node": "invertingeffect", "port": "video_to_downstream"}, "target": {"node": "inverted-virtual-camera", "port": "video"}}
  ]
}
```

The file itself names each node and each link end by the node's UUID; the
names stand in for them here.

The camera's one output port feeds two consumers. That is an ordinary fan-out —
the passthrough sink and the effect each get every frame — and it is why this
stream is two cameras rather than the same camera twice. Two names are at work:
`name=` is the node's name in the stream's graph, the one `graph`, `connect` and
MCP take; the config's `name` is the camera's name in every picker.

**One door per instance, chosen for you.** A virtual camera has to be a device
the rest of the system already knows how to open, and Linux has two such doors:

- the **v4l2loopback** door, where the sink creates its own `/dev/videoN`
  through the module's control node and removes it at teardown. This is the
  door *every* application sees — `/dev/video*` readers directly, and
  portal-based readers through the session manager's V4L2 mirror.
- the **PipeWire** door, a camera node in the PipeWire graph. It needs no
  kernel module and no root, and portal-based applications list it;
  applications that only enumerate `/dev/video*` do not.

The sink picks at `setup()` and logs which and why. Under the default
`door = "auto"` it takes the loopback door whenever the module's control node
is writable by your user, and the PipeWire door otherwise — so a fresh install
with no setup at all still produces a camera. Never both for one instance: the
session manager mirrors V4L2 devices into the portal's camera set, so a sink on
both doors would list the same camera twice.

**The effect publishes its own textures.** `nodes/inverting_effect.py` reads
the camera's pixels through a read-only `numpy.from_dlpack(frame, device="cpu")`
view of the `ImageFrame` its `Input(Image)` port hands it, and writes their inversion straight into a slot of its own
`NodeOutputTextureRing`, in one pass on the host. It never edits the frame
it was handed: the passthrough sink is reading that same engine-owned surface.
Whenever a producer's output feeds more than one consumer, an effect between
them publishes from textures it owns. The ring hands out a fresh slot per
frame, and a slot a consumer still holds is skipped, never rewritten.

The two sinks' per-frame paths never enter a Python interpreter — they are
native built-ins, and the RGBA→YUYV conversion on the loopback door is one GPU
pass straight into the device's buffers. The effect runs in its own child
interpreter, which is where the per-frame Python cost in this stream begins and
ends: each frame goes out to the host and back, so at 1080p the inverted camera
writes fewer frames than the passthrough one. A node in its own interpreter
can never slow another, so the passthrough camera keeps the capture's rate
whatever the effect costs. An effect that stays on the GPU does not pay that —
`ShaderEffect`, below, is one.

## One host, many looks

`nodes/shader_effect.py` is an effect written once. It takes a Slang function
as config, and the function *is* the look: the effect wraps it in a compute
kernel at `setup()` and runs it over every frame on the GPU, so the pixels
never leave it. Three looks ship in `nodes/looks/` to start from:
`grayscale.slang`, `vignette.slang` and `pixelate.slang`.

```json
{"name": "vignette", "type": "nodes.shader_effect:ShaderEffect",
 "config": {"effect_slang": "<the text of nodes/looks/vignette.slang>"}}
```

A new look is a paragraph of Slang, not a node. It is one function returning
the output pixel for the source pixel `source` at `at`, in pixels from the top
left; `shader_effect_frame_extent()` is the frame's size, and
`shader_effect_source_at(at)` reads any other source pixel, clamped to the
frame's edge:

```hlsl
float4 effect(float4 source, int2 at) {
    return float4(source.bgr, source.a);
}
```

A look that does not compile never gets as far as a frame: the effect is
refused at `setup()` with the compiler's own diagnostic, naming the line of the
look, in `tatolab logs`. That node never reaches `Running`; the rest of the
stream keeps going.

The ports are the inverting effect's, so a look takes its place without
touching anything else. **Into the running stream, over MCP**, that is one
remove and one add. Every tool names the stream, `main`; `ShaderEffect` is
importable as `nodes.shader_effect:ShaderEffect` from this project, which is
the `type` `add_node` takes:

1. `remove_node` with `{"stream": "main", "name": "invertingeffect"}`;
2. `add_node` with `{"stream": "main", "type": "nodes.shader_effect:ShaderEffect",
   "config": {"effect_slang": "…"}}`, which answers the node's name —
   `shadereffect`;
3. `connect` `camerasource`'s `video` to `shadereffect`'s
   `video_from_upstream`, and `shadereffect`'s `video_to_downstream` to
   `inverted-virtual-camera`'s `video` — each as `from_node`, `from_port`,
   `to_node`, `to_port`.

Swapping one look for another is the same remove and add again. The camera,
both sinks and every other node keep running through it — only the second
camera goes without new frames for the moment the splice takes.

Before the look runs, the effect copies each frame device-to-device into
a texture it owns, with `ctx.gpu_limited_access.copy_surface_to_surface`: a
camera publishes buffer-backed frames, and a kernel samples textures.

## Run it

This stream runs on Linux only. `VirtualCameraSink` is a v4l2loopback and
PipeWire mechanism, and macOS has no virtual camera the runtime can create; off
Linux the runtime refuses the stream at load, naming the platform. The nodes are
not what binds it — they use nothing single-floor, and the cross-floor check
`tatolab dev` prints names only the two sinks.

With this machine's runtime running (`tatolabd`, which Tato Studio starts):

```bash
tatolab dev
```

The runtime makes the project's `.venv` on its first load: it installs `tatolab-stream` from PyPI — the authoring surface — beside
`numpy`, and builds `tatolab-camera` and `tatolab-virtualcamera` from this
checkout, as editable path dependencies (`[tool.uv.sources]` in
`pyproject.toml`); each module is a maturin project, so the build needs a Rust
toolchain. The
runtime itself is not on pip.

`tatolab dev` follows the project's log and loads the project again on every
saved edit; `tatolab run` queues the load and returns. Ctrl-C ends `tatolab dev`
and leaves the stream loaded; `tatolab stop <stream>` stops it.

That is enough for a camera. If the application you want to open it in
enumerates `/dev/video*` — OBS, `ffmpeg`, `v4l2-ctl`, Chrome as it ships today —
you want the loopback door, and it needs one permission this machine may not
have yet:

```bash
tatolab enable-virtual-camera
```

A one-time step, and the only one that asks for a password: it installs the
standard grant — v4l2loopback loaded with no devices, its control node tagged
`uaccess` for the logged-in user — through `pkexec`, or `sudo` in a headless
shell. **The runtime never runs it, never loads a module and never asks for
elevation** — running it is yours. `--print` writes the three files and the
commands to stdout instead, for placing them by hand. Where the grant is
missing, `door = "auto"` takes the PipeWire door and says so in the log, naming
this verb.

## Observing it

From another terminal, in this folder:

```bash
tatolab streams                  # the streams this machine's runtime holds, `main` among them
tatolab graph --stream <stream>  # nodes, ports, links and counters, as JSON
tatolab logs -f                  # the project's records, Rust and Python alike
```

The nodes are `camerasource`, `invertingeffect`, `passthrough-virtual-camera`
and `inverted-virtual-camera`.

## Knobs

Each knob is a node's config in `streams/main.graph.json`. Change it in Tato
Studio's inspector or with the runtime's `set_node_config` MCP tool, which
records the edit in the project's history and applies it to the running stream.

| Knob | Node | Effect |
| --- | --- | --- |
| `device_id` | `camerasource` | Capture device — a V4L2 path (`/dev/video2`). Absent picks the first device found. |
| `name` | `passthrough-virtual-camera`, `inverted-virtual-camera` | Each camera's name in every picker. `Tatolab Camera` and `Tatolab Camera Inverted`. |
| `door` | `passthrough-virtual-camera`, `inverted-virtual-camera` | `auto` (the sink's own default), `v4l2loopback`, or `pipewire`. |
