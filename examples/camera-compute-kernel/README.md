# camera-compute-kernel

Camera → a GLSL compute kernel → window. Your webcam in black and white,
graded on the GPU by a shader that lives in this app as a string.

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
    bindings={"camera_frame": camera_frame_texture,
              "grayscale_frame": grayscale_frame_texture},
    group_count=(240, 135, 1),
    push_constants=self.strength_push_constants,
)
```

Four things fall out of that pair, and they are the whole lesson:

- **The GLSL is a string, and there is no toolchain.** The wheel carries a C++
  GLSL compiler, so `source=` is compiled at construction on the machine that
  runs the app. There is no `glslc` to install, no `.spv` to build and no build
  step between editing the shader and re-running. Re-creating an identical
  kernel costs no compilation — the result is cached under everything that
  changes it, never under the source alone.
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
  bag this processor writes carries that id, an extent and a timestamp — 167
  bytes on the wire, header included, and the same 167 whatever the resolution.
  (`streamlib tap` reports it as `byte_len`; the camera's own bag is 248,
  the difference being the colour metadata it carries and this one does not.)
  Pixels never ride a link.

### The landing copy, and why it is here

`CameraSource` publishes a **buffer-backed** frame, and a kernel binding
resolves **texture-backed** surfaces only — hand a dispatch a buffer-backed
surface id and it is refused by name. So each frame is copied into a texture
this processor owns before the kernel can sample it:

```python
with camera_frame_texture.as_device_tensor() as writable_texture:
    cupy.from_dlpack(writable_texture)[...] = cupy.from_dlpack(frame)
```

That is the engine's interop door, and it is worth reading closely. `frame` —
the object `ctx.inputs.read(port, into=VideoFrame)` handed back — is a DLPack
producer in its own right, so `cupy.from_dlpack(frame)` is the entire read,
GPU-resident, and the claim that cast took is what holds the camera's pixels
still for the length of the copy. Entering the device-tensor scope hands the
destination texture out as a linear view; leaving it blits the write back,
ordered by the engine ahead of its own next read of that texture. The pixels
never reach the host, and cupy does nothing here but the copy — any
DLPack-speaking GPU array package would serve, which is the point.

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

`GrayscaleCompute` runs in its own child interpreter with its own GIL, like
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

A window opens with your camera in it, in black and white. Ctrl-C stops it.

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

The shader is `GRAYSCALE_COMPUTE_GLSL` in `processors/grayscale_compute.py`.
Change the luma weights, or the whole effect, and re-run — the engine compiles
the new text at startup, and a warm restart is sub-second. That is the edit
loop; there is no reload-on-save and nothing is cached against you.

`WORKGROUP_TILE_SIZE` is safe to change on its own: it reaches the shader as a
`#define`, so the tile size and the group count cannot disagree. Everything
else the shader and the Python share is a name, and names are checked.

Two edits worth making on purpose, because each fails in an instructive way:

- Rename a binding in the GLSL but not in the Python. The refusal lands at
  `setup()`, before a single frame, and names the shader's own bindings.
- Bind the camera's own surface id — `frame.surface_id` in place of
  `camera_frame_texture` — and skip the landing copy. The dispatch refuses,
  saying the graph cannot resolve that surface to a device texture. That
  refusal is the reason the landing copy exists, and it is worth seeing once.

## Observing it

From another terminal with this venv activated, while it runs:

```bash
streamlib nodes                       # the live nodes on this machine
streamlib graph                       # processors, ports, links and their states
streamlib logs --list                 # the runtimes that have a log file
streamlib logs <RUNTIME_ID> --follow  # tail one of them, like `tail -F`
```

`graph` renders per-link drop counts under a node's `metrics`, but only for a
destination that lives in the app process — here, the window. `GrayscaleCompute`
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
    if source["port_name"] == "grayscale_frame_to_downstream":
        print((source["processor_id"] + "/" + source["port_name"]).lower())
        break')
streamlib exchange --channel "$CHANNEL" --count 3 --out /tmp/grayscale
```

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
returns whatever arrived. Worth knowing before either goes in a script.

## Runtime knobs

Everything is literal config in `app.py` — edit it and re-run.

| Knob | Where | What it does |
| --- | --- | --- |
| `strength` | `rt.add(GrayscaleCompute, config=…)` | 0.0 leaves the picture alone, 1.0 is full luma |
| `max_width` / `max_height` | `rt.add(CameraSource, config=…)` | the capture size to ask the device for |
| `title` / `width` / `height` / `scaling` | `rt.add(DisplayWindow, config=…)` | window geometry; `scaling` is `fit` / `fill` / `stretch` |
| `STREAMLIB_CAMERA_DEVICE` | environment | capture device (e.g. `/dev/video0`); unset picks the first device found |
