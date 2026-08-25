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

Two dials worth knowing: `CrtFilmGrain` takes every CRT parameter as config
(`rt.add(CrtFilmGrain, config={"barrel_curve": 0.0})` flattens the tube), and
`PoseSkeletonOverlay` takes `pose_model` and `keypoint_confidence_floor`.

## Why the camera frame goes through `CameraFrameToTexture`

`CameraSource` publishes buffer-backed frames, and a kernel binding resolves
texture-backed surfaces only — a draw handed a buffer-backed surface id is
refused by name. So the chain opens by reading the frame as a GPU tensor and
copying it into a texture this app acquired. cupy does nothing but that copy;
any DLPack-speaking GPU package would serve.

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
