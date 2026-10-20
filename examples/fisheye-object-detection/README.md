# fisheye-object-detection

Camera → a fisheye lens → rectify → detect, with the distorted and the corrected
view both encoded and watched in a browser. The pipeline a monocular drone flies
with, running on a desk.

A wide-FOV lens is what a small aircraft carries — one camera, and as much of
the world in it as the glass can bend in. The bending is why perception stacks
on such a camera rectify before they detect: a model trained on rectilinear
photographs is being shown something its training set does not contain, most
of all at the periphery, which on a drone is where the obstacle you have not
hit yet lives. So the frame is rectified on the GPU first, and the model never
sees the lens at all.

That is the motivation, and this stream demonstrates the *pipeline* rather than
measuring the benefit — on any single scene the detection count moves in both
directions, and a claim about detection quality needs a dataset and a metric,
not a webcam. What it does show is that the correction is real: against a static
test pattern, measured on the uncompressed frames, the rectified frame scores
about **36 dB** PSNR on the camera frame it came from, where the distorted frame
scores about **15 dB**. The two encoded tracks show the same difference by eye,
and reproduce the figures only approximately, since the encode is lossy.

There is no fisheye lens on your desk, so `SyntheticFisheyeLens` applies the
distortion a real one would have baked in. Everything downstream of it is the
real thing.

## The model this example teaches

**A Tatolab stream is a normal Python project.** `stream.py` holds one `@stream`
function that adds, links and exposes nodes; `tatolab dev` loads it into this
machine's runtime, `tatolabd`. `CameraSource` and `H264Encoder` are native
built-ins that ship inside the runtime. `SyntheticFisheyeLens` and
`UndistortingObjectDetector` are this project's own nodes, in `nodes/`, and each
runs in its own child interpreter, which imports the class by name — so the
detector's torch and its network load in a process of their own, and nothing
they do can stall the lens.

### The engine prepares the model input; the frame takes the boxes back

The whole hand-off between the engine and a third-party GPU stack is these
lines, in `nodes/undistorting_object_detector.py`:

```python
detector_model_input = self.detector_input_kernel.apply_to_surface(
    ctx.gpu_limited_access, undistorted_frame_texture
)
with detector_model_input.tensor_surface as detector_input_tensor_surface:
    detections = self._detections_in(
        torch.from_dlpack(detector_input_tensor_surface),
        detector_model_input.geometry,
    )

with undistorted_frame_texture.as_device_tensor() as rectified_pixels:
    self._draw_boxes_into(torch.from_dlpack(rectified_pixels), detections)
```

`ModelInputTensorKernel`, compiled once in `setup()` with
`fit="pad_bottom_right", pad_to_multiple_of=32`, writes the detector's input —
RGB, channels-first, scaled into `[0, 1]`, padded at the bottom and right to
YOLOv8's stride — in one compute pass, into a tensor surface that
`torch.from_dlpack` reads with no copy. Padding rather than resizing keeps
every box the model reports in frame coordinates; the geometry the kernel
returns beside the tensor is what maps them back, and under this fit it maps
them onto themselves.

The detector takes its device from `torch.accelerator.current_accelerator()`
and never spells one by name, so the same node runs on CUDA on Linux and on
MPS on Apple Silicon.

Entering the frame's `as_device_tensor()` scope hands the engine's texture out
as a DLPack view, and that tensor is the write door. On Linux it is a linear
CUDA view the boxes are blitted back from when the scope closes, ordered ahead
of the engine's own next read of that texture; on macOS it is the texture's own
IOSurface pages, so each store lands in place. No fence, no timeline, no device
synchronize — none of that vocabulary reaches Python, and the pixels do not
travel through the host to get read or to get written.

Leaving the scope by a raise on Linux discards the write and the texture keeps
the frame it already held, because half a drawn frame blitted back publishes
something that surfaces as corrupt pixels downstream instead of as the
exception that actually happened.

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

`frame.surface_id` is a texture another **child process** wrote — the lens is a
Python node with its own interpreter, like this one — and a dispatch binds it
exactly as it binds a texture from this node's own ring. Nothing is copied and
nothing crosses the host to make that work.

That is the interesting half of the contrast with the stream's first hop.
There, `SyntheticFisheyeLens` reads `CameraSource` and *cannot* bind what it
gets: a camera publishes a **buffer-backed** frame, and a pixel buffer cannot
bind as a sampled texture, so each frame is landed in a texture the lens owns
first:

```python
ctx.gpu_limited_access.copy_surface_to_surface(
    frame.surface_id, camera_frame_landing_texture
)
```

The engine does that copy device-to-device, so the lens needs no array library
at all. `examples/camera-compute-kernel` is the short example built around that
one step if you want it on its own.

### One output, two destinations

`stream.py` connects the lens's single output port twice — once to an encoder
and once to the rectifier:

```python
stream_builder.connect(
    lens.output("fisheye_frame_to_downstream"),
    fisheye_frame_encoder.input("video"),
)
stream_builder.connect(
    lens.output("fisheye_frame_to_downstream"),
    detector.input("fisheye_frame_from_upstream"),
)
```

The producer does not know or care: the engine sizes the channel for the
destinations it has. Behind each output port is a `NodeOutputTextureRing`, a
pool the engine owns. A slot a consumer still holds is skipped and never
rewritten, and the pool grows while consumers hold frames; `depth` is how many
published frames nobody claimed stay resolvable behind the newest one.

| Ring | Depth | Why |
| --- | --- | --- |
| the lens's landing texture | 1 | `dispatch` returns with the GPU work retired, and nothing outside the lens ever names it |
| the lens's output | 3 | two consumers read every frame from it — the encoder and the rectifier |
| the rectifier's output | 2 | the standard depth — one consumer, its encoder |

The two `H264Encoder`s are two nodes, added under the names
`fisheye-frame-encoder` and `annotated-frame-encoder`, and each exposed
`encoded_video` port is its own track. A stream shows a picture by exposing an
encoder's output: the runtime opens no windows.

### One lens model, two shaders

The warp and its inverse are only each other's inverse while they agree about
what radius a pixel is at and what the polynomial does to it. Kept apart they
drift silently — a rectifier a hair off its lens still produces a
plausible-looking picture, and the only symptom is a detector that quietly
does worse near the edges. So both kernels are built from one GLSL prelude in
`nodes/radial_distortion_model.py` — `frame_centre`, `normalised_radius`,
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
sampled out of an inner disc, and at the stream's default `k1 = -0.25` that disc
has radius `0.75`. Source content outside it was never carried across, so no
inverse recovers it: there is nothing there to recover.
`largest_recoverable_normalised_radius` sweeps the polynomial when the detector
is constructed to find that number.

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
is the part of the frame nobody checks and the part this whole stream is about.

Inside the disc, the rectifier solves `r_d · (1 + k1·r_d² + k2·r_d⁴) = r_u` for
`r_d` by Newton iteration from `r_u` itself, four steps. That is the true
inverse rather than the `r_u / scale(r_u)` approximation the shape invites, and
the approximation is worth knowing about because of *where* it fails.
Rectifying the same captured frame both ways, measured on the uncompressed
frames:

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
rectifier can be handed the exact numbers, which is what makes this stream a
demonstration of the pipeline rather than of the calibration.

## Run it

With this machine's runtime running (`tatolabd`, which the installer starts):

```bash
uv sync
tatolab dev
```

`uv sync` installs `tatolab-stream` from PyPI — the authoring surface, and the
only Tatolab package the venv needs for the stream itself — beside `torch` and
`ultralytics`. The runtime is not on pip.

Use `tatolab dev` while editing — it loads the stream again on every saved edit
— and `tatolab run` otherwise. Both keep the stream as long as the command
runs; Ctrl-C unloads it.

Then open the stream in a browser:

```text
http://127.0.0.1:4446/watch/<machine>/main
```

The page plays the broadcast with its video tracks to pick from:
`fisheye-frame-encoder/encoded_video` is the barrelled camera frame, and
`annotated-frame-encoder/encoded_video` the rectified one with its detections
outlined. Each opens on its own too:

```text
http://127.0.0.1:4446/watch/<machine>/main/fisheye-frame-encoder/encoded_video
http://127.0.0.1:4446/watch/<machine>/main/annotated-frame-encoder/encoded_video
```

`http://127.0.0.1:4446/` lists this machine's name and every stream's exposed
ports. Point the camera at something COCO knows (a person, a chair, a bottle, a
laptop) and the boxes appear; a webcam aimed at a blank wall detects nothing
and says so, once a second, in the log.

The first run downloads `yolov8n.pt` into this directory — about 6 MB, once. The
detector resolves a relative weights path against the project directory, the
parent of `nodes/`, because its child interpreter's working directory is not
necessarily this one.

This stream needs real hardware and says so rather than pretending: the kernels
run on the engine's own GPU, and the detector runs on whichever accelerator
`torch.accelerator` reports — CUDA on Linux, MPS on Apple Silicon. A torch
build that sees no usable accelerator stops the detector in `setup()` and says
why. It is also a heavy install: `ultralytics` brings a detector and `torch`
brings its GPU runtime, several gigabytes between them on Linux.

On an Apple M1 Max (macOS 26.3, torch 2.14, ultralytics 8.4), YOLOv8n on MPS
was measured finding the same objects at the same confidences on the reference
image as on the CPU, at about 11 ms a frame.

One Linux environment note, because the error names nothing useful. On a machine
that also has a **system** cuDNN on the loader's default path — anything with
NVIDIA's apt repo enabled, which puts `/usr/local/cuda*/targets/*/lib` in
`/etc/ld.so.conf.d` — pip's cuDNN loads its main library from the venv and
picks up an optional engine library from the system, and the first convolution
dies with `CUDNN_STATUS_SUBLIBRARY_VERSION_MISMATCH`. Nothing about Tatolab is
involved; every pip-installed torch on that machine has it. Confirm it by
listing the cuDNN files the detector's child process mapped:

```bash
grep cudnn /proc/<pid>/maps | awk '{print $NF}' | sort -u
```

Any path outside `.venv` in that list is the one being mixed in.

## Editing it

Both shaders are strings in their node's module, compiled by the engine when
the node sets up, on the machine that runs the stream. There is no `glslc` to
install, no `.spv` to build and no build step between editing a shader and
seeing it — saving the file while `tatolab dev` runs is the edit loop. A reload
costs a couple of seconds here rather than the sub-second restart the lighter
examples get, and the shader compile is not why: the detector's child imports
torch and loads a network onto the GPU.

Three edits worth making on purpose, because each teaches something the code
alone does not:

- **Change `DEFAULT_RADIAL_DISTORTION_K1` in `nodes/radial_distortion_model.py`
  and save.** `stream.py` hands it to both nodes, so both get the new value,
  the barrel deepens, and the black border grows — the recoverable disc
  shrinks as the barrel deepens, which the detector's `rectifier and
  detector ready` log line reports as `largest_recoverable_radius`.
- **Give the rectifier a coefficient the lens does not have** (edit the
  detector's `config` in `stream.py` on its own). Nothing refuses, nothing
  warns, and the rectified track looks almost right. That is the failure mode
  the shared prelude exists to make structurally impossible for everything
  *except* the coefficients, and the reason a real stack treats its
  calibration as load-bearing data.
- **Bind the camera's own surface id** — `frame.surface_id` in place of
  `camera_frame_landing_texture` in the lens — and skip the landing copy. The
  dispatch refuses it: the camera's frame is a pixel buffer, which binds as no
  sampled texture. The rectifier's identical-looking binding works, because
  what it names is a kernel output and not a camera's pixel buffer. That
  difference is worth seeing once.

## Observing it

From another terminal:

```bash
tatolab streams                 # the streams this machine's runtime holds
tatolab graph --stream main     # nodes, ports, links, exposed ports and counters, as JSON
tatolab logs --stream main -f   # the stream's records, Rust and Python alike
```

A node is named after its class, lowercased — `camerasource`,
`syntheticfisheyelens`, `undistortingobjectdetector` — unless `add` passes
`name=`, as it does for the two encoders. An exposed port is the track
`<node>/<port>` of the broadcast `<machine>/<stream>.hang` at the machine's
local relay, so any MoQ client reads it; this repository's `tatolab-read-track`
writes a track's objects to files:

```bash
cargo run -p tatolab-read-track -- http://127.0.0.1:4443 <machine>/main.hang \
    annotated-frame-encoder/encoded_video --count 30 --out /tmp/rectified
cargo run -p tatolab-read-track -- http://127.0.0.1:4443 <machine>/main.hang \
    fisheye-frame-encoder/encoded_video --count 30 --out /tmp/fisheye
```

Each file is one H.264 access unit, so the files concatenated in
`objects.jsonl` order are an H.264 stream ffmpeg decodes to PNGs — the
before-and-after, frame by frame.

The detector's own port is worth reading as it is. It is internal to the
stream until `tatolab expose` makes it a track; each object is then the record
the detector wrote, which carries the picture as a surface id and the
detections beside it, as ordinary fields:

```bash
tatolab expose main undistortingobjectdetector annotated_frame_to_downstream
cargo run -p tatolab-read-track -- http://127.0.0.1:4443 <machine>/main.hang \
    undistortingobjectdetector/annotated_frame_to_downstream --count 3 --out /tmp/detections
```

Each `object-*.bin` is one msgpack record; decoded, it reads:

```json
{"surface_id": "...", "width": 1280, "height": 720, "timestamp_ns": 12345,
 "color_info": {"primaries": "...", "transfer": "...", "matrix": "...",
                "range": "..."},
 "detection_count": 1,
 "detections": [{"class_index": 0, "class_name": "person",
                 "confidence": 0.87, "box_xyxy": [412, 96, 890, 719]}]}
```

Nothing in the engine knows what any of those keys mean — a record is an open
map and a link is plumbing. The encoder on that same port reads `surface_id`,
`width`, `height` and `timestamp_ns` and ignores the rest. The surface id means
nothing outside the runtime: the pixels stay in engine-owned GPU memory, which
is why the picture is shared through the encoder.

## Knobs

Everything is literal config in `stream.py` — edit it and save while
`tatolab dev` runs.

| Knob | Where | Effect |
| --- | --- | --- |
| `CAMERA_DEVICE_ID` | top of `stream.py` | Capture device — a V4L2 path (`/dev/video2`) on Linux, an AVFoundation unique ID on macOS. `None` picks the first device found. |
| `FRAME_WIDTH` / `FRAME_HEIGHT` | top of `stream.py` | The camera's `max_width` / `max_height`: the negotiated format is clamped to fit. |
| `DEFAULT_RADIAL_DISTORTION_K1` / `_K2` | `nodes/radial_distortion_model.py` | The lens the stream simulates and the lens the rectifier assumes; negative `k1` is barrel. |
| `DETECTION_CONFIDENCE_THRESHOLD` | top of `stream.py` | How sure the model must be before a box is drawn. |
| `detection_model_weights` | `UndistortingObjectDetector`'s `config` | Any ultralytics detection checkpoint; `yolov8n.pt` by default, resolved against the project directory. |
| `Exposure.PRIVATE` | each `expose` | `Exposure.PUBLIC` shares the track off this machine, at every relay the runtime has joined. |

## Where the pixels do touch the host

One place, and it is not Tatolab's side of the hand-off. The tensor handed to
the detector is built entirely on the device — channel order, layout, scale and
pad in the engine's one compute pass — and ultralytics takes a `torch.Tensor`
source as already preprocessed, so it neither letterboxes nor rescales it. What
its postprocessing then does with that batch is its own business, and it does
copy it back to make the `orig_imgs` its result objects carry.

This stream does not use that copy — the boxes are drawn into the engine's own
texture, on the GPU — but it pays for it, once a frame. Reaching under
`predict()` to the raw forward pass and `non_max_suppression` avoids it and
keeps everything on the device; that is what a real perception stack does, and
it is a deliberate non-goal here, because an example that reaches past a
library's public API teaches the wrong lesson about the library.
