# raytracing-showcase

One scene, rendered twice, cut down the middle: **RTX OFF on the left, RTX ON
on the right**, each half labelled across the top. Same camera, same geometry,
same shading function — so everything you can see across the divider is what
casting a ray buys, and nothing else.

The camera orbits and the light orbits at a different rate, so the shadows
sweep across the floor on a period that is not the camera's. It never looks
like a loop, and it is unmistakably running rather than pre-rendered.

## The model this example teaches

**Three kernel kinds, one stream, and every one of them an object.** Each node
lives in its own module under `nodes/` and runs in its own child interpreter.
Each kernel is built once in `setup()`, where the capability is Full, and used
per frame in `process()`:

| Node | Kernel | What it does |
| --- | --- | --- |
| `RasterizedSceneRenderer` | graphics | draws the scene as triangles — ray tracing off |
| `RayTracedSceneRenderer` | ray tracing | traces it, with shadow and reflection rays |
| `SplitScreenCompositor` | compute | cuts the two halves together and labels them |

`streams/main.graph.json` links both renderers into the compositor, each
renderer at the same `width` and `height`.

**The runtime opens no windows.** The cut is the compositor's
`split_screen_frame_to_downstream`, seen in Tato Studio's viewer.

No handle string, fence, timeline or slot number ever reaches Python. The
object is the handle — and so is an acceleration structure:

```python
self.cube_structure = gpu_full_access.build_triangles_blas(vertices=…, indices=…)
self.scene_structure = gpu_full_access.build_tlas(instances=[{"blas": self.cube_structure, …}])
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
    {"stage": "ray_gen", "source": RAY_GENERATION_GLSL},
    {"stage": "miss", "source": SKY_MISS_GLSL},  # index 0
    {"stage": "miss", "source": SHADOW_MISS_GLSL},  # index 1
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

`nodes/showcase_box_scene.py` holds one table of ten boxes. Out of it come both
the acceleration-structure instance transforms and the `const` arrays the
rasterizer's vertex shader reads:

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
ahead-of-time `glslc` step: it works because a kernel's shader is a Python
string the runtime compiles at `setup()`, and there is no toolchain to install.

### The labels are generated into the shader too

Text on a GPU frame usually means a font rasterizer and a glyph atlas. Two
short labels need neither: `nodes/label_font.py` holds a 5×7 font written as
ASCII art, Python lays each label out into a bitmap, and the compositor
generates that bitmap into its GLSL as a `const uint` array. The shader unpacks
one bit per pixel, scales it to the frame, and draws white ink over a black
outline sampled at eight offsets — legible over a bright sky and a dark floor
alike.

This is also the one place the stream builds a shader out of its own config:
the kernel source is a function of `left_label` and `right_label`, not a module
constant, so changing `splitscreencompositor`'s `left_label` changes the GLSL the runtime
compiles at `setup()`. It is why the project depends on nothing but
`tatolab-stream`.

### What the rasterized half cannot do, and why

Both are honest properties of the Python graphics kernel rather than
workarounds hidden in a helper:

- **No vertex or index buffer reaches a Python node.** The vertex stage
  fabricates its positions from `gl_VertexIndex` against the generated unit
  cube, and `gl_InstanceIndex` picks which box.
- **The pass attaches colour targets only, so it has no depth test.** The
  vertex shader collapses faces the camera cannot see (a test against the
  face's own centre, which every vertex of that face agrees on, and which needs
  no winding convention to be right), and the node packs a back-to-front
  instance order into a push constant. Sorting by the distance to each box's
  centre is not exact for convex bodies in general; it is exact for *this*
  ring — one radius, well separated. A box moved off the ring is what would
  need re-checking.

### And the parts that are just the engine

Both renderers publish a `VideoFrame` — a **surface id, an extent and a
timestamp**; no pixels ride a link. The compositor binds both upstream ids
straight into its dispatch, with **no landing copy**: a kernel binding resolves
texture-backed surfaces, and a kernel output is exactly that, whichever process
acquired it. (A camera publishes buffer-backed frames, which is why
`camera-compute-kernel` lands each one with a copy first and this stream does
not.)

The compositor is also a fan-in: two producers, each in its own child
interpreter, one consumer. The two run at their own cadences, so a wake rarely
carries both ports at once — the compositor holds the newest rasterized frame,
read as a `VideoFrame` so its surface stays claimed against reuse, and
composites when the traced one lands. What keeps the halves the same picture
despite that is the section below: one clock, not one cadence.

Nothing here needs a camera, a microphone, or any other device. It needs a GPU
with `VK_KHR_ray_tracing_pipeline` (an RTX-class or RDNA2+ card) — so Linux
only, since MoltenVK exposes no ray tracing — and says so by refusing at
`setup()` rather than pretending. `tatolab dev` and `tatolab run` print the
cross-floor check's warning for the three ray-tracing constructors; the stream
starts anyway.

## Why the boxes do not move

`build_tlas` is a method on the **Full** capability, and the capability split
is on the phase axis: `setup()` holds Full, `process()` holds Limited. A
scene's geometry lives in an acceleration structure, so the scene is built once
and what animates is the camera and the light — two floats of push constant.
Moving geometry would mean rebuilding the top-level structure every frame, and
a Python node cannot reach that from `process()` at all: its limited context's
`escalate` is a refusal, because the callback's one atomic privileged scope
cannot cross the node's process boundary.

### The two halves are the same instant, and that is not free

Both renderers are separate processes reaching their first frame at their own
moment — the ray tracer's `setup()` is the longer one, with four shader modules
and two acceleration structures to build. A phase counted from each node's own
first frame would leave the two cameras degrees apart and break every box
crossing the divider. So the orbit angles are a pure function of `ctx.time`,
the monotonic clock every node shares, wrapped into a single turn before they
reach a shader as push constants:

```python
camera_azimuth, light_azimuth = orbit_azimuths_at(frame_time_ns)
```

What is left is the interval between the two frames the compositor happened to
pair — about a quarter of a degree of orbit, a pixel or two.

## Run it

With this machine's runtime running (`tatolabd`, which Tato Studio starts):

```bash
tatolab dev
```

The runtime makes the project's `.venv` on its first load: it installs `tatolab-stream` from PyPI — the authoring surface — and
needs nothing built. The runtime itself is not on pip.

`tatolab dev` follows the project's log and loads the project again on every
saved edit; `tatolab run` queues the load and returns. Ctrl-C ends `tatolab dev`
and leaves the stream loaded; `tatolab stop <stream>` stops it.

Then watch `splitscreencompositor`'s `split_screen_frame_to_downstream` in Tato
Studio's viewer.

## Editing it

Everything is a Python string the runtime compiles at `setup()`; save a file
while `tatolab dev` runs and the stream loads again with it.

- **The scene** is `SHOWCASE_BOXES` in `nodes/showcase_box_scene.py`. Add a
  box, change a colour, move the ring — both halves follow, because both read
  the same table. A new ring cube needs an entry in
  `_RING_CUBE_SIZES_AND_COLOURS`; keep them on one radius and not touching,
  which is what makes the rasterizer's back-to-front sort exact.
- **The light** is `LIGHT_ORBIT_RADIUS`, `LIGHT_HEIGHT` and
  `LIGHT_ORBIT_RADIANS_PER_SECOND` in the same file. Lowering the light
  lengthens the shadows; changing its rate changes how fast they sweep.
- **What the floor reflects** is `FLOOR_MIRROR_STRENGTH`. Set it to `0.0` and
  the two halves differ only by their shadows.
- **The labels** are `splitscreencompositor`'s `left_label` / `right_label`. The font
  covers A-Z, 0-9, space, hyphen and full stop, and a character it has no glyph
  for is refused by name rather than drawn as a blank — so a typo says so
  instead of leaving a hole.

One edit worth making on purpose, because it fails in an instructive way:
rename a binding in the ray-generation GLSL but not in `DECLARED_BINDINGS`.
The declaration is asserted against the shaders' own reflection when the kernel
is built, so the refusal lands at `setup()`, before a single frame.

## Observing it

From another terminal, in this folder:

```bash
tatolab streams                  # the streams this machine's runtime holds, `main` among them
tatolab graph --stream <stream>  # nodes, ports, links and counters, as JSON
tatolab logs -f                  # the project's records, Rust and Python alike
```

The nodes are `rasterizedscenerenderer`, `raytracedscenerenderer` and
`splitscreencompositor`.

## Knobs

Each knob is a node's config in `streams/main.graph.json`. Change it in Tato
Studio's inspector or with the runtime's `set_node_config` MCP tool, which
records the edit in the project's history and applies it to the running stream.

| Knob | Node | Effect |
| --- | --- | --- |
| `width` / `height` | `rasterizedscenerenderer`, `raytracedscenerenderer` | The extent both halves render at, and so the extent of the cut. Keep the two equal. |
| `split_fraction` | `splitscreencompositor` | Where the cut falls; 0.0 is all ray traced, 1.0 all rasterized. |
| `left_label` / `right_label` | `splitscreencompositor` | The text over each half; generated into the compositor's kernel at `setup()`. |
