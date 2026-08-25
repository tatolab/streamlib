# camera-python-effects

Camera → six Python processors → display. A cyberpunk broadcast look built out
of GLSL kernels, GPU pose detection and a skia overlay, all authored in Python.

```
CameraSource ─┬─ CameraFrameToTexture ─ CyberpunkGlitch ─ CrtFilmGrain ─┐
              │                                                         ▼
              └─ PoseSkeletonOverlay ──────────────────▶ BreakingNewsCompositor ─ DisplayWindow
                                                                        ▲
                                      NeonOverlaySource ────────────────┘
```

`CameraSource` and `DisplayWindow` are native built-ins inside the wheel. The
six between them are this app's own classes, and each runs in its own child
process with its own interpreter — pose detection at a third of the camera's
rate never slows the effect chain down.

## Run it

```bash
uv venv --python 3.12 && uv sync
source .venv/bin/activate
streamlib dev
```

Needs Linux, an NVIDIA GPU, and a V4L2 camera. The first run downloads the pose
model (`yolov8n-pose.pt`, ~6 MB) and compiles nothing — the shaders are built by
the engine at startup, and the GLSL compiler is in the wheel.

`STREAMLIB_CAMERA_DEVICE=/dev/video2 streamlib dev` picks a specific capture
node; unset takes the first one found.

To run against a checkout rather than a release, install that checkout's wheel
over the released one:

```bash
uv pip install maturin
maturin develop --manifest-path ../../sdk/streamlib-python-wheel/Cargo.toml
```

## The pieces

| Processor | What it does |
| --- | --- |
| `CameraFrameToTexture` | Copies the camera frame into a texture, device to device, with cupy |
| `CyberpunkGlitch` | Teal/magenta grade with intermittent glitch flashes |
| `CrtFilmGrain` | Barrel curve, scanlines, aberration, vignette, 24 fps grain |
| `PoseSkeletonOverlay` | YOLOv8 pose over a CUDA tensor, skeleton drawn as an SDF |
| `NeonOverlaySource` | Lower third and watermark, drawn with skia |
| `BreakingNewsCompositor` | Blends the three layers with a sliding picture-in-picture |

Effects live in `src/camera_python_effects/shaders/` as ordinary `.frag` files.
Edit one and re-run `streamlib dev` — that is the whole edit loop.

Config is constructor keyword arguments with ordinary Python defaults —
`rt.add(CrtFilmGrain, config={"barrel_curve": 0.0})` flattens the tube.
`CrtFilmGrain` takes every CRT parameter that way; `PoseSkeletonOverlay` takes
`pose_model`, `keypoint_confidence_floor` and `skeleton_scale`.

If pose detection cannot run — a CUDA stack that does not agree with itself is
the usual cause — the skeleton layer stays empty, the warning is logged once,
and the other five layers carry on.

## Two things worth knowing before you write a processor here

**A camera frame is not a texture.** `CameraSource` publishes buffer-backed
frames and a kernel binding resolves texture-backed surfaces only, so a draw
handed a camera surface id is refused by name. That is what
`CameraFrameToTexture` is for: it reads the frame as a GPU tensor and copies it
into a texture this app owns, device to device. cupy does nothing but that
copy; any DLPack-speaking GPU package would serve.

It is also the *only* reason the chain opens that way. A camera frame is
writable in place — `with frame.writable() as t:` hands a third-party GPU
package a device tensor over the frame's own pixels, and the edit publishes
back with no new surface at all. That is the cheaper door whenever an effect
does not need a shader; these effects do.

**Output textures are allocated once, not per frame.** Every processor here
publishes from a `streamlib.ProcessorOutputTextureRing` — two slots taken on
the first frame and rotated after that, the cross-process sibling of the
engine's own `TextureRing` (`docs/architecture/texture-ring.md`). Acquiring a
texture per frame instead costs 7.2 ms against 2.3 ms at 1080p, and hands you
a lifetime bug: an acquired texture's registration *is* its handle, so a
producer that lets go at the end of `process()` unregisters the surface id a
consumer one process away was handed a millisecond earlier.

## Tests

```bash
uv run pytest
```

No GPU, no camera, no graph: they cover the push-constant blocks each processor
packs against the ones its shader declares, the pose keypoint packing, and the
skia overlay. Shaders are compiled by `glslangValidator` where one is installed,
skipped where not.

## Observing it

The running app is a node. From another terminal:

```bash
streamlib nodes           # the live nodes on this machine
streamlib graph           # this node's processors, ports and links, as JSON
streamlib logs --follow   # the node's JSONL log
```
