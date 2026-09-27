# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Python processor that writes a tensor storage buffer through torch, and
processors that resolve it downstream and read it through torch.

Each reports over the `MARKER:PROBE_RESULT` child-to-parent log forwarding the
other GPU probes use, tagged with `probe` because a scenario runs two of them.
"""

import dataclasses
import json
import math
import os
import sys
import traceback
import uuid

from streamlib import clock, input, log, output, processor

MODEL_INPUT_TENSOR_SHAPE = [1, 3, 640, 640]
ODD_TENSOR_SHAPE = [3, 7, 11]
# Processor configs carry scalars, so a scenario names its tensor.
TENSORS_BY_NAME = {
    "model_input": (MODEL_INPUT_TENSOR_SHAPE, "float32"),
    "odd": (ODD_TENSOR_SHAPE, "float16"),
}
POOL_ROTATION_DEPTH = 2
FRAMES_PUBLISHED_WHILE_THE_FIRST_IS_HELD = 3 * POOL_ROTATION_DEPTH
MINIMUM_INTERVAL_BETWEEN_HELD_TENSOR_PUBLISHES_NS = 150_000_000

RESULT_MARKER = "MARKER:PROBE_RESULT "

# The device a tensor's DLPack export lands on, in torch's name for it.
NATURAL_TORCH_DEVICE_TYPE = "mps" if sys.platform == "darwin" else "cuda"
# A DLPack device the tensor does not live on: CUDA on macOS, Metal on Linux.
FOREIGN_DLPACK_DEVICE = (2, 0) if sys.platform == "darwin" else (8, 0)


def _report(probe_name: str, observation_body) -> None:
    """One result line per observation — a failure carries its own traceback."""
    try:
        observation = observation_body()
    except BaseException:  # noqa: BLE001 — re-raised by the asserting test
        observation = {"failure": traceback.format_exc()}
    log.info(
        RESULT_MARKER
        + json.dumps({"probe": probe_name, "pid": os.getpid(), **observation})
    )


def _torch_device_unavailable_reason() -> "str | None":
    import torch

    if NATURAL_TORCH_DEVICE_TYPE == "mps" and not torch.backends.mps.is_available():
        return "torch sees no MPS device"
    if NATURAL_TORCH_DEVICE_TYPE == "cuda" and not torch.cuda.is_available():
        return "torch sees no CUDA device"
    return None


def _synchronize_the_torch_device(torch) -> None:
    if NATURAL_TORCH_DEVICE_TYPE == "mps":
        torch.mps.synchronize()
    else:
        torch.cuda.synchronize()


def _mlx_or_none():
    """MLX on macOS, where it reads a `kDLMetal` capsule; `None` elsewhere."""
    if sys.platform != "darwin":
        return None
    try:
        import mlx.core  # pyright: ignore[reportMissingImports]

        return mlx.core
    except ImportError:
        return None


def expected_tensor_values(torch, shape: "list[int]", dtype: str, frame_index: int):
    """The values frame `frame_index` carries: a position ramp offset by the
    frame, small enough that float16 holds every one exactly."""
    element_count = math.prod(shape)
    ramp = torch.arange(element_count, dtype=torch.float32) % 251 + frame_index + 1
    return ramp.reshape(shape).to(getattr(torch, dtype))


@dataclasses.dataclass
class TensorStorageBufferPublishingSourceConfig:
    tensor_name: str = "model_input"
    frames_to_publish: int = POOL_ROTATION_DEPTH
    minimum_interval_between_publishes_ns: int = 0
    open_a_window_before_acquiring: bool = False


@processor(execution="continuous", interval_ms=10)
class TensorStorageBufferPublishingSource:
    """Writes each frame's values into a pooled tensor through torch and
    publishes its surface id once the handle's close has ordered the writes."""

    @output()
    def tensors_to_downstream(self) -> None: ...

    def __init__(self, config: TensorStorageBufferPublishingSourceConfig) -> None:
        self._config = config
        self._shape, self._dtype = TENSORS_BY_NAME[config.tensor_name]
        self._pool_key = f"tensor-storage-buffer-probe-{uuid.uuid4().hex}"
        self._last_publish_ns: "int | None" = None
        self._surface_ids_published_so_far: "list[str]" = []
        self._producer_observations: "list[dict]" = []
        self._window = None
        self._stopped = False

    def setup(self, ctx) -> None:
        if self._config.open_a_window_before_acquiring:
            self._window = ctx.gpu_full_access.create_window(
                "tensor-storage-buffer-after-a-swapchain", 64, 64
            )

    def process(self, ctx) -> None:
        import torch

        if self._stopped:
            return
        frame_index = len(self._surface_ids_published_so_far)
        if frame_index >= self._config.frames_to_publish:
            return
        unavailable = _torch_device_unavailable_reason()
        if unavailable is not None:
            self._stopped = True
            _report(
                "TensorStorageBufferPublishingSource",
                lambda: {"torch_device_unavailable": unavailable},
            )
            return
        now_ns = clock.monotonic_now_ns()
        if (
            self._last_publish_ns is not None
            and now_ns - self._last_publish_ns
            < self._config.minimum_interval_between_publishes_ns
        ):
            return
        self._last_publish_ns = now_ns

        with ctx.gpu_limited_access.acquire_storage_buffer_from_processor_output_pool(
            self._pool_key,
            POOL_ROTATION_DEPTH,
            self._shape,
            self._dtype,
        ) as tensor_surface:
            surface_id = tensor_surface.surface_id
            written = torch.from_dlpack(tensor_surface)
            written.copy_(
                expected_tensor_values(
                    torch, self._shape, self._dtype, frame_index
                ).to(written.device)
            )
            reread = torch.from_dlpack(tensor_surface)
            _synchronize_the_torch_device(torch)
            with ctx.gpu_limited_access.resolve_surface(
                surface_id
            ) as independently_imported_surface:
                # A second checkout imports the engine allocation on its own,
                # so it sees the write before the close only if torch wrote
                # that allocation rather than a staging copy of it.
                write_visible_through_an_independent_import = bool(
                    torch.equal(
                        torch.from_dlpack(independently_imported_surface), written
                    )
                )
            self._producer_observations.append(
                {
                    "surface_id": surface_id,
                    "stated_shape": tensor_surface.shape,
                    "stated_dtype": tensor_surface.dtype,
                    "tensor_device": str(written.device),
                    "tensor_shape": list(written.shape),
                    "tensor_dtype": str(written.dtype),
                    "exports_share_memory": written.data_ptr() == reread.data_ptr(),
                    "write_visible_through_an_independent_import": (
                        write_visible_through_an_independent_import
                    ),
                }
            )

        ctx.outputs.write(
            "tensors_to_downstream",
            {
                "surface_id": surface_id,
                "frame_index": frame_index,
                "timestamp_ns": clock.monotonic_now_ns(),
            },
        )
        self._surface_ids_published_so_far.append(surface_id)
        if len(self._surface_ids_published_so_far) == self._config.frames_to_publish:
            _report(
                "TensorStorageBufferPublishingSource",
                lambda: {
                    "surface_ids_published": self._surface_ids_published_so_far,
                    "producer_observations": self._producer_observations,
                },
            )


def _read_a_published_tensor(
    ctx, surface_id: str, shape, dtype, frame_index, resolved_frame_index
) -> dict:
    """Compare the tensor `surface_id` names against the values of the frame
    whose bag arrived and of the frame the id was published for."""
    import torch

    with ctx.gpu_limited_access.resolve_surface(surface_id) as tensor_surface:
        read = torch.from_dlpack(tensor_surface)

        def equals_frame(index: int) -> bool:
            expected = expected_tensor_values(torch, shape, dtype, index)
            return bool(torch.equal(read, expected.to(read.device)))

        observation = {
            "surface_id": surface_id,
            "stated_shape": tensor_surface.shape,
            "stated_dtype": tensor_surface.dtype,
            "tensor_device": str(read.device),
            "tensor_shape": list(read.shape),
            "values_equal": equals_frame(frame_index),
            "values_equal_its_own_frames": equals_frame(resolved_frame_index),
        }
        mx = _mlx_or_none()
        if mx is not None:
            mlx_read = mx.from_dlpack(tensor_surface, copy=False)
            expected = expected_tensor_values(torch, shape, dtype, frame_index)
            observation["mlx_shape"] = list(mlx_read.shape)
            observation["mlx_values_equal"] = bool(
                mx.array_equal(mlx_read, mx.array(expected.float().numpy()))
            )
        return observation


@dataclasses.dataclass
class PublishedTensorReadingSinkConfig:
    tensor_name: str = "model_input"
    # The negative control: resolve the previous frame's id against this
    # frame's expected values, which must then mismatch.
    resolve_the_previous_frames_id: bool = False


@processor
class PublishedTensorReadingSink:
    """Resolves each published tensor id and compares its values to the ones
    its producer wrote for that frame."""

    @input(delivery_profile="ordered")
    def tensors_from_upstream(self) -> None: ...

    def __init__(self, config: PublishedTensorReadingSinkConfig) -> None:
        self._config = config
        self._previous_surface_id: "str | None" = None

    def process(self, ctx) -> None:
        bag = ctx.inputs.read("tensors_from_upstream")
        if bag is None:
            return
        resolved_surface_id = bag["surface_id"]
        resolved_frame_index = bag["frame_index"]
        if self._config.resolve_the_previous_frames_id:
            previous_surface_id = self._previous_surface_id
            self._previous_surface_id = bag["surface_id"]
            if previous_surface_id is None:
                return
            resolved_surface_id = previous_surface_id
            resolved_frame_index = bag["frame_index"] - 1

        _report(
            "PublishedTensorReadingSink",
            lambda: {
                "frame_index": bag["frame_index"],
                "published_surface_id": bag["surface_id"],
                **_read_a_published_tensor(
                    ctx,
                    resolved_surface_id,
                    *TENSORS_BY_NAME[self._config.tensor_name],
                    bag["frame_index"],
                    resolved_frame_index,
                ),
            },
        )


@processor
class HeldTensorRereadingSink:
    """Resolves the first tensor and holds its handle open, re-reading it
    through torch as every later tensor is published."""

    @input(delivery_profile="ordered")
    def tensors_from_upstream(self) -> None: ...

    def __init__(self) -> None:
        self._held_tensor_surface = None
        self._later_tensors_seen = 0

    def setup(self, ctx) -> None:
        # Loading torch and its CUDA context takes longer than the producer
        # takes to cycle the pool; done lazily in the first process() it
        # outlasts the first tensor.
        import torch

        if _torch_device_unavailable_reason() is None:
            torch.zeros(1, device=NATURAL_TORCH_DEVICE_TYPE)

    def process(self, ctx) -> None:
        import torch

        bag = ctx.inputs.read("tensors_from_upstream")
        if bag is None:
            return
        if self._held_tensor_surface is None:
            # The claim holds the slot from the read to the resolve, then
            # drops, so the rereads rest on the resolved handle alone.
            claim_from_the_read = (
                ctx.gpu_limited_access.claim_surface_against_producer_reuse(
                    bag["surface_id"]
                )
            )
            self._held_tensor_surface = ctx.gpu_limited_access.resolve_surface(
                bag["surface_id"]
            )
            del claim_from_the_read
            return
        self._later_tensors_seen += 1
        held_tensor_surface = self._held_tensor_surface

        def reread_the_held_tensor() -> dict:
            read = torch.from_dlpack(held_tensor_surface)
            expected = expected_tensor_values(
                torch, MODEL_INPUT_TENSOR_SHAPE, "float32", 0
            ).to(read.device)
            return {
                "held_surface_id": held_tensor_surface.surface_id,
                "later_surface_id": bag["surface_id"],
                "later_tensors_seen": self._later_tensors_seen,
                "held_values_still_frame_0s": bool(torch.equal(read, expected)),
            }

        _report("HeldTensorRereadingSink", reread_the_held_tensor)


KERNEL_WRITTEN_TENSOR_SHAPE = [4, 64, 64]
KERNEL_TENSOR_UNWRITTEN_SENTINEL = -1.0
INDEX_PATTERN_TENSOR_BINDING = "index_pattern_tensor"
WRITE_INDEX_PATTERN_GLSL = """\
#version 450
layout(local_size_x = 64) in;
layout(set = 0, binding = 0, std430) writeonly buffer IndexPatternTensor {
    float values[];
} index_pattern_tensor;
void main() {
    uint at = gl_GlobalInvocationID.x;
    if (at < index_pattern_tensor.values.length()) {
        index_pattern_tensor.values[at] = float(at);
    }
}
"""

DRAWN_COLOUR_TARGET_EXTENT = 16
PAINT_COLOUR_BINDING = "paint_colour_tensor"
# Each channel a multiple of 1/255 exactly, so the rgba8 target holds it with
# no rounding to argue about.
PAINT_COLOUR = [0.2, 0.4, 0.6, 1.0]
CONTROL_PAINT_COLOUR = [0.6, 0.2, 0.4, 1.0]
FULL_SCREEN_TRIANGLE_VERTEX_GLSL = """\
#version 450
void main() {
    vec2 corner = vec2((gl_VertexIndex << 1) & 2, gl_VertexIndex & 2);
    gl_Position = vec4(corner * 2.0 - 1.0, 0.0, 1.0);
}
"""
PAINT_THE_TENSORS_COLOUR_FRAGMENT_GLSL = """\
#version 450
layout(set = 0, binding = 0, std430) readonly buffer PaintColourTensor {
    float values[4];
} paint_colour_tensor;
layout(location = 0) out vec4 painted_colour;
void main() {
    painted_colour = vec4(
        paint_colour_tensor.values[0],
        paint_colour_tensor.values[1],
        paint_colour_tensor.values[2],
        paint_colour_tensor.values[3]
    );
}
"""


@processor(
    execution="manual",
    description="A compute kernel and a draw each bind a tensor storage buffer by surface id",
)
class TensorStorageBufferKernelBindingProbe:
    """A compute kernel writes an index pattern into one tensor, which torch
    reads back; a second tensor the kernel never names keeps its sentinel.
    A draw reads its colour from a tensor, and a second tensor paints a
    different colour — so the pixels come from the binding, not the shader."""

    def setup(self, ctx) -> None:
        import torch

        unavailable = _torch_device_unavailable_reason()
        if unavailable is not None:
            _report(
                "TensorStorageBufferKernelBindingProbe",
                lambda: {"torch_device_unavailable": unavailable},
            )
            return
        gpu = ctx.gpu_full_access

        def observe() -> dict:
            element_count = math.prod(KERNEL_WRITTEN_TENSOR_SHAPE)
            index_pattern = (
                torch.arange(element_count, dtype=torch.float32)
                .reshape(KERNEL_WRITTEN_TENSOR_SHAPE)
                .to(NATURAL_TORCH_DEVICE_TYPE)
            )
            compute_kernel = gpu.create_compute_kernel(
                source=WRITE_INDEX_PATTERN_GLSL
            )
            with (
                gpu.acquire_storage_buffer(
                    KERNEL_WRITTEN_TENSOR_SHAPE, "float32"
                ) as dispatched_tensor,
                gpu.acquire_storage_buffer(
                    KERNEL_WRITTEN_TENSOR_SHAPE, "float32"
                ) as undispatched_tensor,
            ):
                for tensor_surface in (dispatched_tensor, undispatched_tensor):
                    torch.from_dlpack(tensor_surface).fill_(
                        KERNEL_TENSOR_UNWRITTEN_SENTINEL
                    )
                compute_kernel.dispatch(
                    bindings={INDEX_PATTERN_TENSOR_BINDING: dispatched_tensor},
                    group_count=(element_count // 64, 1, 1),
                )
                dispatched = torch.from_dlpack(dispatched_tensor)
                undispatched = torch.from_dlpack(undispatched_tensor)
                try:
                    dispatched_tensor.__dlpack__(
                        max_version=(1, 0), dl_device=FOREIGN_DLPACK_DEVICE
                    )
                    foreign_device_refusal = None
                except BufferError as refusal:
                    foreign_device_refusal = str(refusal)
                compute_observation = {
                    "foreign_device_refusal": foreign_device_refusal,
                    "compute_binding_names": list(compute_kernel.binding_names),
                    "dispatched_tensor_device": str(dispatched.device),
                    "dispatched_tensor_shape": list(dispatched.shape),
                    "dispatched_holds_the_index_pattern": bool(
                        torch.equal(dispatched, index_pattern)
                    ),
                    "undispatched_holds_the_index_pattern": bool(
                        torch.equal(undispatched, index_pattern)
                    ),
                    "undispatched_still_holds_the_sentinel": bool(
                        torch.all(undispatched == KERNEL_TENSOR_UNWRITTEN_SENTINEL)
                    ),
                }

            graphics_kernel = gpu.create_graphics_kernel(
                color_attachment_formats=["rgba8_unorm"],
                vertex_source=FULL_SCREEN_TRIANGLE_VERTEX_GLSL,
                fragment_source=PAINT_THE_TENSORS_COLOUR_FRAGMENT_GLSL,
                label="python-tensor-painted-fullscreen-triangle",
            )
            colour_target = gpu.acquire_texture(
                DRAWN_COLOUR_TARGET_EXTENT,
                DRAWN_COLOUR_TARGET_EXTENT,
                "rgba8_unorm",
                ["render_attachment", "texture_binding", "copy_src", "copy_dst"],
            )

            def draw_painting_from(colour: "list[float]") -> "list[list[int]]":
                with gpu.acquire_storage_buffer([4], "float32") as colour_tensor:
                    torch.from_dlpack(colour_tensor).copy_(
                        torch.tensor(colour, dtype=torch.float32)
                    )
                    graphics_kernel.draw(
                        bindings={PAINT_COLOUR_BINDING: colour_tensor},
                        color_targets=[colour_target],
                        extent=(DRAWN_COLOUR_TARGET_EXTENT, DRAWN_COLOUR_TARGET_EXTENT),
                        vertex_count=3,
                    )
                colour_target.lock(read_only=True)
                try:
                    pixels = colour_target.as_numpy().reshape(-1, 4)
                    distinct = {tuple(int(channel) for channel in pixel) for pixel in pixels}
                    return [list(pixel) for pixel in sorted(distinct)]
                finally:
                    colour_target.unlock()

            return {
                **compute_observation,
                "graphics_binding_names": list(graphics_kernel.binding_names),
                "distinct_pixels_painted_from_the_tensor": draw_painting_from(
                    PAINT_COLOUR
                ),
                "distinct_pixels_painted_from_the_control_tensor": draw_painting_from(
                    CONTROL_PAINT_COLOUR
                ),
            }

        _report("TensorStorageBufferKernelBindingProbe", observe)

    def process(self, ctx) -> None:
        pass
