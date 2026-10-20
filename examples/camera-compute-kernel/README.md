# camera-compute-kernel

Camera → a GLSL compute kernel → H.264, watched in a browser. Your webcam in
black and white, graded on the GPU by a shader that lives in this project as a
string.

## The model this example teaches

**A kernel is an object.** It is built once in `setup()`, where the capability
is Full, and dispatched per frame in `process()`. No handle string, fence,
timeline or slot number ever reaches Python — the object is the handle:

```python
self.grayscale_kernel = ctx.gpu_full_access.create_compute_kernel(
    source=GRAYSCALE_COMPUTE_GLSL,
    push_constant_size=GRAYSCALE_STRENGTH_SIZE,
    bindings={"camera_frame": "sampled_texture", "grayscale_frame": "storage_image"},
)
```

```python
self.grayscale_kernel.dispatch(
    bindings={
        "camera_frame": camera_frame_landing_texture,
        "grayscale_frame": grayscale_frame_texture,
    },
    group_count=(240, 135, 1),
    push_constants=self.strength_push_constants,
)
```

Four things fall out of that pair, and they are the whole lesson:

- **The GLSL is a string, and there is no toolchain.** The runtime carries the
  GLSL compiler, so `source=` is compiled at construction on the machine that
  runs the stream. There is no `glslc` to install, no `.spv` to build and no
  build step between editing the shader and running it again. Re-creating an
  identical kernel costs no compilation.
- **Bindings are passed at dispatch, by name, and never persist.** The names
  are the shader's own, read off it by reflection; the `bindings=` mapping at
  construction merely *asserts* that reflection, so renaming `camera_frame` in
  the GLSL and forgetting the Python is refused at `setup()` rather than
  producing a wrong picture. Every dispatch supplies every binding — there is
  no implicit default and no value carried over from the previous frame — and
  an unknown or omitted name raises before any GPU work is submitted.
- **Dispatch is synchronous.** It returns when the work has retired and the
  writes are visible. Several passes that want one submission and one stall are
  recorded inside a `kernel_dispatch_batch()` scope instead — a scope opened on
  the **Full** capability, the one `setup()` is handed, not on the limited
  context `process()` gets. This effect is one pass, so it needs neither.
- **The kernel's output is an engine-owned texture named by surface id.** The
  record this node writes carries that id, an extent and a timestamp, and is
  the same size whatever the resolution. Pixels never ride a link.

### The landing copy, and why it is here

`CameraSource` publishes a **buffer-backed** frame, and a kernel binding
resolves **texture-backed** surfaces only. So each frame is copied into a
texture this node owns before the kernel can sample it:

```python
ctx.gpu_limited_access.copy_surface_to_surface(
    frame.surface_id, camera_frame_landing_texture
)
```

The engine does the copy, so it is worth reading closely. `frame` — the object
`ctx.inputs.read(port, into=VideoFrame)` handed back — holds a claim on the
camera's surface, and that claim is what keeps the camera's pixels still for
the length of the copy. The call picks the copy a buffer-to-texture pair needs,
converts nothing, and returns once the kernel's read of the texture would see
the copied pixels. The pixels never reach the host, and no GPU array package is
involved. The two surfaces must share a format and an extent — the camera's
`rgba` and the texture's `rgba8_unorm` are one format — and a mismatch is
refused by name rather than converted.

### Two rings, two depths

Both textures come from `NodeOutputTextureRing`, which hands each frame the
next slot of a pool the engine owns and replaces the slots when the upstream
extent changes. A slot a consumer still holds is skipped, never rewritten, and
a frame nobody claimed stays resolvable for `depth` publishes. Their depths
differ, and the difference is the contract:

| Ring | Depth | Why |
| --- | --- | --- |
| the landing texture | 1 | `dispatch` returns with the GPU work retired, and nothing outside this node ever names this texture |
| the kernel's output | 2 | the encoder reads it after this node has moved on to the next frame |

When every slot the pool may grow to is held, `next_texture_for_this_frame`
raises: this node drops its own frame and never waits on a consumer.

### And the ordinary parts

`GrayscaleCompute` runs in its own child interpreter with its own GIL, like
every Python node. `CameraSource` and `H264Encoder` are native built-ins inside
the runtime whose per-frame paths never enter an interpreter at all. The input
port declares `newest`, the profile for a stream where a stale frame has no
value once a fresher one exists. The runtime opens no windows: the stream ends
in an encoder whose `encoded_video` it exposes, so a browser plays it.

## Run it

With this machine's runtime running (`tatolabd`, which the installer starts):

```bash
uv sync
tatolab dev
```

`uv sync` installs `tatolab-stream` from PyPI — the authoring surface, and the
only Tatolab package a stream's venv holds. The runtime itself is not on pip.

Use `tatolab dev` while editing — it loads the stream again on every saved edit
— and `tatolab run` otherwise. Both keep the stream as long as the command
runs; Ctrl-C unloads it.

Then open the stream in a browser — your camera, in black and white:

```text
http://127.0.0.1:4446/watch/<machine>/main
```

`http://127.0.0.1:4446/` lists this machine's name and every stream's exposed
ports.

This stream needs a GPU, and says so rather than pretending: the kernel runs on
the runtime's own device, so a machine without one fails while the stream is
starting. It runs on Linux and on macOS alike. There is no fallback — a null GPU
would have no pixels to hand back.

## Editing it

The shader is `GRAYSCALE_COMPUTE_GLSL` in `nodes/grayscale_compute.py`. Change
the luma weights, or the whole effect, and save while `tatolab dev` runs — the
stream loads again and the runtime compiles the new text.

`WORKGROUP_TILE_SIZE` is safe to change on its own: it reaches the shader as a
`#define`, so the tile size and the group count cannot disagree. Everything
else the shader and the Python share is a name, and names are checked.

Two edits worth making on purpose, because each fails in an instructive way:

- Rename a binding in the GLSL but not in the Python. The refusal lands at
  `setup()`, before a single frame, and names the shader's own bindings.
- Bind the camera's own surface id — `frame.surface_id` in place of
  `camera_frame_landing_texture` — and skip the landing copy. The dispatch
  refuses it, because the camera's frame is buffer-backed. That refusal is why
  the landing copy exists, and it is worth seeing once.

## Observing it

From another terminal:

```bash
tatolab streams                 # the streams this machine's runtime holds
tatolab graph --stream main     # nodes, ports, links, exposed ports and counters, as JSON
tatolab logs --stream main -f   # the stream's records, Rust and Python alike
```

A node is named after its class, lowercased — `camerasource`,
`grayscalecompute`, `h264encoder` — unless `add` passes `name=`. An exposed port
is the track `<node>/<port>` of the broadcast `<machine>/<stream>.hang` at the
machine's local relay, so any MoQ client reads it; this repository's
`tatolab-read-track` writes a track's objects to files:

```bash
cargo run -p tatolab-read-track -- http://127.0.0.1:4443 <machine>/main.hang \
    h264encoder/encoded_video --count 30 --out /tmp/grayscale
```

Each file is one H.264 access unit, so the files concatenated in `objects.jsonl`
order are an H.264 stream ffmpeg decodes to PNGs — what the kernel wrote.

The kernel's own `grayscale_frame_to_downstream` port is internal to the stream.
`tatolab expose main grayscalecompute grayscale_frame_to_downstream` makes it a
track too, but each of its objects is a frame's record, not its picture: a
`VideoFrame` naming its `surface_id`, `width`, `height`, `timestamp_ns` and the
camera's `color_info`. The pixels stay in
engine-owned GPU memory that the surface id names, which means nothing outside
the runtime — that is why the picture is shared through the encoder.

## Knobs

Everything is literal config in `stream.py` — edit it and save while
`tatolab dev` runs.

| Knob | Where | Effect |
| --- | --- | --- |
| `CAMERA_DEVICE_ID` | top of `stream.py` | Capture device — a V4L2 path (`/dev/video2`) on Linux, an AVFoundation unique ID on macOS. `None` picks the first device found. |
| `strength` | `stream_builder.add(GrayscaleCompute, config=…)` | 0.0 leaves the picture alone, 1.0 is full luma. |
| `max_width` / `max_height` | `CameraSourceConfig` | Resolution cap; the negotiated format is clamped to fit. |
| `Exposure.PRIVATE` | `expose` | `Exposure.PUBLIC` shares the track off this machine, at every relay the runtime has joined. |
