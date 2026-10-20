# camera-virtual-camera

One capture device in, **two cameras out**. While this stream runs, Chrome, OBS,
Zoom and `v4l2-ctl` all see two extra cameras on the machine — "Tatolab Camera"
showing the capture device untouched, and "Tatolab Camera Inverted" showing the
same picture after a Python node has inverted its colours. Stop the stream and
both are gone.

## The model this example teaches

**A camera is a node.** `VirtualCameraSink` is a native built-in: it takes video
on an input port and has no output port, because its output is a camera other
applications open. Each instance is one camera that exists exactly as long as
its node runs — created at `setup()`, removed at `teardown()` — so from every
other application's point of view a stream starting is a USB camera being
plugged in, and Ctrl-C is it being pulled back out.

**As many as the stream adds.** Two cameras is two `add` calls and one more
`connect`; there is no registry, no device number to reserve, and no limit in
the built-in:

```python
@stream
def main(stream_builder: StreamBuilder) -> None:
    camera = stream_builder.add(CameraSource, config=camera_config)
    inverting_effect = stream_builder.add(InvertingEffect)
    passthrough_virtual_camera = stream_builder.add(
        VirtualCameraSink,
        name="passthrough-virtual-camera",
        config={"name": "Tatolab Camera"},
    )
    inverted_virtual_camera = stream_builder.add(
        VirtualCameraSink,
        name="inverted-virtual-camera",
        config={"name": "Tatolab Camera Inverted"},
    )

    stream_builder.connect(
        camera.output("video"), passthrough_virtual_camera.input("video")
    )
    stream_builder.connect(
        camera.output("video"), inverting_effect.input("video_from_upstream")
    )
    stream_builder.connect(
        inverting_effect.output("video_to_downstream"),
        inverted_virtual_camera.input("video"),
    )
```

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
view and writes their inversion straight into a slot of its own
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

`nodes/shader_effect.py` is an effect written once. It takes a GLSL function
as config, and the function *is* the look: `GlslPixelEffect` compiles it at
`setup()` and runs it over every frame on the GPU, so the pixels never leave
it. Three looks ship in `nodes/looks/` to start from: `grayscale.glsl`,
`vignette.glsl` and `pixelate.glsl`.

```python
from nodes.shader_effect import ShaderEffect, shipped_look_glsl

vignette = stream_builder.add(
    ShaderEffect, config={"effect_glsl": shipped_look_glsl("vignette.glsl")}
)
```

A new look is a paragraph of GLSL, not a node. It is one function returning
the output pixel for the source pixel `source` at `at`, in pixels from the top
left; `streamlib_extent` is the frame's size, and `streamlib_source_at(at)`
reads any other source pixel, clamped to the frame's edge:

```glsl
vec4 effect(vec4 source, ivec2 at) {
    return vec4(source.bgr, source.a);
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
   "config": {"effect_glsl": "…"}}`, which answers the node's name —
   `shadereffect`;
3. `connect` `camerasource`'s `video` to `shadereffect`'s
   `video_from_upstream`, and `shadereffect`'s `video_to_downstream` to
   `inverted-virtual-camera`'s `video` — each as `from_node`, `from_port`,
   `to_node`, `to_port`.

Swapping one look for another is the same remove and add again. The camera,
both sinks and every other node keep running through it — only the second
camera goes without new frames for the moment the splice takes.

Before the look runs, `GlslPixelEffect` copies each frame device-to-device into
a texture it owns, with `ctx.gpu_limited_access.copy_surface_to_surface`: a
camera publishes buffer-backed frames, and a kernel samples textures.

## Run it

This stream runs on Linux only. `VirtualCameraSink` is a v4l2loopback and
PipeWire mechanism, and macOS has no virtual camera the runtime can create; off
Linux the runtime refuses the stream at load, naming the platform. The nodes are
not what binds it — they use nothing single-floor, and the cross-floor check
`tatolab dev` prints names only the two sinks in `stream.py`.

With this machine's runtime running (`tatolabd`, which the installer starts):

```bash
uv sync
tatolab dev
```

`uv sync` installs `tatolab-stream` from PyPI — the authoring surface, and the
only Tatolab package a stream's venv holds — beside `numpy`. The runtime itself
is not on pip.

Use `tatolab dev` while editing — it loads the stream again on every saved edit
— and `tatolab run` otherwise. Both keep the stream as long as the command
runs; Ctrl-C unloads it, and both cameras go with it.

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

The cameras are the output, so the first place to look is another application.
Both appear by name in any picker: Chrome's `chrome://settings/content/camera`
or a video call's device list, OBS's "Video Capture Device" source, GNOME's
Camera app.

From a terminal, which door the sinks took decides what can see them — the log
line at setup says which it was. **On the v4l2loopback door** they are ordinary
video nodes:

```bash
v4l2-ctl --list-devices        # both by name, beside the real capture devices
ffplay /dev/videoN             # whichever number the list gave you
```

**On the PipeWire door** there is no `/dev/video*` entry to find, so those two
commands will not show them. Ask PipeWire instead:

```bash
pw-dump | grep -A2 node.description   # both by name, among the graph's nodes
pw-cli ls Node                        # the same, node by node
```

Either way, run the listing again after Ctrl-C and neither camera is there.

The stream itself is observed like any other, from another terminal:

```bash
tatolab streams                 # the streams this machine's runtime holds
tatolab graph --stream main     # nodes, ports, links, exposed ports and counters, as JSON
tatolab logs --stream main -f   # the stream's records, Rust and Python alike
```

The nodes are `camerasource`, `invertingeffect`, `passthrough-virtual-camera`
and `inverted-virtual-camera` — a node is named after its class, lowercased,
unless `add` passes `name=`. `tatolab logs` is where the door decision is
written down: one record per sink at setup, carrying the camera, the `door`,
the reason it chose that door and, on the loopback door, the device it created
and the module version; then the tier the GPU writes the device's buffers
through; and at teardown the frames each sink wrote and dropped.

To watch the effect's frames rather than its picture, expose its output. An
exposed port is the track `<node>/<port>` of the broadcast
`<machine>/<stream>.hang` at the machine's local relay, so any MoQ client reads
it; read the machine's name out of `tatolab graph`. This repository's
`tatolab-read-track` writes a track's objects to files:

```bash
tatolab expose main invertingeffect video_to_downstream
cargo run -p tatolab-read-track -- http://127.0.0.1:4443 <machine>/main.hang \
    invertingeffect/video_to_downstream --count 5 --out /tmp/effect
```

Each object is one frame's record, and it is metadata, not picture —
`surface_id`, `width`, `height`, `timestamp_ns` and `color_info` — naming pixels
that stay in engine-owned GPU memory, so it is a few hundred bytes whatever the
resolution. The `surface_id` differs from the camera's: it names the effect's
own ring slot, which is the whole point of the ring. The `timestamp_ns` does
not — the effect carries the camera's stamp through, so both cameras present
the same picture under the same time.

## Knobs

Everything is literal config at the top of `stream.py` — edit it and save while
`tatolab dev` runs.

| Knob | Where | Effect |
| --- | --- | --- |
| `CAMERA_DEVICE_ID` | top of `stream.py` | Capture device — a V4L2 path (`/dev/video2`). `None` picks the first device found. |
| `PASSTHROUGH_VIRTUAL_CAMERA_NAME` | top of `stream.py` | The untouched camera's name in every picker. `Tatolab Camera`. |
| `INVERTED_VIRTUAL_CAMERA_NAME` | top of `stream.py` | The inverted camera's name. `Tatolab Camera Inverted`. |
| `VIRTUAL_CAMERA_DOOR` | top of `stream.py` | `auto` (the sink's own default), `v4l2loopback`, or `pipewire`. Applies to both sinks. |

A name is what every picker shows, on either door. The loopback device's label
field holds 31 bytes, so a longer name is cut to fit — pick one that reads whole
in a list. This stream always passes a camera name; a sink given none is named
`StreamLib Camera` plus a short id unique to the node and the stream.

Naming `v4l2loopback` explicitly turns the fallback off: without the permission
the sink refuses at `setup()` by name, saying it cannot create a camera and
naming `tatolab enable-virtual-camera`. That node never reaches `Running`, the
rest of the stream keeps going, and `tatolab graph --stream main` shows it in
state `Error` — which is what you want when you meant the door that every
application can see and would rather know than be quietly given the other one.
`pipewire` is taken whatever the control node says, and refuses by name where
no PipeWire session answers.
