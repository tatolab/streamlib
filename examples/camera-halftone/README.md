# camera-halftone

Camera → a GLSL compute kernel → H.264, watched in a browser. Your webcam
printed as newsprint: one ink dot per cell, each dot as big as that cell is
bright. The pixels never leave the GPU.

## What the shader does

Halftone is the trick newspapers print photographs with. Dice the picture into
cells; measure each cell's brightness from a single sample at its centre; draw
one dot there whose size is that brightness. Light regions get fat dots that
touch their neighbours, dark ones get specks, and the eye averages the two back
into a picture.

```glsl
ivec2 cell = at / dial.cell_size;
ivec2 centre = min(cell * dial.cell_size + dial.cell_size / 2, extent - 1);
vec4 ink = texelFetch(camera_frame, centre, 0);
```

What scales with brightness is the dot's **area**, not its radius — a cell
twice as bright is twice as covered in ink — so the radius takes a square
root. A radius scaled by brightness directly loses the shadows: a quarter grey
inks one texel of the sixty-four, against nine here. The edge gets one texel of
feather for a reason worth knowing too, and the shader says which.

**Every invocation reads a texel it does not write.** That is the one line
worth carrying away, and it is what separates this from a per-texel grade like
`camera-compute-kernel`'s: 64 invocations in an 8×8 cell all sample the same
centre texel, and each writes only its own. A gather like that cannot run in
place — an invocation early in the cell would overwrite the sample its
neighbours have not read yet — so the effect needs a texture to read and a
different texture to write, which is exactly the two-binding shape the kernel
declares.

## The stream

```python
@stream
def main(stream_builder: StreamBuilder) -> None:
    camera = stream_builder.add(CameraSource, config=camera_config)
    halftone = stream_builder.add(HalftoneCompute, config={"cell_size": 8, ...})
    encoder = stream_builder.add(H264Encoder)
    stream_builder.connect(
        camera.output("video"), halftone.input("camera_frame_from_upstream")
    )
    stream_builder.connect(
        halftone.output("halftone_frame_to_downstream"), encoder.input("video")
    )
    stream_builder.expose(encoder.output("encoded_video"), Exposure.PRIVATE)
```

- `tatolab dev` reads `stream.py` from the current directory and loads its one
  `@stream` function into this machine's runtime, `tatolabd`.
- `CameraSource` and `H264Encoder` are **native built-ins**: they ship inside
  the runtime, and their per-frame paths never enter a Python interpreter.
- `HalftoneCompute` is this project's own node, in `nodes/halftone_compute.py`
  — never in `stream.py`, because every Python node runs in its own child
  interpreter with its own GIL, which imports the class by name. Its config is
  the `HalftoneComputeConfig` dataclass its `__init__` names; `add` passes a
  dict and the child builds the dataclass from it.
- The input port declares `delivery_profile="newest"`, the profile for a
  stream where a stale frame has no value once a fresher one exists.
- **The runtime opens no windows.** What a stream shows, it exposes: `expose`
  makes the encoder's output a track at this machine's local relay, which a
  browser plays and any other stream or MoQ client reads. `H264Encoder` takes
  a texture-backed frame like the kernel's output as readily as the camera's.

## A kernel is an object

Built once in `setup()`, where the capability is Full, and dispatched per frame
in `process()`. No handle string, fence, timeline or slot number ever reaches
Python — the object is the handle:

```python
self.halftone_kernel = ctx.gpu_full_access.create_compute_kernel(
    source=HALFTONE_COMPUTE_GLSL,
    push_constant_size=HALFTONE_DIAL_SIZE,
    bindings={"camera_frame": "sampled_texture", "halftone_frame": "storage_image"},
)
```

```python
self.halftone_kernel.dispatch(
    bindings={
        "camera_frame": camera_frame_landing_texture,
        "halftone_frame": halftone_frame_texture,
    },
    group_count=(240, 135, 1),
    push_constants=self.halftone_dial_push_constants,
)
```

- **The GLSL is a string, and there is no toolchain.** The engine compiles
  `source=` when the node sets up, on the machine that runs the stream. There
  is no `glslc` to install, no `.spv` to build and no build step between
  editing the shader and saving.
- **Bindings are passed at dispatch, by name, and never persist.** The names
  are the shader's own, read off it by reflection; the `bindings=` mapping at
  construction merely *asserts* that reflection, so renaming `camera_frame` in
  the GLSL and forgetting the Python is refused at `setup()` rather than
  producing a wrong picture. Every dispatch supplies every binding, and an
  unknown or omitted name raises before any GPU work is submitted.
- **Dispatch is synchronous.** It returns when the work has retired and the
  writes are visible. This effect is one pass, so it needs no
  `kernel_dispatch_batch()` scope.
- **The kernel's output is an engine-owned texture named by surface id.** The
  record this node writes carries that id, an extent and the camera's
  timestamp. Pixels never ride a link.

### The three dials, and how they reach the shader

The cell size, the ink boost and the background are all runtime values in one
push-constant block:

```glsl
layout(push_constant) uniform HalftoneDial {
    int cell_size;
    float dot_boost;
    float background_level;
} dial;
```

```python
HALFTONE_DIAL_FORMAT = "<iff"
```

That pairing is a **layout contract**, not a convenience. `struct.pack` writes
exactly the bytes the block expects: three 4-byte scalars, which std430 packs
at offsets 0, 4 and 8 with no padding between them, little-endian. Nothing
checks it — `push_constants=` takes bytes and the shader reads bytes — so a
`float` where the block declares an `int` produces a picture rather than an
error. Spell the format string against the block, and change the two together.

Padding is the part that bites once a block grows: a `vec2` aligns to 8 bytes
and a `vec3` or `vec4` to 16, so inserting one where the scalars are shifts
every field after it. Three scalars is the case with nothing to get wrong,
which is why it is worth seeing before the case that has.

`cell_size` deliberately is *not* `WORKGROUP_TILE_SIZE`. A tile is how the GPU
is diced up into workgroups; a cell is how the picture is diced up into dots.
They are both 8 here by coincidence of taste, and changing one does not change
the other.

### The landing copy

`CameraSource` publishes a **buffer-backed** frame, and a pixel buffer cannot
bind as a sampled texture. So each frame is copied into a texture this node
owns before the kernel can sample it:

```python
ctx.gpu_limited_access.copy_surface_to_surface(
    frame.surface_id, camera_frame_landing_texture
)
```

`frame` — the object `ctx.inputs.read(port, into=VideoFrame)` handed back —
holds a claim on the camera's surface, and that claim is what keeps the
camera's pixels still for the length of the copy. The engine picks the copy a
buffer-to-texture pair needs, converts nothing, and returns once the kernel's
read of the texture would see the copied pixels. The pixels never reach the
host, and no GPU array package is involved.

`examples/camera-compute-kernel` is the shorter example built around that door
alone, if this one is the first you have read.

### Two rings, two depths

Both textures come from `NodeOutputTextureRing`, which names a pool the engine
owns and asks it for the next slot each frame; an extent change replaces the
slots with ones the new size. Their depths differ, and the difference is the
contract:

| Ring | Depth | Why |
| --- | --- | --- |
| the landing texture | 1 | `dispatch` returns with the GPU work retired, and nothing outside this node ever names this texture |
| the kernel's output | 2 (the standard) | the encoder reads it in its own time, downstream |

A slot a consumer still holds is skipped, never rewritten, so the encoder never
reads newer pixels under an older frame's id. A frame nobody claimed stays
resolvable for `depth` publishes, then its id is refused as recycled. When
every slot the pool may grow to is held, the ring raises and this node drops its
own frame — it never waits on a consumer.

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

Then open the stream in a browser — your camera, printed as dots:

```text
http://127.0.0.1:4446/watch/<machine>/main
```

`http://127.0.0.1:4446/` lists this machine's name and every stream's exposed
ports.

This stream needs real hardware: the kernel runs on the engine's own Vulkan
device, so a machine without one fails while the stream is starting. It runs on
Linux and on macOS alike.

## Editing it

The shader is `HALFTONE_COMPUTE_GLSL` in `nodes/halftone_compute.py`. Save it
while `tatolab dev` runs and the stream loads again with the new text compiled.

Four edits worth making, in rising order of how much they teach:

- **Turn the screen up.** Raise `cell_size` from 8 to 24 in `stream.py` and the
  dots get big enough to count, the effect unmistakable from across a room;
  drop it to 3 and the picture comes back. No shader change — it is a push
  constant.
- **Scale the radius by luma directly** — drop the `sqrt` — and watch the
  shadows collapse. Against a colour-bar pattern the magenta, red and blue bars
  go to near-black while the four brighter than them screen fine. It is the
  clearest demonstration here that a halftone's tone lives in dot *area*.
- **Drop the feather.** Replace the `smoothstep` coverage with
  `float coverage = distance_from_centre <= radius ? 1.0 : 0.0;`. Still
  recognisably halftone on a still frame, and visibly crawling once you move —
  a dot's radius changes by a fraction of a texel per frame, and a hard edge can
  only move a whole texel at a time.
- **Screen the dots monochrome.** Replace `ink.rgb` in `dot_colour` with
  `vec3(luma)` and the picture becomes true black-and-white newsprint rather
  than a colour screen.

Two edits that fail on purpose, because each failure is the contract stating
itself:

- Rename a binding in the GLSL but not in the Python. The refusal lands at
  `setup()`, before a single frame, and names the shader's own bindings.
- Bind the camera's own surface id — `frame.surface_id` in place of
  `camera_frame_landing_texture` — and skip the landing copy. The dispatch
  refuses it, because the camera's frame is a buffer, not a texture. That
  refusal is why the landing copy exists.

## Observing it

From another terminal:

```bash
tatolab streams                 # the streams this machine's runtime holds
tatolab graph --stream main     # nodes, ports, links, exposed ports and counters, as JSON
tatolab logs --stream main -f   # the stream's records, Rust and Python alike
```

The nodes are `camerasource`, `halftonecompute` and `h264encoder` — each named
after its class, lowercased, since `add` passes no `name=`. An exposed port is
the track `<node>/<port>` of the broadcast `<machine>/<stream>.hang` at the
machine's local relay, so any MoQ client reads it; this repository's
`tatolab-read-track` writes a track's objects to files:

```bash
cargo run -p tatolab-read-track -- http://127.0.0.1:4443 <machine>/main.hang \
    h264encoder/encoded_video --count 30 --out /tmp/halftone
```

Each file is one H.264 access unit, so the files concatenated in `objects.jsonl`
order are an H.264 stream ffmpeg decodes — to PNGs, to see exactly what the
kernel wrote.

The kernel's own `halftone_frame_to_downstream` port is internal to the stream.
`tatolab expose main halftonecompute halftone_frame_to_downstream` makes it a
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
| `CAPTURE_MAX_WIDTH` / `CAPTURE_MAX_HEIGHT` | top of `stream.py` | The camera's resolution cap; the negotiated format is clamped to fit. 1920×1080. |
| `cell_size` | `add(HalftoneCompute, config=…)` | Pixels across one dot cell; bigger is a coarser screen. At least 2. |
| `dot_boost` | `add(HalftoneCompute, config=…)` | How far a dot's ink is lifted above the colour it sampled; above 0.0. |
| `background_level` | `add(HalftoneCompute, config=…)` | The grey the paper is, 0.0 black to 1.0 white. |
| `Exposure.PRIVATE` | `expose` | `Exposure.PUBLIC` shares the track off this machine, at every relay the runtime has joined. |
