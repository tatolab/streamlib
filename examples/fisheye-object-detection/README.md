# fisheye-object-detection

Camera → a fisheye lens → rectify → detect → two windows. The pipeline a
monocular drone flies with, running on a desk, with the distorted and the
corrected view open side by side.

A wide-FOV lens is what a small aircraft carries — one camera, and as much of
the world in it as the glass can bend in. The bending is why perception stacks
on such a camera rectify before they detect: a model trained on rectilinear
photographs is being shown something its training set does not contain, most
of all at the periphery, which on a drone is where the obstacle you have not
hit yet lives. So the frame is rectified on the GPU first, and the model never
sees the lens at all.

That is the motivation, and this app demonstrates the *pipeline* rather than
measuring the benefit — on any single scene the detection count moves in both
directions, and a claim about detection quality needs a dataset and a metric,
not a webcam. What it does show is that the correction is real, and you can
check that yourself with two `exchange` calls (see *Observing it*): against a
static test pattern the rectified frame scores about **36 dB** PSNR on the
camera frame it came from, where the distorted frame in the other window
scores about **15 dB**.

There is no fisheye lens on your desk, so `SyntheticFisheyeLens` applies the
distortion a real one would have baked in. Everything downstream of it is the
real thing.

## The model this example teaches

### One scope, both directions

The whole hand-off between the engine and a third-party GPU stack is these
four lines, in `processors/undistorting_object_detector.py`:

```python
with undistorted_frame_texture.as_device_tensor() as rectified_pixels:
    rectified_frame = torch.from_dlpack(rectified_pixels)
    detections = self._detections_in(rectified_frame)
    self._draw_boxes_into(rectified_frame, detections)
```

Entering the scope hands the engine's texture out as a linear DLPack view, so
`torch.from_dlpack` is the entire read and the tensor it returns is
GPU-resident. That same tensor is the write door: the boxes drawn into it are
blitted back into the texture when the scope closes, ordered by the engine
ahead of its own next read of that texture. No fence, no timeline, no
`torch.cuda.synchronize()` — none of that vocabulary reaches Python, and the
pixels do not travel through the host to get read or to get written.

Leaving the scope by a raise is the other half of the contract: the write is
discarded and the texture keeps the frame it already held, because half a
drawn frame blitted back publishes something that surfaces as corrupt pixels
downstream instead of as the exception that actually happened.

### A published surface id binds straight into a dispatch

The rectifier's kernel reads the lens's output without copying it:

```python
self.fisheye_rectify_kernel.dispatch(
    bindings={
        "fisheye_frame": frame.surface_id,      # the lens's own kernel output
        "undistorted_frame": undistorted_frame_texture,
    },
    ...
)
```

`frame.surface_id` is a texture another **helper process** wrote — the lens is
a Python processor with its own interpreter, like this one — and a dispatch
binding resolves it through the surface-share service exactly as it resolves a
texture from this process's own ring. Nothing is copied and nothing crosses
the host to make that work.

That is the interesting half of the contrast with the app's first hop. There,
`SyntheticFisheyeLens` reads `CameraSource` and *cannot* bind what it gets: a
camera publishes a **buffer-backed** frame, and a dispatch binds
**texture-backed** surfaces only, so each frame is landed in a texture the
lens owns first:

```python
with camera_frame_landing_texture.as_device_tensor() as writable_texture:
    torch.from_dlpack(writable_texture)[...] = torch.from_dlpack(frame)
```

Same device-to-device door as above, used for a copy rather than an edit.
`examples/camera-compute-kernel` is the short example built around that one
step if you want it on its own.

### One output, two destinations

`app.py` connects the lens's single output port twice — once to a window and
once to the rectifier:

```python
rt.connect(lens.output("fisheye_frame_to_downstream"), lens_window.input("video"))
rt.connect(lens.output("fisheye_frame_to_downstream"),
           detector.input("fisheye_frame_from_upstream"))
```

The producer does not know or care: the engine sizes the channel for the
destinations it has. What *does* change is the ring depth behind that port.
Depth bounds how far behind a consumer may fall before the producer overwrites
the slot it is still reading, so it is the consumer count that sets it:

| Ring | Depth | Why |
| --- | --- | --- |
| the lens's landing texture | 1 | `dispatch` returns with the GPU work retired, and nothing outside the lens ever names it |
| the lens's output | 3 | two consumers hold frames from it at once |
| the rectifier's output | 2 | the standard depth — one consumer, the window |

Two `DisplayWindow` instances is not a special arrangement either. The engine
owns the process's one event pump and mints a window per owner that asks, each
rendering on its own thread, so the two are not serialised behind one another.

### One lens model, two shaders

The warp and its inverse are only each other's inverse while they agree about
what radius a pixel is at and what the polynomial does to it. Kept apart they
drift silently — a rectifier a hair off its lens still produces a
plausible-looking picture, and the only symptom is a detector that quietly
does worse near the edges. So both kernels are built from one GLSL prelude in
`processors/radial_distortion_model.py` — `frame_centre`, `normalised_radius`,
`radial_scale` and the texel-to-sampler coordinate convention, and nothing
else — and each shader is that prelude plus its own bindings, push constants
and `main()`.

The same module owns the tile size, which reaches both shaders as a `#define`
so the dispatch's group count and the shader's `local_size` cannot disagree.

## The lens, and why the corners go black

The distortion is the ordinary polynomial radial model — a point at distance
`r` from the centre is moved to `r · (1 + k1·r² + k2·r⁴)`. Negative `k1` is the
barrel direction. `r` is normalised against the **half-diagonal**, so the
corners of any aspect ratio sit at `r = 1`; normalising against the shorter
half-axis instead, which is the natural choice on a square image, puts the
corners of a 16:9 frame past `r = 2`, well outside the range the coefficients
mean anything over.

The warp is a pull. Each output pixel goes looking for the input pixel that
lands on it, and under a barrel that reach is inward — which is what pushes
the picture outward. The consequence is that the whole distorted frame is
sampled out of an inner disc, and at the app's default `k1 = -0.25` that disc
has radius `0.75`. Source content outside it was never carried across, so no
inverse recovers it: there is nothing there to recover.
`largest_recoverable_normalised_radius` sweeps the polynomial at startup to
find that number.

That radius is necessary and not sufficient, and the second condition is the
one that is easy to miss: it asks whether a *circle* of that radius holds
anything, and the frame is a *rectangle*. A pixel straight above the centre
reaches a source radius near the half-diagonal while the top edge is only the
half-height away, so it lands off the frame entirely. So the rectifier makes
both tests — radius, then whether the source texel is inside the frame at all
— and what survives is their intersection: about 70% of a 16:9 frame at the
default coefficient, in the rounded shape the black border traces.

Both masks write black rather than sampling, and each has its own failure
mode if you drop it. Without the radius test Newton still returns a root — the
polynomial does not stop existing where the optics do — and the corners fill
with a stretched mirrored copy of content from elsewhere in the frame. Without
the rectangle test the engine's sampler clamps to the edge instead, painting a
smeared band along the top and bottom that reads as motion blur and is not.
The second one is the one to watch for: it is confined to the periphery, which
is the part of the frame nobody checks and the part this whole app is about.

Inside the disc, the rectifier solves `r_d · (1 + k1·r_d² + k2·r_d⁴) = r_u` for
`r_d` by Newton iteration from `r_u` itself, four steps, unrolled. That is the
true inverse rather than the `r_u / scale(r_u)` approximation the shape
invites, and the approximation is worth knowing about because of *where* it
fails. Rectifying the same captured frame both ways:

| inverse | whole frame | centre, `r ≤ 0.35` | outer, `r > 0.55` |
| --- | --- | --- | --- |
| Newton, four steps | 36.3 dB | 44.4 dB | 32.8 dB |
| `r_u / scale(r_u)` | 18.9 dB | 43.4 dB | 14.5 dB |

A degree apart in the middle and eighteen apart at the edge. An approximation
that is excellent everywhere you would think to look and wrong exactly at the
periphery is the worst possible shape for this particular application, which
is the whole reason the four Newton steps are there.

In a real deployment `k1` and `k2` come out of a checkerboard calibration and
are a property of the hardware. A synthetic lens is the one case where the
rectifier can be handed the exact numbers, which is what makes this app a
demonstration of the pipeline rather than of the calibration.

## Run it

```bash
uv venv --python 3.12 && uv sync
source .venv/bin/activate
streamlib run
```

Two windows open — the barrelled camera frame, and the rectified one with its
detections outlined. Ctrl-C stops it. Point the camera at something COCO knows
(a person, a chair, a bottle, a laptop) and the boxes appear; a webcam aimed at
a blank wall detects nothing and says so, once a second, in the log.

The first run downloads `yolov8n.pt` beside the app — about 6 MB, once.

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

This app needs real hardware and says so rather than pretending: the kernels
run on the engine's own Vulkan device and the detector wants a CUDA runtime, so
a machine with neither fails while the graph is starting. It is also a heavy
install — `ultralytics` brings a detector and `torch` brings CUDA, several
gigabytes between them.

One environment note, because the error names nothing useful. On a machine
that also has a **system** cuDNN on the loader's default path — anything with
NVIDIA's apt repo enabled, which puts `/usr/local/cuda*/targets/*/lib` in
`/etc/ld.so.conf.d` — pip's cuDNN loads its main library from the venv and
picks up an optional engine library from the system, and the first convolution
dies with `CUDNN_STATUS_SUBLIBRARY_VERSION_MISMATCH`. Nothing about streamlib
is involved; every pip-installed torch on that machine has it. Confirm it by
listing the cuDNN files a run mapped:

```bash
grep cudnn /proc/<pid>/maps | awk '{print $NF}' | sort -u
```

Any path outside `.venv` in that list is the one being mixed in.

## Editing it

Both shaders are strings in their processor's module, compiled by the engine at
startup on the machine that runs the app. There is no `glslc` to install, no
`.spv` to build and no build step between editing a shader and re-running —
re-running `streamlib run` is the edit loop. It costs a couple of seconds here
rather than the sub-second restart the lighter examples get, and the shader
compile is not why: each helper imports torch, and the detector's also loads a
network onto the GPU.

Three edits worth making on purpose, because each teaches something the code
alone does not:

- **Change `RADIAL_DISTORTION_K1` in `app.py` and re-run.** Both processors get
  the new value, the barrel deepens, and the black border grows — the
  recoverable disc shrinks as the barrel deepens, which the rectifier's log
  line at startup reports as `largest_recoverable_radius`.
- **Give the rectifier a coefficient the lens does not have** (edit its
  `config` in `app.py` on its own). Nothing refuses, nothing warns, and the
  rectified window looks almost right. That is the failure mode the shared
  prelude exists to make structurally impossible for everything *except* the
  coefficients, and the reason a real stack treats its calibration as
  load-bearing data.
- **Bind the camera's own surface id** — `frame.surface_id` in place of
  `camera_frame_landing_texture` in the lens — and skip the landing copy. The
  dispatch refuses, saying the graph cannot resolve that surface to a device
  texture. The rectifier's identical-looking binding works, because what it
  names is a kernel output and not a camera's pixel buffer. That difference is
  worth seeing once.

## Observing it

From another terminal with this venv activated, while it runs:

```bash
streamlib nodes                       # the live nodes on this machine
streamlib graph                       # processors, ports, links and their states
streamlib logs --list                 # the runtimes that have a log file
streamlib logs <RUNTIME_ID> --follow  # tail one of them, like `tail -F`
```

`graph` renders per-link drop counts under a node's `metrics`, but only for a
destination that lives in the app process — here, the two windows. Both Python
processors are helper-placed, like every Python processor, and count their
losses inside their own children; their nodes carry no `metrics` key at all
rather than a zero the parent cannot stand behind. So a quiet `graph` is not
yet proof that nothing was dropped.

To see what a kernel actually wrote rather than what a window shows, tap its
output channel and exchange the surface ids for PNGs. The channel name is the
source processor's id, lowercased, joined to its output port — read it out of
`graph` rather than spelling it by hand:

```bash
channel_for() {
  streamlib graph | python3 -c '
import json, sys
wanted = sys.argv[1]
for link in json.load(sys.stdin)["links"]:
    source = link["source"]
    if source["port_name"] == wanted:
        print((source["processor_id"] + "/" + source["port_name"]).lower())
        break' "$1"
}
streamlib exchange --channel "$(channel_for fisheye_frame_to_downstream)" \
    --count 3 --out /tmp/fisheye
streamlib exchange --channel "$(channel_for annotated_frame_to_downstream)" \
    --count 3 --out /tmp/rectified
```

That pair is the before-and-after at full resolution, with no window in the
graph and no display server in the path — the same door any API consumer uses.
`exchange` prints the paths it wrote, one per line; read those rather than
listing the directory, which is not cleared between runs.

`streamlib tap` on the same channel shows the bag itself. The rectifier's
carries the picture as a surface id and the detections beside it, as ordinary
bag fields:

```json
{"surface_id": "...", "width": 1280, "height": 720, "timestamp_ns": 12345,
 "detection_count": 1,
 "detections": [{"class_index": 0, "class_name": "person",
                 "confidence": 0.87, "box_xyxy": [412, 96, 890, 719]}]}
```

Nothing in the engine knows what any of those keys mean — a bag is a
self-describing named map and a link is plumbing. The window on that same port
reads `surface_id`, `width` and `height` and ignores the rest.

`--count` here is a target, not a ceiling: `exchange` keeps tapping — up to
eight bounded sampling rounds — until it has that many frames, and a run that
comes up short **exits nonzero** and says how many it got. That is the opposite
of `tap`'s own `--count`, which is an upper bound over one 500 ms window and
returns whatever arrived. Worth knowing before either goes in a script.

## Runtime knobs

Everything is literal config in `app.py` — edit it and re-run.

| Knob | Where | What it does |
| --- | --- | --- |
| `RADIAL_DISTORTION_K1` / `_K2` | both `rt.add(...)` calls | the lens the app simulates and the lens the rectifier assumes; negative `k1` is barrel |
| `detection_confidence_threshold` | `rt.add(UndistortingObjectDetector, config=…)` | how sure the model must be before a box is drawn |
| `detection_model_weights` | same | any ultralytics detection checkpoint; `yolov8n.pt` by default |
| `max_width` / `max_height` | `rt.add(CameraSource, config=…)` | the capture size to ask the device for |
| `title` / `width` / `height` / `scaling` | each `rt.add(DisplayWindow, config=…)` | window geometry; `scaling` is `fit` / `fill` / `stretch` |
| `STREAMLIB_CAMERA_DEVICE` | environment | capture device (e.g. `/dev/video0`); unset picks the first device found |

## Where the pixels do touch the host

One place, and it is not streamlib's side of the hand-off. The tensor handed
to the detector is built entirely on the device — channel slice, permute,
batch, scale, pad, all of it — and ultralytics takes a `torch.Tensor` source as
already preprocessed, so it neither letterboxes nor rescales it. What its
postprocessing then does with that batch is its own business, and it does copy
it back to make the `orig_imgs` its result objects carry.

This app does not use that copy — the boxes are drawn into the engine's own
texture, on the GPU — but it pays for it, once a frame. Reaching under
`predict()` to the raw forward pass and `non_max_suppression` avoids it and
keeps everything on the device; that is what a real perception stack does, and
it is a deliberate non-goal here, because an example that reaches past a
library's public API teaches the wrong lesson about the library.
