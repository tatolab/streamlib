# raytracing-showcase

One scene, rendered twice, cut down the middle: **RTX OFF on the left, RTX ON
on the right**, each half labelled across the top. Same camera, same geometry,
same shading function — so everything you can see across the divider is what
casting a ray buys, and nothing else.

The camera orbits and the light orbits at a different rate, so the shadows
sweep across the floor on a period that is not the camera's. It never looks
like a loop, and it is unmistakably running rather than pre-rendered.

## The model this example teaches

**Three kernel kinds, one app, and every one of them an object.** Each is
built once in `setup()`, where the capability is Full, and used per frame in
`process()`:

| Processor | Kernel | What it does |
| --- | --- | --- |
| `RasterizedSceneRenderer` | graphics | draws the scene as triangles — ray tracing off |
| `RayTracedSceneRenderer` | ray tracing | traces it, with shadow and reflection rays |
| `SplitScreenCompositor` | compute | cuts the two halves together and labels them |

No handle string, fence, timeline or slot number ever reaches Python. The
object is the handle — and so is an acceleration structure:

```python
self.cube_structure = gpu.build_triangles_blas(vertices=…, indices=…)
self.scene_structure = gpu.build_tlas(instances=[{"blas": self.cube_structure, …}])
…
self.trace_kernel.trace(
    bindings={"scene_structure": self.scene_structure, "traced_frame": texture},
    grid=(width, height, 1),
    push_constants=…,
)
```

### The shader binding table is where a ray tracer's control flow lives

The ray-tracing kernel is built from **four shader modules and four groups**.
Two of those modules are miss shaders, and that is the whole trick:

```python
RAY_TRACING_STAGES = [
    {"stage": "ray_gen",     "source": RAY_GENERATION_GLSL},
    {"stage": "miss",        "source": SKY_MISS_GLSL},      # index 0
    {"stage": "miss",        "source": SHADOW_MISS_GLSL},   # index 1
    {"stage": "closest_hit", "source": CLOSEST_HIT_GLSL},
]
```

A group names its modules **by index into `stages`**, because two modules can
fill the same stage. A primary ray that hits nothing runs miss 0 and gets the
sky. A shadow ray — cast from the hit shader, with
`gl_RayFlagsTerminateOnFirstHitEXT | gl_RayFlagsSkipClosestHitShaderEXT`, at
miss index 1 — runs no hit shader at all, so *arriving* at miss 1 is the only
thing that can happen to it, and that means nothing was in the way:

```glsl
lit_fraction = 0.0;                        // a hit leaves it dark
traceRayEXT(scene_structure, …, SHADOW_MISS_INDEX, …, 1);
```

That is the question a fragment shader cannot ask, and it is why the right
half has shadows.

### The reflection bounces from ray generation, not from the hit shader

A shader may declare only one incoming payload, so a hit shader cannot both
receive at a payload location and trace at it. The mirror floor therefore
reports its reflectivity back to `main()` in ray generation, which fires the
second ray itself. Two consequences worth knowing:

- `max_recursion_depth` stays at **2** — one hit shader casting one shadow ray
  — rather than the 3 a recursive reflection would need.
- The reflected ray runs the same hit shader, so it casts its own shadow ray:
  the reflections come back with shadows already in them.

### The scene is generated *into* the GLSL

`processors/showcase_box_scene.py` holds one table of ten boxes. Out of it
come both the acceleration-structure instance transforms and the `const`
arrays the rasterizer's vertex shader reads:

```python
SHOWCASE_SCENE_GLSL = (
    _SHOWCASE_SCENE_DEFINES
    + _glsl_vec3_array("SHOWCASE_BOX_CENTRES", [box.centre for box in SHOWCASE_BOXES])
    + …
)
```

The two renderers therefore cannot drift into drawing different scenes, they
share one camera function and one lighting function, and the sky runs
continuously across the divider because both sides ask the same
`sky_colour_towards` for the same view ray. None of that is possible with an
ahead-of-time `glslc` step: it works because a StreamLib shader is a Python
string the engine compiles at `setup()`, and there is no toolchain to install.

### The labels are generated into the shader too

Text on a GPU frame usually means a font rasterizer and a glyph atlas. Two
short labels need neither: `processors/label_font.py` holds a 5×7 font written
as ASCII art, Python lays each label out into a bitmap, and the compositor
generates that bitmap into its GLSL as a `const uint` array. The shader unpacks
one bit per pixel, scales it to the frame, and draws white ink over a black
outline sampled at eight offsets — legible over a bright sky and a dark floor
alike.

This is also the one place the app builds a shader out of its own config: the
kernel source is a function of `left_label` and `right_label`, not a module
constant, so changing a label in `app.py` changes the GLSL the engine compiles
at `setup()`. It is why the app still depends on nothing but the wheel.

### What the rasterized half cannot do, and why

Both are honest properties of the Python graphics surface rather than
workarounds hidden in a helper:

- **No vertex or index buffer reaches a Python processor.** The vertex stage
  fabricates its positions from `gl_VertexIndex` against the generated unit
  cube, and `gl_InstanceIndex` picks which box.
- **The pass has no depth attachment** — an unbuilt engine capability,
  reachable from neither language. So the vertex shader collapses faces the
  camera cannot see (a test against the face's own centre, which every vertex
  of that face agrees on, and which needs no winding convention to be right),
  and the app packs a back-to-front instance order into a push constant. That
  ordering is *exact* rather than approximate here, because the boxes are
  convex and none of them touches another.

### And the parts that are just the engine

Both renderers publish a **surface id, an extent and a timestamp** — no
pixels ride a link. The compositor binds both upstream ids straight into its
dispatch, with **no landing copy**: a kernel binding resolves texture-backed
surfaces, and a kernel output is exactly that, whichever process acquired it.
(A camera is the other case — it publishes buffer-backed frames, which is why
`camera-compute-kernel` needs a copy and this app does not.)

The compositor is also the tree's fan-in example: two producers, each in its
own child interpreter, one consumer. Because both renderers run their own
clocks, a reactive wake rarely carries both ports at once — so the compositor
holds the newest rasterized frame and composites when the traced one lands.

Nothing here needs a camera, a microphone, or any other device. It needs a GPU
with `VK_KHR_ray_tracing_pipeline` (an RTX-class or RDNA2+ card), and says so
by refusing at `setup()` rather than pretending.

## Why the boxes do not move

`build_tlas` is a method on the **Full** capability, and the capability
typestate is on the phase axis: `setup()` holds Full, `process()` holds
Limited. A scene's geometry lives in an acceleration structure, so the scene is
built once and what animates is the camera and the light — two floats of push
constant. Moving geometry would mean rebuilding the top-level structure every
frame, and a Python processor cannot reach that from `process()` at all: its
limited context's `escalate` is a refusal, because the callback's one atomic
privileged scope cannot cross the helper's process boundary. (A Rust processor
in the app process can escalate, at the cost of a device-idle wait per frame.)

### The two halves are the same instant, and that is not free

Both renderers are separate processes reaching their first frame at their own
moment — the ray tracer's `setup()` is a couple of hundred milliseconds longer,
with four shader modules and two acceleration structures to build. A phase
counted from each processor's own first frame would leave the two cameras
degrees apart and break every box crossing the divider. So the orbit angles are
a pure function of `ctx.time`, the one machine-monotonic clock every processor
shares, wrapped into a single turn before they reach a shader as push constants:

```python
camera_azimuth, light_azimuth = orbit_azimuths_at(ctx.time)
```

What is left is the interval between the two frames the compositor happened to
pair — about a quarter of a degree of orbit, a pixel or two.

## Run it

```bash
uv venv --python 3.12 && uv sync
source .venv/bin/activate
streamlib run
```

A window opens with the split view. Ctrl-C stops it.

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

## Editing it

Everything is a Python string the engine compiles at startup, and a warm
restart is sub-second. That is the edit loop; there is no reload-on-save and
nothing is cached against you.

- **The scene** is `SHOWCASE_BOXES` in `processors/showcase_box_scene.py`.
  Add a box, change a colour, move the ring — both halves follow, because both
  read the same table. A new ring cube needs an entry in
  `_RING_CUBE_SIZES_AND_COLOURS`; keep them on one radius and not touching,
  which is what makes the rasterizer's back-to-front sort exact.
- **The light** is `LIGHT_ORBIT_RADIUS`, `LIGHT_HEIGHT` and
  `LIGHT_ORBIT_RADIANS_PER_SECOND` in the same file. Lowering the light
  lengthens the shadows; changing its rate changes how fast they sweep.
- **What the floor reflects** is `FLOOR_MIRROR_STRENGTH`. Set it to `0.0` and
  the two halves differ only by their shadows.
- **The labels** are `left_label` / `right_label` in `app.py`. The font covers
  A-Z, 0-9, space, hyphen and full stop, and a character it has no glyph for is
  refused by name rather than drawn as a blank — so a typo says so instead of
  leaving a hole.

Two edits worth making on purpose, because each fails in an instructive way:

- Rename a binding in the ray-generation GLSL but not in `DECLARED_BINDINGS`.
  The refusal lands at `setup()`, before a single frame, and names the
  shaders' own bindings.
- Declare the scene structure for a stage the kernel has no module for —
  `("acceleration_structure", ["any_hit"])`. Refused at construction, because
  a trace never revisits which stage reads what.

## Observing it

From another terminal with this venv activated, while it runs:

```bash
streamlib nodes                       # the live nodes on this machine
streamlib graph                       # processors, ports, links and their states
streamlib logs --list                 # the runtimes that have a log file
streamlib logs <RUNTIME_ID> --follow  # tail one of them, like `tail -F`
```

To see each half on its own rather than only the cut, exchange the surface ids
one of the three channels publishes for PNGs. The channel name is the source
processor's id, lowercased, joined to its output port — read it out of `graph`
rather than spelling it by hand:

```bash
CHANNEL=$(streamlib graph | python3 -c '
import json, sys
for node in json.load(sys.stdin)["nodes"]:
    for port in node["ports"]["outputs"]:
        if port["name"] == "ray_traced_frame_to_downstream":
            print((node["id"] + "/" + port["name"]).lower())')
streamlib exchange --channel "$CHANNEL" --count 3 --out /tmp/traced
```

`streamlib tap "$CHANNEL"` on the same channel shows the bag itself: a surface
id, an extent, a timestamp, and no pixels. `exchange` is the door that turns
one of those ids into a full-resolution PNG — the same door any API consumer
uses, with no window in the graph and no display server in the path. Swap the
port name for `rasterized_frame_to_downstream` or
`split_screen_frame_to_downstream` to see the other two.

Exchanging a few frames several seconds apart is also how you check the light
is really orbiting rather than the picture being static.

`graph` renders per-link drop counts under a node's `metrics`, but only for a
destination that lives in the app process — here, the window. Every Python
processor is helper-placed and counts its losses inside its own child; its node
carries no `metrics` key at all rather than a zero the parent cannot stand
behind. So a quiet `graph` is not yet proof that nothing was dropped.

## Runtime knobs

Everything is literal config in `app.py` — edit it and re-run.

| Knob | Where | What it does |
| --- | --- | --- |
| `split_fraction` | `rt.add(SplitScreenCompositor, config=…)` | where the cut falls; 0.0 is all ray traced, 1.0 all rasterized |
| `left_label` / `right_label` | `rt.add(SplitScreenCompositor, config=…)` | the text over each half; baked into the kernel at `setup()` |
| `width` / `height` | `rt.add(…SceneRenderer, config=…)` | the resolution both halves render at |
| `title` / `scaling` | `rt.add(DisplayWindow, config=…)` | window geometry; `scaling` is `fit` / `fill` / `stretch` |
