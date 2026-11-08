# camera-halftone

Camera → a Slang compute kernel, seen in Tato Studio's viewer. Your webcam
printed as newsprint: one ink dot per cell, each dot as big as that cell is
bright. The pixels never leave the GPU.

## What the shader does

Halftone is the trick newspapers print photographs with. Dice the picture into
cells; measure each cell's brightness from a single sample at its centre; draw
one dot there whose size is that brightness. Light regions get fat dots that
touch their neighbours, dark ones get specks, and the eye averages the two back
into a picture.

```hlsl
int2 cell = at / dial.cell_size;
int2 centre = min(cell * dial.cell_size + dial.cell_size / 2, extent - 1);
float4 ink = camera_frame.Load(int3(centre, 0));
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

`streams/main.graph.json` is the stream: two nodes and the link between them.

```json
{
  "nodes": [
    {"name": "camerasource", "type": "tatolab.camera.camera_source:CameraSource", "config": {"max_width": 1920, "max_height": 1080}},
    {"name": "halftonecompute", "type": "nodes.halftone_compute:HalftoneCompute", "config": {"cell_size": 8, "dot_boost": 1.3, "background_level": 0.0627}}
  ],
  "links": [
    {"source": {"node": "camerasource", "port": "video"}, "target": {"node": "halftonecompute", "port": "camera_frame_from_upstream"}}
  ]
}
```

The file itself names each node and each link end by the node's UUID; the
names stand in for them here.

- `tatolab dev` queues a load of this project: this machine's runtime,
  `tatolabd`, reads every graph document under `streams/` and loads each as its
  own stream.
- `CameraSource` comes from the **`tatolab-camera` module**, a dependency in
  `pyproject.toml`. It runs in its own child process, and its per-frame path is
  the module's Rust, never Python.
- `HalftoneCompute` is this project's own node, in `nodes/halftone_compute.py`
  — its own module, because every Python node runs in its own child
  interpreter with its own GIL, which imports the class by name. Its config is
  the `HalftoneComputeConfig` dataclass its `__init__` names; the graph document holds
  a dict and the child builds the dataclass from it.
- The input port is `Input(Image, delivery="newest")`, the profile for a
  stream where a stale frame has no value once a fresher one exists.
- **The runtime opens no windows.** Any port of the running stream, the
  kernel's output among them, is seen in Tato Studio's viewer.

## A kernel is an object

Built once in `setup()`, where the capability is Full, and dispatched per frame
in `process()`. No handle string, fence, timeline or slot number ever reaches
Python — the object is the handle:

```python
self.halftone_kernel = ctx.gpu_full_access.create_compute_kernel(
    source=HALFTONE_COMPUTE_SLANG,
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

- **The Slang is a string, and there is no toolchain.** The runtime compiles
  `source=` when the node sets up, on the machine that runs the stream. There
  is no `slangc` to install, no `.spv` to build and no build step between
  editing the shader and saving.
- **Bindings are passed at dispatch, by name, and never persist.** The names
  are the shader's own, read off it by reflection; the `bindings=` mapping at
  construction merely *asserts* that reflection, so renaming `camera_frame` in
  the Slang and forgetting the Python is refused at `setup()` rather than
  producing a wrong picture. Every dispatch supplies every binding, and an
  unknown or omitted name raises before any GPU work is submitted.
- **Dispatch is synchronous.** It returns when the work has retired and the
  writes are visible. This effect is one pass, so it needs no
  `kernel_dispatch_batch()` scope.
- **The kernel's output is an engine-owned texture named by surface id.** The
  node writes it with `ImageFrameToWrite.of_texture(texture, frame.colour)`, so
  the frame downstream carries that id, an extent, the camera's colour and its
  timestamp. Pixels never ride a link.

### The three dials, and how they reach the shader

The cell size, the ink boost and the background are all runtime values in one
push-constant block:

```hlsl
struct HalftoneDial {
    int cell_size;
    float dot_boost;
    float background_level;
};
[[vk::push_constant]] ConstantBuffer<HalftoneDial> dial;
```

```python
HALFTONE_DIAL_FORMAT = "<iff"
```

That pairing is a **layout contract**, not a convenience. `struct.pack` writes
exactly the bytes the block expects: three 4-byte scalars, which a push-constant block packs
at offsets 0, 4 and 8 with no padding between them, little-endian. Nothing
checks it — `push_constants=` takes bytes and the shader reads bytes — so a
`float` where the block declares an `int` produces a picture rather than an
error. Spell the format string against the block, and change the two together.

Padding is the part that bites once a block grows: a `float2` aligns to 8 bytes
and a `float3` or `float4` to 16, so inserting one where the scalars are shifts
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

`frame` — the object `ctx.inputs.read(self.camera_frame_from_upstream)` handed back, an `ImageFrame`, —
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
| the kernel's output | 2 (the standard) | the viewer, or any node downstream, reads it in its own time |

A slot a consumer still holds is skipped, never rewritten, so a reader never
sees newer pixels under an older frame's id. A frame nobody claimed stays
resolvable for `depth` publishes, then its id is refused as recycled. When
every slot the pool may grow to is held, the ring raises and this node drops its
own frame — it never waits on a consumer.

## Run it

With this machine's runtime running (`tatolabd`, which Tato Studio starts):

```bash
tatolab dev
```

The runtime makes the project's `.venv` on its first load: it installs `tatolab-stream` from PyPI — the authoring surface — and
builds `tatolab-camera` from this checkout, as an editable path dependency
(`[tool.uv.sources]` in `pyproject.toml`); the module is a maturin project,
so the build needs a Rust toolchain. The runtime itself is not on pip.

`tatolab dev` follows the project's log and loads the project again on every
saved edit; `tatolab run` queues the load and returns. Ctrl-C ends `tatolab dev`
and leaves the stream loaded; `tatolab stop <stream>` stops it.

Then watch `halftonecompute`'s `halftone_frame_to_downstream` in Tato Studio's
viewer — your camera, printed as dots.

This stream needs real hardware: the kernel runs on the engine's own Vulkan
device, so a machine without one fails while the stream is starting. It runs on
Linux and on macOS alike.

## Editing it

The shader is `HALFTONE_COMPUTE_SLANG` in `nodes/halftone_compute.py`. Save it
while `tatolab dev` runs and the stream loads again with the new text compiled.

Four edits worth making, in rising order of how much they teach:

- **Turn the screen up.** Raise `halftonecompute`'s `cell_size` from 8 to 24 and the
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
  `float3(luma)` and the picture becomes true black-and-white newsprint rather
  than a colour screen.

Two edits that fail on purpose, because each failure is the contract stating
itself:

- Rename a binding in the Slang but not in the Python. The refusal lands at
  `setup()`, before a single frame, and names the shader's own bindings.
- Bind the camera's own surface id — `frame.surface_id` in place of
  `camera_frame_landing_texture` — and skip the landing copy. The dispatch
  refuses it, because the camera's frame is a buffer, not a texture. That
  refusal is why the landing copy exists.

## Observing it

From another terminal, in this folder:

```bash
tatolab streams                  # the streams this machine's runtime holds, `main` among them
tatolab graph --stream <stream>  # nodes, ports, links and counters, as JSON
tatolab logs -f                  # the project's records, Rust and Python alike
```

The nodes are `camerasource` and `halftonecompute`.

## Knobs

Each knob is a node's config in `streams/main.graph.json`. Change it in Tato
Studio's inspector or with the runtime's `set_node_config` MCP tool, which
records the edit in the project's history and applies it to the running stream.

| Knob | Node | Effect |
| --- | --- | --- |
| `device_id` | `camerasource` | Capture device — a V4L2 path (`/dev/video2`) on Linux, an AVFoundation unique ID on macOS. Absent picks the first device found. |
| `max_width` / `max_height` | `camerasource` | Resolution cap; the negotiated format is clamped to fit. |
| `cell_size` | `halftonecompute` | Pixels across one dot cell; bigger is a coarser screen. At least 2. |
| `dot_boost` | `halftonecompute` | How far a dot's ink is lifted above the colour it sampled; above 0.0. |
| `background_level` | `halftonecompute` | The grey the paper is, 0.0 black to 1.0 white. |
