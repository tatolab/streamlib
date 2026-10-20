# camera-python-effects

Camera → six Python nodes → H.264, watched in a browser. A cyberpunk broadcast
look built out of GLSL kernels, MediaPipe pose tracking, a ModernGL avatar and
a skia overlay, all authored in Python.

```
CameraSource ─┬─ CameraFrameToTexture ─ CyberpunkGlitch ─ CrtFilmGrain ─┐
              │                                                         ▼
              └─ CyberpunkAvatar ──────────────────────▶ BreakingNewsCompositor ─ H264Encoder
                                                                        ▲
                                      NeonOverlaySource ────────────────┘
```

## The model this example teaches

**A stream with many nodes is still a normal Python project.** `stream.py`
holds the one `@stream` function; the nodes, the code they share and their
shaders live in an ordinary installable package, `src/camera_python_effects/`:

```python
@stream
def main(stream_builder: StreamBuilder) -> None:
    camera = stream_builder.add(CameraSource, config=camera_config)
    camera_texture = stream_builder.add(CameraFrameToTexture)
    ...
    compositor = stream_builder.add(BreakingNewsCompositor)
    encoder = stream_builder.add(H264Encoder)
    ...
    stream_builder.connect(
        compositor.output("video_to_downstream"), encoder.input("video")
    )
    stream_builder.expose(encoder.output("encoded_video"), Exposure.PRIVATE)
```

- `CameraSource` and `H264Encoder` are **native built-ins**: they ship inside
  the runtime, and their per-frame paths never enter a Python interpreter.
- The six between them are this project's own `@node` classes, one module each
  under `camera_python_effects/nodes/`. **Every one runs in its own child
  process with its own interpreter**, so pose tracking at whatever rate the CPU
  allows never slows the effect chain down.
- A node is named by its class's import path —
  `camera_python_effects.nodes.crt_film_grain:CrtFilmGrain` — and the child
  imports it by that name from the stream's venv. That is why the nodes are a
  package: `uv sync` installs it, editable, into `.venv`, and the shared
  modules (`avatar_rig`, `gpu_surface_conventions`, the shaders) come along
  under one import root. The `tatolab new` scaffold's `nodes/` directory is the
  same idea for a project small enough to need no package.
- **The runtime opens no windows.** The composited picture goes through
  `H264Encoder`, and `expose` makes its output a track at this machine's local
  relay, which a browser plays and any other stream or MoQ client reads.

## Run it

With this machine's runtime running (`tatolabd`, which the installer starts):

```bash
uv sync
tatolab dev
```

`uv sync` installs `tatolab-stream` from PyPI — the authoring surface, and the
only Tatolab package a stream's venv holds — beside numpy, skia-python,
MediaPipe, ModernGL and this project's own package. The runtime itself is not
on pip.

Then open the stream in a browser:

```text
http://127.0.0.1:4446/watch/<machine>/main
```

`http://127.0.0.1:4446/` lists this machine's name and every stream's exposed
ports.

Runs on Linux with an NVIDIA GPU and a V4L2 camera, and on macOS arm64. The
first run downloads MediaPipe's pose model (`pose_landmarker_lite.task`,
~5.5 MB) into `~/.cache/camera-python-effects`; the shaders need no toolchain —
the runtime compiles them when each node's `setup()` builds its kernel.

Use `tatolab dev` while editing — it loads the stream again on every saved
`.py` or `pyproject.toml` edit — and `tatolab run` otherwise. Both keep the
stream as long as the command runs; Ctrl-C unloads it.

## The pieces

| Node | What it does |
| --- | --- |
| `CameraFrameToTexture` | Copies the camera frame into a texture, device to device, with the engine's copy |
| `CyberpunkGlitch` | Teal/magenta grade with intermittent glitch flashes |
| `CrtFilmGrain` | Barrel curve, scanlines, aberration, vignette, 24 fps grain |
| `CyberpunkAvatar` | MediaPipe 3D pose driving a procedural android on a neon stage |
| `NeonOverlaySource` | Lower third, ticker and watermark, drawn with skia, redrawn every 33 ms |
| `BreakingNewsCompositor` | Blends the three layers with a sliding picture-in-picture |

The effects live in `src/camera_python_effects/shaders/` as ordinary `.frag`
files. The `dev` watch follows `.py` files only, so after editing a shader save
any `.py` file (or restart `tatolab dev`) to load it.

Each node's config is a dataclass beside it with ordinary Python defaults,
named by its `__init__(self, config: ...)` annotation —
`stream_builder.add(CrtFilmGrain, config={"barrel_curve": 0.0})` flattens the
tube. `CrtFilmGrain` takes every CRT parameter that way; `CyberpunkAvatar`
takes `scene_width`, `scene_height`, `detection_confidence` and
`pose_model_path` (a local `.task` file, which skips the download).

The avatar is three third-party worlds in one child process — numpy reads the
camera frame's host side through `numpy.from_dlpack(frame, device="cpu")`,
MediaPipe lifts a 3D pose out of it on the CPU, ModernGL renders the posed
android on its own stage — and the engine sees only a frame in and a frame
out. If tracking cannot run, the android holds its idle sway, the warning is
logged once, and the other five layers carry on.

## Two things worth knowing before you write a node here

**A camera frame is not a texture.** `CameraSource` publishes buffer-backed
frames and a kernel binding resolves texture-backed surfaces only, so a draw
handed a camera surface id is refused by name. That is what
`CameraFrameToTexture` is for: `ctx.gpu_limited_access.copy_surface_to_surface`
copies the frame into a texture this node owns, device to device, with no GPU
array package involved. The frame read as `into=VideoFrame` holds its surface
still for the length of the copy.

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
`glslangValidator` where one is installed, skipped where not; the avatar's GL
smoke test is skipped where no GL context can be created.

## The physical-AI story: this graph is a data-collection rig

Set `LEROBOT_DATASET_ROOT` at the top of `stream.py` to an absolute directory
and the same graph records imitation-learning episodes while it runs:

```bash
uv sync --extra lerobot
tatolab dev
```

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
running, and the encoder keeps publishing.

## Observing it

From another terminal:

```bash
tatolab streams                 # the streams this machine's runtime holds
tatolab graph --stream main     # nodes, ports, links, exposed ports and counters, as JSON
tatolab logs --stream main -f   # the stream's records, Rust and Python alike
```

A node is named after its class, lowercased — `camerasource`,
`breakingnewscompositor`, `h264encoder` — unless `add` passes `name=`. An
exposed port is the track `<node>/<port>` of the broadcast
`<machine>/<stream>.hang` at the machine's local relay, so any MoQ client reads
it; this repository's `tatolab-read-track` writes a track's objects to files:

```bash
cargo run -p tatolab-read-track -- http://127.0.0.1:4443 <machine>/main.hang \
    h264encoder/encoded_video --count 30 --out /tmp/camera-python-effects
```

Each file is one H.264 access unit, so the files concatenated in
`objects.jsonl` order are an H.264 stream ffmpeg plays.

Every other port is internal to the stream; `tatolab expose main <node>
<port>` makes one a track too. A video port's objects are frame records, not
pictures — `surface_id`, `width`, `height` and `timestamp_ns`, naming
engine-owned GPU memory that means nothing outside the runtime — which is why
the picture is shared through the encoder. The avatar's pose is data all the
way through: `tatolab expose main cyberpunkavatar pose_to_downstream` and each
object is one msgpack pose record — the frame's stamp, `detected` / `held` /
`idle`, and the settled joints.

## Knobs

Everything is literal config in `stream.py` — edit it and save while
`tatolab dev` runs.

| Knob | Where | Effect |
| --- | --- | --- |
| `CAMERA_DEVICE_ID` | top of `stream.py` | Capture device — a V4L2 path (`/dev/video2`) on Linux, an AVFoundation unique ID on macOS. `None` picks the first device found. |
| `LEROBOT_DATASET_ROOT` | top of `stream.py` | Directory the LeRobot recorder writes episodes into, after `uv sync --extra lerobot`. `None` leaves the recorder out of the graph. |
| `FRAME_WIDTH` / `FRAME_HEIGHT` | top of `stream.py` | The camera's resolution cap and the overlay's canvas size. Default 1920×1080. |
| `Exposure.PRIVATE` | `expose` | `Exposure.PUBLIC` shares the track off this machine, at every relay the runtime has joined. |
