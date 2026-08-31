# camera-halftone

Camera → a GLSL compute kernel → window. Your webcam printed as newsprint:
one ink dot per cell, each dot as big as that cell is bright.

The effect is recovered from a retired example that ran it through a Deno
subprocess and a WebGPU pipeline of its own. Nothing of that survives — the
shader is rewritten as GLSL the engine compiles, and the pixels never leave
the GPU or the engine's own device.

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
root. The mined original scaled the radius directly, which is why every tone
below mid grey collapsed to a single-texel speck there and does not here. The
edge gets one texel of feather for a reason worth knowing too, and the shader
says which.

**Every invocation reads a texel it does not write.** That is the one line
worth carrying away, and it is what separates this from a per-texel grade like
`camera-compute-kernel`'s: 64 invocations in an 8×8 cell all sample the same
centre texel, and each writes only its own. A gather like that cannot run in
place — an invocation early in the cell would overwrite the sample its
neighbours have not read yet — so the effect needs a texture to read and a
different texture to write, which is exactly the two-binding shape the kernel
declares.

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
    bindings={"camera_frame": camera_frame_landing_texture,
              "halftone_frame": halftone_frame_texture},
    group_count=(240, 135, 1),
    push_constants=self.halftone_dial_push_constants,
)
```

- **The GLSL is a string, and there is no toolchain.** The wheel carries a C++
  GLSL compiler, so `source=` is compiled at construction on the machine that
  runs the app. There is no `glslc` to install, no `.spv` to build and no build
  step between editing the shader and re-running.
- **Bindings are passed at dispatch, by name, and never persist.** The names
  are the shader's own, read off it by reflection; the `bindings=` mapping at
  construction merely *asserts* that reflection, so renaming `camera_frame` in
  the GLSL and forgetting the Python is refused at `setup()` rather than
  producing a wrong picture. Every dispatch supplies every binding, and an
  unknown or omitted name raises before any GPU work is submitted.
- **Dispatch is synchronous.** It returns when the work has retired and the
  writes are visible. This effect is one pass, so it needs no batching scope.
- **The kernel's output is an engine-owned texture named by surface id.** The
  bag this processor writes carries that id, an extent and a timestamp.
  Pixels never ride a link.

### The three dials, and how they reach the shader

The mined original hardcoded its cell size, its ink boost and its background.
Here all three are runtime values in one push-constant block:

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

`CameraSource` publishes a **buffer-backed** frame, and a kernel binding
resolves **texture-backed** surfaces only — hand a dispatch a buffer-backed
surface id and it is refused by name. So each frame is copied into a texture
this processor owns before the kernel can sample it:

```python
with camera_frame_landing_texture.as_device_tensor() as writable_texture:
    cupy.from_dlpack(writable_texture)[...] = cupy.from_dlpack(frame)
```

`frame` — the object `ctx.inputs.read(port, into=VideoFrame)` handed back — is
a DLPack producer in its own right, so `cupy.from_dlpack(frame)` is the entire
read, GPU-resident, and the claim that cast took is what holds the camera's
pixels still for the length of the copy. Entering the device-tensor scope hands
the destination texture out as a linear view; leaving it blits the write back,
ordered by the engine ahead of its own next read of that texture. The pixels
never reach the host, and cupy does nothing here but the copy — any
DLPack-speaking GPU array package would serve, which is the point.

`examples/camera-compute-kernel` is the shorter example built around that door
alone, if this one is the first you have read.

### Two rings, two depths

Both textures come from `ProcessorOutputTextureRing`, which allocates once and
re-allocates only when the upstream extent changes. Their depths differ, and
the difference is the contract:

| Ring | Depth | Why |
| --- | --- | --- |
| the landing texture | 1 | `dispatch` returns with the GPU work retired, and nothing outside this processor ever names this texture |
| the kernel's output | 2 | the window may still be sampling the frame before this one |

Depth bounds how far behind a consumer may fall, not how fast anything runs: a
slot is redrawn when its turn comes round again, so a consumer still holding a
frame `depth` publishes old reads the producer's newer pixels.

### And the ordinary parts

`HalftoneCompute` runs in its own child interpreter with its own GIL, like
every Python processor. `CameraSource` and `DisplayWindow` are native built-ins
inside the wheel whose per-frame paths never enter an interpreter at all. The
input port declares `newest`, the profile for a stream where a stale frame has
no value once a fresher one exists.

## Run it

```bash
uv venv --python 3.12 && uv sync
source .venv/bin/activate
streamlib run
```

A window opens with your camera in it, printed as dots. Ctrl-C stops it.

`uv sync` installs `streamlib` from the simple index pinned in
`pyproject.toml`; the wheel carries the Python API, the engine and the
`streamlib` CLI. The CLI lands in `.venv/bin`, so it needs the venv on your
`PATH` — activate it as above, or spell it `.venv/bin/streamlib` every time.

**Not `uv run streamlib`.** It re-syncs the environment from `pyproject.toml`
first, which silently replaces a locally built wheel with the released one —
exactly what the next paragraph is for.

To work against a checkout rather than a release, install that checkout's wheel
into this venv instead:

```bash
uv pip install maturin
maturin develop --manifest-path ../../sdk/streamlib-python-wheel/Cargo.toml
```

This app needs real hardware, and says so rather than pretending: the kernel
runs on the engine's own Vulkan device and `cupy-cuda13x` wants a CUDA runtime,
so a machine with neither fails while the graph is starting. There is no
demotion path here of the kind the audio backends have — a null GPU would have
no pixels to hand back.

## Editing it

The shader is `HALFTONE_COMPUTE_GLSL` in `processors/halftone_compute.py`.
Change it and re-run — the engine compiles the new text at startup, and a warm
restart is sub-second. That is the edit loop; there is no reload-on-save and
nothing is cached against you.

Four edits worth making, in rising order of how much they teach:

- **Turn the screen up.** `cell_size` at 24 in `app.py` makes the dots big
  enough to count and the effect unmistakable from across a room; at 3 it
  approaches the original picture. No shader change — it is a push constant.
- **Scale the radius by luma directly** — drop the `sqrt` — and watch the
  shadows collapse. That is the mined original's tone curve, and against a
  colour-bar pattern the magenta, red and blue bars go to near-black while the
  top three stay fine. It is the clearest demonstration in this app that a
  halftone's tone lives in dot *area*.
- **Drop the feather.** Replace the `smoothstep` coverage with
  `float coverage = distance_from_centre <= radius ? 1.0 : 0.0;`, also what the
  mined original did. Still recognisably halftone on a still frame, and visibly
  crawling once you move — a dot's radius changes by a fraction of a texel per
  frame, and a hard edge can only move a whole texel at a time.
- **Screen the dots monochrome.** Replace `ink.rgb` in `dot_colour` with
  `vec3(luma)` and the picture becomes true black-and-white newsprint rather
  than a colour screen.

Two edits that fail on purpose, because each failure is the contract stating
itself:

- Rename a binding in the GLSL but not in the Python. The refusal lands at
  `setup()`, before a single frame, and names the shader's own bindings.
- Bind the camera's own surface id — `frame.surface_id` in place of
  `camera_frame_landing_texture` — and skip the landing copy. The dispatch
  refuses, saying the graph cannot resolve that surface to a device texture.
  Resolving the frame first (`ctx.gpu_limited_access.resolve_surface(...)`) and
  binding the handle is refused identically, because a handle binding travels
  as its surface id. That refusal is why the landing copy exists.

## Observing it

From another terminal with this venv activated, while it runs:

```bash
streamlib nodes                       # the live nodes on this machine
streamlib graph                       # processors, ports, links and their states
streamlib logs --list                 # the runtimes that have a log file
streamlib logs <RUNTIME_ID> --follow  # tail one of them, like `tail -F`
```

`graph` renders per-link drop counts under a node's `metrics`, but only for a
destination that lives in the app process — here, the window. `HalftoneCompute`
is helper-placed, like every Python processor, and counts its losses inside its
own child; its node carries no `metrics` key at all rather than a zero the
parent cannot stand behind. So a quiet `graph` is not yet proof that the effect
dropped nothing.

`logs` takes the runtime id, not a node — it reads the file on disk, and the
ids `--list` prints are what `nodes` calls `runtime_id`.

To see what the kernel actually wrote rather than what the window shows, tap
its output channel and exchange the surface ids for PNGs. The channel name is
the source processor's id, lowercased, joined to its output port — read it out
of `graph` rather than spelling it by hand:

```bash
CHANNEL=$(streamlib graph | python3 -c '
import json, sys
for link in json.load(sys.stdin)["links"]:
    source = link["source"]
    if source["port_name"] == "halftone_frame_to_downstream":
        print((source["processor_id"] + "/" + source["port_name"]).lower())
        break')
streamlib exchange --channel "$CHANNEL" --count 3 --out /tmp/halftone
```

A halftone frame is the one effect in the showcase you can grade from a still:
zoom a PNG in and the dot grid is either there or it is not.

`streamlib tap "$CHANNEL"` on the same channel shows the bag itself: a surface
id, an extent, a timestamp, and no pixels. `exchange` is the door that turns
one of those ids into a full-resolution PNG — the same door any API consumer
uses, with no window in the graph and no display server in the path. It prints
the paths it wrote, one per line; read those rather than listing the directory,
which is not cleared between runs.

`--count` here is a target, not a ceiling: `exchange` keeps tapping — up to
eight bounded sampling rounds — until it has that many frames, and a run that
comes up short **exits nonzero** and says how many it got. That is the opposite
of `tap`'s own `--count`, which is an upper bound over one 500 ms window and
returns whatever arrived.

## Runtime knobs

Everything is literal config in `app.py` — edit it and re-run.

| Knob | Where | What it does |
| --- | --- | --- |
| `cell_size` | `rt.add(HalftoneCompute, config=…)` | pixels across one dot cell; bigger is a coarser screen |
| `dot_boost` | `rt.add(HalftoneCompute, config=…)` | how far a dot's ink is lifted above the colour it sampled |
| `background_level` | `rt.add(HalftoneCompute, config=…)` | the grey the paper is, 0.0 black to 1.0 white |
| `max_width` / `max_height` | `rt.add(CameraSource, config=…)` | the capture size to ask the device for |
| `title` / `width` / `height` / `scaling` | `rt.add(DisplayWindow, config=…)` | window geometry; `scaling` is `fit` / `fill` / `stretch` |
| `STREAMLIB_CAMERA_DEVICE` | environment | capture device (e.g. `/dev/video0`); unset picks the first device found |
