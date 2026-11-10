# camera-python-effects

Camera → six Python nodes, seen in Tato Studio's viewer. A cyberpunk broadcast
look built out of Slang kernels, MediaPipe pose tracking, a ModernGL avatar and
a skia overlay, all authored in Python.

```
CameraSource ─┬─ CameraFrameToTexture ─ CyberpunkGlitch ─ CrtFilmGrain ─┐
              │                                                         ▼
              └─ CyberpunkAvatar ──────────────────────▶ BreakingNewsCompositor
                                                                        ▲
                                      NeonOverlaySource ────────────────┘
```

## The model this example teaches

**A stream with many nodes is still one graph asset.** `streams/main.graph.json`
holds the seven nodes, their config and their links; the nodes, the code they
share and their shaders live in an ordinary installable package,
`src/camera_python_effects/`.

- `CameraSource` comes from the **`tatolab-camera` module**, a dependency in
  `pyproject.toml`. It runs in its own child process, and its per-frame path is
  the module's Rust, never Python.
- The six between them are this project's own `@node.process` classes, one module each
  under `camera_python_effects/nodes/`. **Every one runs in its own child
  process with its own interpreter**, so pose tracking at whatever rate the CPU
  allows never slows the effect chain down.
- A node is named by its class's import path —
  `camera_python_effects.nodes.crt_film_grain:CrtFilmGrain` — and the child
  imports it by that name from the stream's venv. That is why the nodes are a
  package: the runtime installs it, editable, into the project's `.venv`, and the shared
  modules (`avatar_rig`, `gpu_surface_conventions`, the shaders) come along
  under one import root. The `tatolab new` scaffold's `nodes/` directory is the
  same idea for a project small enough to need no package.
- **The runtime opens no windows.** Any port of the running stream, the
  composited picture among them, is seen in Tato Studio's viewer.

## Run it

With this machine's runtime running (`tatolabd`, which Tato Studio starts):

```bash
tatolab dev
```

The runtime makes the project's `.venv` on its first load: it installs `tatolab-stream` from PyPI — the authoring surface — beside
numpy, skia-python, MediaPipe, ModernGL and this project's own package, and
builds `tatolab-camera` from this checkout, as an editable path dependency
(`[tool.uv.sources]` in `pyproject.toml`); the module is a maturin project,
so the build needs a Rust toolchain. The runtime itself is not on pip.

Then watch `breakingnewscompositor`'s `video_to_downstream` in Tato Studio's
viewer.

Runs on Linux with an NVIDIA GPU and a V4L2 camera, and on macOS arm64. The
first run downloads MediaPipe's pose model (`pose_landmarker_lite.task`,
~5.5 MB) into `~/.cache/camera-python-effects`; the shaders need no toolchain —
the runtime compiles them when each node's `setup()` builds its kernel.

`tatolab dev` follows the project's log and loads the project again on every
saved edit; `tatolab start` queues the load and returns, and `tatolab reload` re-reads a project that already runs. Ctrl-C ends `tatolab dev`
and leaves the stream loaded; `tatolab stop` stops it.

## The pieces

| Node | What it does |
| --- | --- |
| `CameraFrameToTexture` | Copies the camera frame into a texture, device to device, with the engine's copy |
| `CyberpunkGlitch` | Teal/magenta grade with intermittent glitch flashes |
| `CrtFilmGrain` | Barrel curve, scanlines, aberration, vignette, 24 fps grain |
| `CyberpunkAvatar` | MediaPipe 3D pose driving a procedural android on a neon stage |
| `NeonOverlaySource` | Lower third, ticker and watermark, drawn with skia, redrawn every 33 ms |
| `BreakingNewsCompositor` | Blends the three layers with a sliding picture-in-picture |

The effects live in `src/camera_python_effects/shaders/` as ordinary `.slang`
files. The `dev` watch follows `.py` files only, so after editing a shader save
any `.py` file (or restart `tatolab dev`) to load it.

Each node's config is a dataclass beside it with ordinary Python defaults,
named by its `__init__(self, config: ...)` annotation —
setting `crtfilmgrain`'s `barrel_curve` to `0.0` flattens the
tube. `CrtFilmGrain` takes every CRT parameter that way; `CyberpunkAvatar`
takes `scene_width`, `scene_height`, `detection_confidence` and
`pose_model_path` (a local `.task` file, which skips the download).

The avatar is three third-party worlds in one child process — numpy reads the
camera frame's host side through `numpy.from_dlpack(frame, device="cpu")`,
MediaPipe lifts a 3D pose out of it on the CPU, ModernGL renders the posed
android on its own stage — and the engine sees only a frame in and a frame
out. The pose itself rides `pose_to_downstream`, a Records port carrying
`timestamp_ns`, `provenance`, `joints` and `visibility`, stamped with the camera
frame it was lifted from. If tracking cannot run, the android holds its idle sway, the warning is
logged once, and the other five layers carry on.

## Two things worth knowing before you write a node here

**A camera frame is not a texture.** `CameraSource` publishes buffer-backed
frames and a kernel binding resolves texture-backed surfaces only, so a draw
handed a camera surface id is refused by name. That is what
`CameraFrameToTexture` is for: `ctx.gpu_limited_access.copy_surface_to_surface`
copies the frame into a texture this node owns, device to device, with no GPU
array package involved. The `ImageFrame` its `Input(Image)` port hands back
holds its surface still for the length of the copy.

It is also the *only* reason the chain opens that way. A camera frame is
writable in place — `with frame.writable() as t:` hands a third-party GPU
package a device tensor over the frame's own pixels, and the edit publishes
back with no new surface at all. That is the cheaper door whenever an effect
does not need a shader; these effects do.

**Output textures come from a ring, not a per-frame acquire.** Every node
here publishes from a `tatolab.stream.NodeOutputTextureRing`: each frame asks
the engine's node output pool for its next slot, and a slot a downstream node
still holds is skipped, never rewritten — so the compositor holding the
avatar's last stage frame keeps seeing its pixels. A one-off
`acquire_texture` is held only until its handle closes, which is the wrong
lifetime for a surface id a consumer one process away was handed a
millisecond earlier.

## Tests

```bash
uv run pytest
```

No camera, no runtime, no graph: they cover the push-constant blocks each node
packs against the ones its shader declares, the avatar rig and smoother, the
LeRobot join and format, and the skia overlay. Shaders are compiled by
`slangc` where one is installed, skipped where not; the avatar's GL
smoke test is skipped where no GL context can be created.

## The physical-AI story: this graph is a data-collection rig

Add the recorder (see Knobs) with `dataset_root` set to an absolute directory
and the same graph records imitation-learning episodes while it runs.

One more node, `LeRobotRecorder`, in one more child process, reads three ports
that all descend from the same camera frame — the raw view, the composited
output, and the avatar's solved pose — joins them on the frame's own monotonic
timestamp, and writes episodes through LeRobot's official writer. Per frame:
two video streams, the 51-dim pose state, per-joint visibility, the true
capture stamp, and the pose as `action` — the target a retargeting consumer
servos toward.

The dataset loads in LeRobot's own tooling (`LeRobotDataset(...,
video_backend="pyav")`), visualizes with their viewer, and pushes to the Hub
with `dataset.push_to_hub()`. Episode saves video-encode for whole seconds —
and stall exactly one process. The camera keeps capturing, the effects keep
running, and the composite keeps publishing.

## Observing it

From another terminal, in this folder:

```bash
tatolab streams                  # the streams this machine's runtime holds, `main` among them
tatolab graph --stream <stream>  # nodes, ports, links and counters, as JSON
tatolab logs -f                  # the project's records, Rust and Python alike
```

The nodes are `camerasource`, `cameraframetotexture`, `cyberpunkglitch`,
`crtfilmgrain`, `cyberpunkavatar`, `neonoverlaysource` and
`breakingnewscompositor`.

## Knobs

Each knob is a node's config in `streams/main.graph.json`. Change it in Tato
Studio's inspector or with the runtime's `set_node_config` MCP tool, which
records the edit in the project's history and applies it to the running stream.

| Knob | Node | Effect |
| --- | --- | --- |
| `device_id` | `camerasource` | Capture device — a V4L2 path (`/dev/video2`) on Linux, an AVFoundation unique ID on macOS. Absent picks the first device found. |
| `max_width` / `max_height` | `camerasource` | The camera's resolution cap. 1920×1080. |
| `width` / `height` | `neonoverlaysource` | The overlay's canvas size. 1920×1080. |

The LeRobot recorder is not in the graph. To record episodes, `uv sync --extra
lerobot`, add a `camera_python_effects.nodes.lerobot_recorder:LeRobotRecorder`
node with `dataset_root` set, and link `camerasource`'s `video` to its
`video_from_camera`, `cyberpunkavatar`'s `pose_to_downstream` to its
`pose_from_avatar`, and `breakingnewscompositor`'s `video_to_downstream` to its
`stylized_from_compositor`.
