# Design memo: a shader-body effect step, a model-input step, and the starter they change

2026-09-22, the design pass `docs/plan/changes/portable-gpu-interop.md` §Not in scope handed to
"the next `/align` round". Written on the Apple Silicon host against the tree at `2df51fde`.
Nothing here is decided; every recommendation is the session's, and the batch at the end is
what the owner answers.

Evidence is marked **[V]** verified from a primary source (URL or `file:line`), **[P]** measured
in the earlier memo (`docs/research/2026-09-22-portable-gpu-interop-for-python-processors.md`),
or **[I]** inferred. Two facts the brief handed over could not be re-verified from a primary
page this session and are marked as such rather than restated as fact.

## Question

Three linked decisions, all downstream of the engine copy the owner decided today
(`ARCHITECTURE.md:1164-1171`):

1. Should the engine offer a one-call GPU effect step where the user writes only a GLSL
   function body, and if so where does it live and what does it look like?
2. Should model-input pre-processing (frame → resized, normalised, channel-ordered float
   tensor at the model's input size, over DLPack, on both floors) be an engine step, and is
   it the same capability as #516's missing buffer bindings?
3. Given 1, what does `streamlib new` scaffold?

## Answer, short

1. **Yes, as wheel-Python over the primitives that exist — never a second kernel system.**
   `GlslPixelEffect` composes `create_compute_kernel` + `ProcessorOutputTextureRing` +
   `copy_surface_to_surface` + `dispatch`; the GLSL around the user's body is one template
   string the engine compiles like any other kernel. The engine gains nothing; the escalate
   wire gains nothing. Same shape as `ProcessorOutputTextureRing` and `VideoFrame`, which the
   plan already records as "wheel-layer grammar over the shipped primitives — no engine
   change" (`ARCHITECTURE.md:322-324`). One source, same extent and format out, dials as
   push constants, always landed by the engine copy. Rust gets nothing until a Rust consumer
   names it — the parity bar is kernel *kinds* (`ARCHITECTURE.md:983-995`), and this is not a
   kind. On macOS it works the day #2403 un-gates kernels and #2420's macOS xfail flips,
   with no macOS line in the helper by construction.
2. **Yes, in two layers, and it is one capability with #516.** The engine work is the
   storage-buffer surface Python cannot reach today: acquire one with a tensor shape and
   dtype, bind it at dispatch as `storage_buffer`, export it over DLPack as that shape, pass
   it downstream by surface id. That single capability is what #516's forward-forward layer
   hits first and what a model-input kernel writes into. The pre-processing itself is then a
   wheel-Python preset, `ModelInputTensorKernel`, one GLSL pass over that capability — the
   same composition shape as decision 1. Not a built-in graph node: it meets none of the
   built-in criterion's three clauses (`ARCHITECTURE.md:110-121`), and a Python processor
   that wants the DeepStream shape can wrap the preset in three lines.
3. **Option C, after both #2420 and #2403 are green on their lanes.** One GPU effect through
   `GlslPixelEffect` in the video path, one CPU processor off a fan-out that reads the
   effect's output on the host with `numpy` and logs a number — "GPU for pixels, CPU for
   logic, and the pixel view is explicit and says slow in its name". Fan-out is supported to
   32 destinations per port (`runtime/streamlib-ipc-types/src/lib.rs:341`,
   `open_iceoryx2_service_op.rs:554-580`) and the fisheye example already does it
   (`examples/fisheye-object-detection/app.py:86-96`). Until the flip, the numpy scaffold
   stays: on macOS the first minute is #2361's, and a scaffold that refuses at `setup()` on a
   Mac because kernels are gated would break the sentence there.

Ordering: 1 needs #2420. 3 needs 1, and its flip waits on #2403 for the Mac. 2's engine
half is a new change that can start now and sits beside the parity milestone; its preset
follows 1's pattern and lands after it. #2423 (fisheye off the device literal) precedes 2's
fisheye conversion and is unaffected by it.

---

## Decision 1 — the shader-body effect step

### What it is, plainly

Today a user who wants a GPU effect writes `examples/camera-compute-kernel/processors/grayscale_compute.py`:
~60 lines around a 5-line idea, and seven things to hold in the head — the landing ring,
the output ring, the kernel object, the binding names, the workgroup tile and its `#define`,
the push-constant `struct.pack`, and the output bag. The GLSL itself is 25 lines of which
five are the effect. The comparison the brief handed over — a raw starter at ~7 concepts /
~60 lines against numpy's ~3 / ~30 — matches what the file shows.

The effect tools users already know hide all of that. TouchDesigner's GLSL TOP pre-declares
the inputs and the user writes four lines [V]:

```glsl
layout(location = 0) out vec4 fragColor;
void main()
{
   vec4 inputColor = texture(sTD2DInputs[0], vUV.st);
   fragColor = TDOutputSwizzle(inputColor);
}
```
"Input sampler variables are declared for you as arrays" and uniforms are matched by
declaring "a uniform of the same name and size as the parameters you have set on the
Vectors pages" — https://docs.derivative.ca/Write_a_GLSL_TOP. Godot's canvas item is
`void fragment() { COLOR = texture(TEXTURE, UV); }` with `COLOR`, `UV`, `TEXTURE`, `TIME`
provided, and "when a shader is later assigned to a material, the uniforms will appear as
editable parameters" with `uniform float amount : hint_range(0, 1);` [V]
https://docs.godotengine.org/en/stable/tutorials/shaders/shader_reference/canvas_item_shader.html,
https://docs.godotengine.org/en/stable/tutorials/shaders/shader_reference/shading_language.html.
OBS shaderfilter pre-declares `image`, `uv_pixel_interval`, `uv_size`, `elapsed_time`, and
"any parameters you add to your shader (defined as `uniform` variables) will be detected by
the plugin and exposed in the properties window" [V] https://github.com/exeldro/obs-shaderfilter.
Unity's low-code post-processing is a Fullscreen shader graph whose "URP Sample Buffer"
node "automatically retrieves the rendered scene" and the user fills Base Color [V]
https://docs.unity3d.com/6000.0/Documentation/Manual/urp/post-processing/post-processing-custom-effect-low-code.html.

The common shape: the tool owns the sampler, the output, the coordinates and the time; the
user owns one function and a list of named dials. That is what this step is.

### Where it can live — three shapes

**Shape A — wheel-Python composition over the primitives (recommended).** A class in
`sdk/streamlib-python-wheel/python/streamlib/glsl_pixel_effect.py`, beside
`processor_output_texture_ring.py`. It owns one GLSL template string, builds a
`ComputeKernel` from it in `setup()`, and per frame does copy → dispatch → bag. No new
escalate op, no Rust, no stub entry beyond the Python module's own annotations (pyright
gates it; `stubtest` is untouched because nothing compiled changes).

```python
# processors/inverting_effect.py — the whole processor
from streamlib import (
    GlslPixelEffect, RuntimeContextFullAccess, RuntimeContextLimitedAccess,
    VideoFrame, input, output, processor,
)

INVERT_GLSL = """
vec4 effect(vec4 source, ivec2 at) {
    return vec4(1.0 - source.rgb, source.a);
}
"""

@processor
class InvertingEffect:
    """Inverts every frame's colours on the GPU and passes it on."""

    @input(delivery_profile="newest")
    def video_from_upstream(self) -> VideoFrame: ...

    @output()
    def video_to_downstream(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.invert = GlslPixelEffect.compile(ctx.gpu_full_access, effect_glsl=INVERT_GLSL)

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
        if frame is None:
            return
        ctx.outputs.write(
            "video_to_downstream",
            self.invert.apply_to_frame(ctx.gpu_limited_access, frame),
        )
```

With a dial and time:

```python
GRADE_GLSL = """
vec4 effect(vec4 source, ivec2 at) {
    float luma = dot(source.rgb, vec3(0.299, 0.587, 0.114));
    float pulse = 0.5 + 0.5 * sin(streamlib_elapsed_seconds);
    return vec4(mix(source.rgb, vec3(luma), dials.strength * pulse), source.a);
}
"""

def setup(self, ctx):
    self.grade = GlslPixelEffect.compile(
        ctx.gpu_full_access, effect_glsl=GRADE_GLSL, dials={"strength": "float"}
    )

def process(self, ctx):
    ...
    bag = self.grade.apply_to_frame(
        ctx.gpu_limited_access, frame, dials={"strength": self.strength}
    )
```

What the template supplies, and the user never writes (names prefixed so they cannot
collide with a user's own):

- `layout(local_size_x = 8, local_size_y = 8) in;` and the bounds check on `imageSize`.
- `uniform sampler2D streamlib_source;` (sampled) and `uniform writeonly image2D
  streamlib_output;` (storage), plus `vec4 streamlib_source_at(ivec2 at)` and
  `vec4 streamlib_source_uv(vec2 uv)` for neighbourhood reads — what a blur or a
  chromatic-aberration effect needs, and what OBS's `uv_pixel_interval` exists for.
- `ivec2 streamlib_extent` and `float streamlib_elapsed_seconds`, read from the helper's
  monotonic clock (`monotonic_now_ns`, `_engine.pyi:86`) at the first `apply_to_frame` —
  never wall-clock, per the doctrine.
- A `layout(push_constant) uniform Dials { ... } dials;` block generated from the `dials`
  declaration, in declaration order, with `struct` packing done by the helper.
- `#line 1` before the user's body, so a compile error reports the user's own line number
  and not the template's. (`#line` is core GLSL; that shaderc/glslang honours it in the
  diagnostic was not re-verified this session — **[I]**, one probe.)
- `main()`, which fetches, calls `effect`, and stores.

Per frame, `apply_to_frame` does: `next_texture_for_this_frame` on a depth-1 landing ring
and a depth-2 output ring (the grayscale example's exact shapes, `grayscale_compute.py:
122-129`); `copy_surface_to_surface(frame.surface_id, landing)`; `dispatch` with
`group_count` computed from the extent; and returns the bag
`{"surface_id", "width", "height", "timestamp_ns", "color_info"}` carried from the source
frame — timestamp preserved, as the example's comment insists (`grayscale_compute.py:188-192`).

**Shape B — an engine-side primitive, `RhiPixelEffect`, exposed by a new escalate op.** Rust
would hold the template, compile it, and own the rings; Python would get
`ctx.gpu_full_access.create_pixel_effect(body, dials)`. Rust authors would get the same
object. Costs: a new wire op with its `deny_unknown_fields` struct, encoding vector, dispatch
arm and cfg twin, handler, helper client, two `#[pymethods]` blocks and two stub entries —
the exact list #2420 is paying for the copy (`docs/plan/changes/portable-gpu-interop.md:37-44`) — for
something that is, on the wire, a kernel registration plus a dispatch. The engine would then
hold two ways to make a compute kernel from Python. That is the parallel-system shape the
doctrine forbids, and the tree's closest Rust precedent — `RhiToneMapper`, an engine-owned
fixed-body image→image kernel (`runtime/streamlib-engine/src/core/rhi/tone_mapper.rs:1-14`)
— exists because the engine's own colour pipeline consumes it; no Rust consumer names a
user-body effect.

**Shape C — a built-in graph node, `rt.add(GlslEffect, config={"glsl": ...})`.** The most
"zero ceremony" spelling: no Python processor at all, the effect runs in the app process on
the engine's own thread with no helper hop. It fails the built-in criterion on every clause
(`ARCHITECTURE.md:110-121`): no deadline the helper hop cannot meet (a), no engine-only
primitive once the copy lands (b), no OS-facing device (c). It would also make the starter's
first edit a string inside `app.py` rather than "the scaffolded processor", which is the
sentence's own words (`ARCHITECTURE.md:11-19`). And it teaches the wrong lesson for the AI
story: the user's next step after an effect is logic, and logic is a Python processor.

### The contract the recommended shape states

- **One source, one output, same extent, same format.** The output ring is allocated at
  the frame's extent in `rgba8_unorm`, which is what the camera publishes and the window
  samples (`grayscale_compute.py:48`). A frame in a format the copy refuses (NV12 is
  two planes — `python_gpu_surface_pixel_exchange.rs:1078`) is refused by the copy's own
  message, and the helper adds only the sentence "a pixel effect takes a single-plane RGBA
  frame; decode or convert first".
- **Always landed by the engine copy, never bound by bare id.** A texture-backed source
  (another kernel's output) could bind straight into the dispatch, as the fisheye detector
  does (`undistorting_object_detector.py:236-250`). The helper does not try, because nothing
  on the Python surface says which backing a surface has, and the plan holds that "no door
  names the backing" (`ARCHITECTURE.md:1034`). One copy per frame is the price; it is
  GPU-side and it is the copy the owner just decided. The skip is an optimisation for the
  day a measured budget asks for it, and it would be the engine's to derive.
- **Dials are push constants.** `dials={"name": "float" | "int" | "vec2" | "vec4"}`,
  declared at compile, supplied at every `apply_to_frame` (never persisted — the kernel
  API's own rule, `ARCHITECTURE.md:1113-1123`). `vec3` is refused by name at compile: its
  16-byte alignment in the push block is the classic silent-shift bug. The whole block must
  fit 128 bytes, the Vulkan minimum for `maxPushConstantsSize` **[I — spec floor, not
  re-fetched]**; a larger dial set is the uniform-buffer case decision 2's capability could
  carry later, and is refused by name until then.
- **Refusals**, each at the line the user can fix: no `vec4 effect(vec4, ivec2)` in the body
  (checked before compile, naming the signature); a compiler diagnostic, offset to the
  user's own lines; a dial supplied that was not declared, or declared and not supplied
  (the same rule dispatch already enforces for bindings); a `vec3` dial; a push block over
  the floor; a frame the copy refuses. All engine-side refusals stay engine-side — the
  helper never becomes the only guard.
- **Errors inside the GPU work** surface from `dispatch` as today.
- **Rust authors** keep the descriptor form (`core/rhi/compute_kernel.rs:199-214`). A Rust
  `PixelEffect` helper would be the template copied into a second language with no
  consumer; when one appears, the template's text is the thing to share, not a wire.

### On macOS

The helper has no platform line. It runs the day two things are true: #2403 lifts the
"only available on Linux" arms from `register_compute_kernel` / `run_compute_kernel`, and
#2420's macOS test (a strict `awaiting_macos_parity(issue=2403)` xfail, per its body) turns
green. Its textures are #2402's IOSurface flavour; its ordering is the shared-event timeline
#2401 carries; none of that is visible from the helper. The per-frame cost is two escalate
round trips with a host wait each (copy, then dispatch — decision A in
`docs/plan/changes/portable-gpu-interop.md:191-217`); the owner accepted that cost for the copy, and the
batch scope cannot absorb the copy today (its rejected option C).

### Recommendation

Shape A. Name: `GlslPixelEffect`, constructed by `GlslPixelEffect.compile(gpu_full_access,
effect_glsl=..., dials=...)`, applied by `apply_to_frame(gpu_limited_access, frame,
dials=...) -> dict`. The name says what it is (a GLSL effect over pixels) and the method
names say which capability each needs. Where it lives: the wheel, exported from
`streamlib`, with the template and the packing rules under test (a wheel test that a
one-line body inverts a test-pattern frame pixel-exact, a test that a compile error names
the user's line, and one per refusal).

**What would change my mind.** A Rust built-in that wants a user-supplied body — then the
template moves to Rust and Python calls it, and the wire op is paid once. Or a measured
per-frame budget where the extra host wait of the always-copy shape shows up — then the
engine's skip-when-texture-backed is the fix, still inside the helper's contract.

**Confidence: high** on the shape (the tree's own precedent, twice), **medium** on the
surface details (the dial vocabulary, the two sampling helpers) until a user writes a blur
with it.

---

## Decision 2 — model-input pre-processing as an engine step

### What it is, plainly

A detector wants a tensor, not a picture: a fixed size (640×640 for YOLOv8n), RGB with no
alpha, channels first, batched, float32 in `[0, 1]`, on the device. ultralytics states
this for a tensor source: "BCHW format with RGB channels `float32 (0.0-1.0)`" [V]
https://docs.ultralytics.com/modes/predict/. Today the fisheye example builds that by hand
(`examples/fisheye-object-detection/processors/undistorting_object_detector.py:284-300`):

```python
detector_input = (
    rectified_frame[..., :3]
    .permute(2, 0, 1)
    .unsqueeze(0)
    .contiguous()
    .float()
    .div_(255.0)
)
pad_right = -width % DETECTOR_INPUT_STRIDE
pad_bottom = -height % DETECTOR_INPUT_STRIDE
if pad_right or pad_bottom:
    detector_input = torch.nn.functional.pad(detector_input, (0, pad_right, 0, pad_bottom))
```

Six torch ops, at least two device allocations (`contiguous`, `float`), and a pad that this
example chose over a resize so boxes stay in frame coordinates. Every AI runtime surveyed
ships this as a built-in GPU step:

- Holoscan `FormatConverterOp` — "Convert between tensor/image formats, memory layouts, and
  data types between operators"; parameters `in_dtype` (`rgb888`, `rgba8888`, `yuv420`,
  `nv12`, `yuyv`, ...), `out_dtype` (`rgb888`, `uint8`, `float32`, ...), `scale_min` /
  `scale_max` ("Output will be clipped to this minimum/maximum value", defaults 0.0 / 1.0),
  `resize_width` / `resize_height`, `resize_mode` ("NPP's NppiInterpolationMode"),
  `out_channel_order` ("Sequence of integers describing how channel values are permuted"),
  `alpha_value` [V] https://docs.nvidia.com/holoscan/sdk-user-guide/operators.html and
  `include/holoscan/operators/format_converter/format_converter.hpp` on `main`.
- DeepStream `Gst-nvdspreprocess` — ROIs "scaled and format converted as per the network
  requirements for inference", then "prepares a raw tensor from the scaled & converted
  ROIs"; keys `network-input-shape`, `network-color-format`, `tensor-data-type`,
  `network-input-order` (NCHW / NHWC), `pixel-normalization-factor`, `offsets` /
  `mean-file`, `scaling-filter` [V]
  https://docs.nvidia.com/metropolis/deepstream/dev-guide/text/DS_plugin_gst-nvdspreprocess.html.
  `Gst-nvinfer` otherwise does it itself: "y = net scale factor*(x-mean)" [V]
  https://docs.nvidia.com/metropolis/deepstream/dev-guide/text/DS_plugin_gst-nvinfer.html.
- MediaPipe `ImageToTensorCalculator` — `output_tensor_width/height`, `keep_aspect_ratio`
  ("usually results in letterbox padding. Otherwise ... stretched"),
  `output_tensor_float_range` `[min, max]`, `border_mode` [V]
  `mediapipe/calculators/tensor/image_to_tensor_calculator.proto` on `master`.

The union is small and stable: **target size, fit (stretch / letterbox / pad), channel order
and alpha drop, layout (NCHW / NHWC), dtype (float32 / float16 / uint8), and an affine
`(x * scale - mean) / std`**. That is one compute pass. The preset recommended below takes
float32 and float16 only; uint8 stays with the generic tensor buffer, whose dtype is the
caller's.

### What blocks it today: the output is a buffer, and Python cannot reach one

A tensor is not a picture. Its natural GPU backing is a storage buffer, and three things in
the tree stop a Python processor from holding one:

1. No acquire: `acquire_pixel_buffer` and `acquire_texture` are the only allocations on
   either capability (`_engine.pyi:1053-1065`).
2. No bind: the wire spells `storage_buffer` and `uniform_buffer` as kinds
   (`python_processor_context.rs:2445-2470`), but the dispatch resolves every binding as a
   texture and refuses a buffer-backed surface — "a buffer-backed surface is not something a
   dispatch can bind" (`subprocess_escalate.rs:2219-2230`). The plan names this exactly:
   "the only by-surface-id resolution the escalate path has is texture-shaped, so a Python
   processor is refused by name. Both are undesigned" (`ARCHITECTURE.md:989-995`).
3. No export: the DLPack shape is derived from a `PixelFormat` — `(H, W, 4)` u8/u16/f16/f32
   for the RGBA formats, `(H, W)` for gray, `None` for NV12
   (`python_gpu_surface_pixel_exchange.rs:245-285`). Nothing can say `(1, 3, 640, 640)`.

#516 hits the same three: "Python kernels can bind textures only: storage- and
uniform-buffer bindings are a named, undesigned gap, which is the constraint a
forward-forward layer on flat tensors hits first" [V] `gh issue view 516`. So the answer
to "one capability or two" is **one**: a storage-buffer surface Python can acquire with a
tensor shape and dtype, bind at dispatch, export over DLPack as that shape, and pass
downstream by surface id. Two consumers: the pre-processing kernel and #516's layer.
Uniform buffers are a separate, smaller item — push constants cover dials — and can trail.

### Options

**Option A — no engine step; a torch recipe in the wheel.** `streamlib.model_input_tensor_from_frame(frame, size, fit, layout, dtype, scale, mean, std)`
implemented with `torch.from_dlpack(frame)` plus `interpolate` / `pad` / `permute`. torch
imported lazily so the wheel keeps no torch dependency (the ADR's decision 3). Portable by
construction after #2423: the device is the tensor's own. `interpolate` supports `bilinear`,
`bicubic`, `area`, `nearest-exact` and `antialias` for `bilinear` / `bicubic` [V]
https://docs.pytorch.org/docs/2.14/generated/torch.nn.functional.interpolate.html. Zero
engine work; ships this week.

Against: it is the fisheye code moved into the wheel, still N kernel launches and
allocations per frame, still torch-only (an ONNX Runtime, TensorRT or CoreML user gets
nothing), and its output cannot leave the processor — a downstream processor cannot read a
torch tensor from a bag. It also puts torch-shaped code into a wheel whose stated position
is that torch is a user's library, not the wheel's.

**Option B — the buffer capability, then a one-pass kernel preset over it (recommended).**

Engine half (a change of its own, resolving the plan's "undesigned" line):

```python
# new on GpuContextLimitedAccess and GpuContextFullAccess
def acquire_storage_buffer(self, shape: Sequence[int], dtype: str) -> GpuSurfaceHandle:
    """A device storage buffer named by surface id, shaped as a tensor.

    `dtype` is one of `float32`, `float16`, `int32`, `uint8`. The id binds at
    dispatch as `storage_buffer`, exports over DLPack as `shape`/`dtype` on the
    floor's own device, and passes downstream in a bag like any surface.
    """
```

Plus: the escalate dispatch resolves a `storage_buffer` binding by surface id (the plan's
sentence retires); the DLPack export carries the declared shape and dtype instead of a
`PixelFormat` derivation; the surface-share service registers a buffer with its shape the
way it registers a texture with its recipe. The forward-forward layer in #516 then has its
weights and activations, and so does JEPA or any tensor-shaped kernel.

Wheel half, the preset:

```python
from streamlib import ModelInputTensorKernel

def setup(self, ctx: RuntimeContextFullAccess) -> None:
    self.detector_input = ModelInputTensorKernel.compile(
        ctx.gpu_full_access,
        input_width=640,
        input_height=640,
        fit="letterbox",            # or "stretch", or "pad_bottom_right"
        channel_order="rgb",        # alpha dropped; "bgr" for models trained that way
        layout="nchw",              # or "nhwc"
        dtype="float32",            # or "float16"
        scale=1.0 / 255.0,
        mean=(0.0, 0.0, 0.0),
        std=(1.0, 1.0, 1.0),
    )

def process(self, ctx: RuntimeContextLimitedAccess) -> None:
    ...
    model_input = self.detector_input.apply_to_surface(
        ctx.gpu_limited_access, undistorted_frame_texture
    )
    detector_input = torch.from_dlpack(model_input.tensor_surface)   # (1, 3, 640, 640)
    result = self.detection_model.predict(source=detector_input, ...)[0]
    boxes_in_frame = model_input.geometry.boxes_to_source(result.boxes.xyxy)
```

`apply_to_surface` returns a small object: `tensor_surface` (the ring slot, a
`GpuSurfaceHandle` that is a DLPack producer) and `geometry` (the scale and pad offsets the
letterbox applied, so boxes map back — MediaPipe and ultralytics both need this and both
make the caller do it). `torch.from_dlpack` "will share the memory with the input tensor" [V]
https://docs.pytorch.org/docs/2.14/generated/torch.from_dlpack.html; the capsule's device is
`kDLCUDA = 2` on Linux and `kDLMetal = 8` on macOS, `kDLFloat = 2` for the element [V]
https://github.com/dmlc/dlpack/blob/main/include/dlpack/dlpack.h — torch reads both (memo §5).

The kernel is one GLSL pass: sample the source texture with a bilinear `sampler2D` at the
letterboxed coordinate, apply the affine, write the element at `[c][y][x]` (or
`[y][x][c]`) into the storage buffer. Colour conversion is *not* folded in: the source is an
RGBA texture, which is what every published frame reaches Python as; a YUV source goes
through the engine's own `RhiColorConverter` before it is published, and the ADR already
rejected "a converting blit" as two concerns in one (`docs/decisions/portable-gpu-interop.md:71-72`).

Before/after for the fisheye detector, counted from the file: the device-literal lines
`:214-216` go with #2423 regardless; the 20-line `_detections_in` preamble `:275-300`
becomes three lines; the stride padding becomes `fit="pad_bottom_right"` with
`pad_to_multiple_of=32`, which keeps its "boxes already in frame coordinates" property
(`:293-300`).

**Option C — a built-in graph node, `rt.add(ModelInputPreprocessor, config={...})`.** The
Holoscan / DeepStream shape: a node between the source and the model, its output a bag
`{"surface_id", "shape", "dtype", "layout"}`. It needs everything option B needs (the
buffer surface must cross a process boundary by id), then adds a helper process and a hop
per model. It fails the built-in criterion the same way decision 1's shape C does. And once
option B exists, a user who wants the node shape writes a three-line Python processor
around the preset — which is the extension model's stated preference ("Pure Python stays a
complete way to write a processor", `ARCHITECTURE.md:106-107`).

**The hack, named so nobody reaches for it:** a `Rgba32Float` pixel buffer of extent
`(W, 3·H)` exports as `(3H, W, 4)` float32 today, and `torch.from_dlpack(x)[..., 0].view(3, H, W)`
would read it. It hides a tensor inside a picture format, the read side has to know the
trick, and it wastes three of four channels. Rejected.

### Recommendation

Option B, in two tickets under one change: the storage-buffer capability first (engine,
Linux; the macOS arm rides #2403 like every other escalate GPU op), then
`ModelInputTensorKernel` as wheel-Python over it, converting the fisheye example as the
canary. Option A is not worth shipping in between: it would be deleted the week B lands.

**What would change my mind.** If the owner rules the runtime line at "the engine moves
pixels, models are the user's" — then B's *preset* is an extension wheel and only the
buffer capability is engine work. That split costs nothing today; the batch asks it.

**Confidence: high** that the capability is one and is the blocker; **medium** on the preset's
parameter set (the union above is the survey's, not a user's); **low** on any performance
claim — one pass versus six torch ops is unmeasured, and this memo does not claim a number.

---

## Decision 3 — the `streamlib new` starter

### What it is, plainly

The scaffold today (`sdk/streamlib-python-wheel/python/streamlib/cli.py:289-411`) writes one
numpy processor whose longest comment is a workaround: "The mapping is write-combined: CPU
reads of it run around 175 MB/s ... costs ~225ms a frame against ~30ms this way". The
first thing a new user reads is that the path they were handed is the slow one. #2361
already owes that comment a rewrite for macOS, where the mapping is cached.

### Options

- **A — GPU only.** One `GlslPixelEffect` processor. Fewest files; but the vision/ML user's
  first real question — "how do I get at the pixels in Python?" — goes unanswered, and the
  scaffold then models no CPU path at all.
- **B — numpy default, `--gpu` flag.** Two scaffolds to keep green on both lanes, and the
  default stays the slow path with the apologetic comment.
- **C — one GPU effect in the video path, one CPU processor off a fan-out (recommended).**
- **D — keep numpy.** Fine until the helper exists; wrong after.

### Option C, the files

`app.py`:

```python
"""A StreamLib app: camera → GPU effect → window, with a CPU meter watching.

`streamlib dev` finds `setup(rt)` below by convention — there is no manifest and
no `main()`. Edit `processors/inverting_effect.py` and re-run `streamlib dev` to
see the change.

Processors live in their own modules, never in this file: each one runs in its
own child interpreter, which imports the class by name.
"""

from processors.brightness_meter import BrightnessMeter
from processors.inverting_effect import InvertingEffect
from streamlib import CameraSource, DisplayWindow, Runtime


def setup(rt: Runtime) -> None:
    source = rt.add(CameraSource)
    effect = rt.add(InvertingEffect)
    meter = rt.add(BrightnessMeter)
    window = rt.add(DisplayWindow, config={"title": "StreamLib", "scaling": "fit"})

    rt.connect(source.output("video"), effect.input("video_from_upstream"))
    # One output, two readers: the window shows the frame, the meter measures it.
    rt.connect(effect.output("video_to_downstream"), window.input("video"))
    rt.connect(effect.output("video_to_downstream"), meter.input("video_from_upstream"))
```

`processors/inverting_effect.py` — exactly the decision 1 listing above.

`processors/brightness_meter.py`:

```python
"""Logic on the CPU: reads the effect's frames on the host and logs a number.

The pixel view is explicit — `frame.cpu()` says it is the slow door — and this
processor is a sink off a fan-out, so nothing it does can slow the picture.
"""

import numpy

from streamlib import RuntimeContextLimitedAccess, VideoFrame, input, log, processor

REPORT_INTERVAL_NS = 1_000_000_000


@processor
class BrightnessMeter:
    """Logs the mean brightness of the frames it sees, once a second."""

    @input(delivery_profile="newest")
    def video_from_upstream(self) -> VideoFrame: ...

    def setup(self, ctx) -> None:
        self.next_report_at_ns = 0

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
        if frame is None or ctx.time < self.next_report_at_ns:
            return
        with frame.cpu() as pixels:
            brightness = float(numpy.mean(pixels[:, :, :3]))
        log.info("brightness", mean=round(brightness, 1), width=frame.width, height=frame.height)
        self.next_report_at_ns = ctx.time + REPORT_INTERVAL_NS
```

`pyproject.toml` keeps `["streamlib", "numpy>=2.1"]` — no torch, per the ADR's decision 3
— and the other three files are unchanged.

The meter is the shape a remote or local inference call takes later: read a bag, do
something slow in your own process, emit a fact. Swapping `numpy.mean` for an HTTP call to
a typed-answer API or a local LLM server changes nothing about the graph, and the placement
rule (one helper per processor) is what makes a blocking call harmless.

### First minute

- **Linux, after the flip:** `streamlib new`, `uv sync`, `streamlib dev` — the window shows
  the camera inverted at camera rate; the log prints a brightness line once a second. The
  first edit is the three-line GLSL body; the second is the number the meter computes.
- **macOS, after the flip:** identical, once #2402 → #2403 → #2420's mac arm are green.
  Before that, `GlslPixelEffect.compile` refuses at `setup()` with #2403's message, which
  breaks "sees their camera within a minute" on the Mac. So the flip waits.
- **Today, both floors:** unchanged numpy scaffold. #2361's payoff ("the scaffolded
  `InvertingEffect` ... edits a camera frame as a numpy view") keeps its target; the flip
  turns the edit into a meter read and a GPU invert, and #2361's own wheel test — a
  processor edits a pooled frame, verified pixel-wise — stands on its own regardless.

### What the flip needs to prove

- The cross-floor check clean over the scaffold (already gated by #2421's pytest beside
  `test_cli.py:435-541`); no device literal, no floor-bound import — true by construction.
- `test_cli_launch.py::test_the_scaffolded_app_reaches_a_running_graph` and
  `test_every_helper_interpreter_goes_live_inside_the_startup_budget` with two helpers
  instead of one — the second is the one to watch: the startup budget test now covers two
  child interpreters.
- Fan-out: supported and capped at 32 per output port, refused by name past the cap
  (`open_iceoryx2_service_op.rs:571-577`); the fisheye app is the shipped precedent.

### Recommendation

C, flipped by one ticket after #2420 and #2403 are both green, with the flip's ticket
carrying #2361's comment rewrite because the comment disappears with the file it lives in.

**What would change my mind.** If the two-helper startup budget test fails on the Mac
floor, A (GPU only, one helper) becomes the flip and the meter becomes the second thing
the docs show rather than the scaffold. **Confidence: medium-high.**

---

## How the three fit together, and the AI positioning

The three are one ladder with the same shape on every rung — *engine primitive, wheel-Python
composition, user body*:

| rung | engine primitive | wheel composition | user writes |
|---|---|---|---|
| pixels | copy + compute kernel (shipped / #2420) | `GlslPixelEffect` | a `vec4 effect(...)` body |
| tensors | storage-buffer surface (decision 2's change) | `ModelInputTensorKernel` | the model's input spec |
| logic | bags, fan-out, own process (shipped) | nothing — plain Python | the call |

Against the owner's AI positioning:

- **Local torch inference** — decision 2 is its front door on both floors: the frame lands
  in the tensor the model wants, on the device the capsule names, and `torch.accelerator`
  never has to be spelled. Decision 1 is the rectify / denoise / crop step before it.
- **Engine-kernel learning primitives (forward-forward, JEPA)** — #516's stated blocker is
  decision 2's capability. Once a Python kernel binds a storage buffer by surface id, an FF
  layer is a compute kernel over two tensor surfaces (positive, negative) with its weights
  in a third, and the memo #516 asks for can be written against a real surface.
- **Remote typed inference** — TypeSafe's Jev takes "state + questions" in one request and
  returns typed answers (`choice`, `score`, a 0–1 `Noul`), with "every question ... evaluated
  in parallel and in isolation against the same state" [V] https://docs.typesafe.ai/introduction.
  Its input is facts, not pixels: the meter's brightness, the detector's `detections` list
  that already rides the bag (`undistorting_object_detector.py:270-271`). That is the third
  rung — a Python processor that reads a bag and calls out — and needs no engine work at
  all. The same holds for a local LLM server. Decision 3's CPU processor is the scaffold's
  demonstration of that rung.

Ordering between the three: 1 before 3 (3 uses 1). 2 is independent of 1 in code but should
follow it in time so the preset copies a settled composition pattern. Neither 1 nor 3
waits on 2.

## Dependencies and sequencing against the tickets that exist

| this memo | needs | why |
|---|---|---|
| D1 `GlslPixelEffect`, Linux | #2420 | the landing copy is its first per-frame call |
| D1 on macOS | #2402 → #2403, then #2420's mac xfail flipping | kernels gated; textures cross on IOSurface |
| D2 buffer capability | nothing shipped; sits beside milestone 52 | its own change under §Graphics; retires the "undesigned" sentence at `ARCHITECTURE.md:989-995` and answers #516's precondition |
| D2 on macOS | #2403 | the same un-gate every escalate GPU op takes |
| D2 preset + fisheye conversion | D2 capability, #2423 | #2423 removes the device literal first, so the conversion diff is the preprocessing only |
| D3 flip | D1 green on both lanes, #2403, #2361 | the Mac first minute must not refuse at `setup()`; #2361's comment moves into the flip |

No ticket here blocks a parity ticket. #2361, #2402, #2403, #2420, #2423 all proceed as
written.

## Unknowns

- **Format equality in the copy.** A camera frame is a pixel buffer whose format the
  Python surface spells `"bgra"` / `"rgba"` (`acquire_pixel_buffer`, `_engine.pyi:1053`);
  a landing texture is `"rgba8_unorm"`. #2420's "same format" refusal must treat those as
  one format for the helper to land a camera frame at all. Verify in #2420's tests before
  D1 starts; if they are distinct in the engine's `PixelFormat`, the helper's landing
  texture takes the frame's own spelling.
- **Two host waits per frame.** Copy then dispatch, each synchronous. Unmeasured; the
  earlier cupy path paid a blit-out, a foreign import and a blit-back, so this is expected
  to be cheaper, but "expected" is not a number.
- **`#line` in shaderc diagnostics** — assumed; one probe settles it.
- **Push-constant floor (128 bytes)** — the Vulkan spec minimum from memory, not
  re-fetched. The helper refuses over it by name either way.
- **ultralytics on MPS** — #2423's own unmeasured item; D2's fisheye conversion inherits it.
- **`interpolate(antialias=True)` on MPS** — only relevant to option A, which is not
  recommended.
- **The tensor surface's memory type on each floor** — device-local on both (a kernel writes
  it), exported the way a pixel buffer is: on Linux over the OPAQUE_FD staging blit
  (`ARCHITECTURE.md:205-209`), on macOS over a no-copy `MTLBuffer` if #2404's IOSurface
  route extends to a plain buffer, which #2404 does not promise for a non-frame buffer.
  That is D2's one real macOS design question and belongs in its change file.
- **Whether the runtime line puts the preset inside the engine tree** — asked below.

## Suggested `/align` batch

One round; recommended option first; each answerable in one line.

1. **D1 shape** — A: wheel-Python `GlslPixelEffect` over `create_compute_kernel` + rings +
   `copy_surface_to_surface`, no engine change. B: engine primitive plus escalate op. C:
   built-in node. **Rec A.**
2. **D1 spelling** — `GlslPixelEffect.compile(gpu_full_access, effect_glsl=, dials=)` in
   `setup()`, `apply_to_frame(gpu_limited_access, frame, dials=) -> bag` in `process()`,
   user body `vec4 effect(vec4 source, ivec2 at)`. **Rec yes.**
3. **D1 dials** — `{name: "float"|"int"|"vec2"|"vec4"}` as push constants, `vec3` refused;
   `streamlib_extent` and `streamlib_elapsed_seconds` (monotonic) pre-declared, plus
   `streamlib_source_at` / `streamlib_source_uv` sampling helpers. **Rec yes.**
4. **D1 scope** — one source, output at the source's extent in `rgba8_unorm`, always landed
   by the engine copy (no backing sniffing), multi-input deferred. **Rec yes.**
5. **D1 Rust** — no Rust helper until a Rust consumer names one. **Rec yes.**
6. **D2 capability** — one change: `acquire_storage_buffer(shape, dtype)` on both
   capabilities, `storage_buffer` bound by surface id at dispatch, DLPack export by declared
   shape, downstream by id; uniform buffers trail. Serves #516 and the preset. **Rec yes.**
7. **D2 runtime line** — the pre-processing preset is A: wheel-Python in the engine tree
   (`ModelInputTensorKernel`), or B: an extension wheel, with only the buffer capability in
   the engine. **Rec A** — it is the same shape as `GlslPixelEffect` and the fisheye
   example is its named consumer.
8. **D2 parameters** — target size, fit (`stretch` / `letterbox` / `pad_bottom_right` with
   `pad_to_multiple_of`), channel order with alpha dropped, layout (`nchw` / `nhwc`),
   dtype (`float32` / `float16`), `scale` / `mean` / `std`; no colour conversion; returns
   the tensor surface plus the letterbox geometry. **Rec yes.**
9. **D2 timing** — its own change now, Linux first, macOS arm rides #2403; the fisheye
   conversion after #2423. **Rec yes.**
10. **D3 default** — C: `GlslPixelEffect` invert in the video path plus a numpy
    `BrightnessMeter` sink off a fan-out; flip after #2420 and #2403 are green on both
    lanes; the flip's ticket carries #2361's comment rewrite. **Rec C.**
11. **D3 dependencies** — the scaffold keeps `numpy`, gains nothing. **Rec yes.**
