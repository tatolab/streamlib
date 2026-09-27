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


def _cuda_unavailable_reason() -> "str | None":
    import torch

    if not torch.cuda.is_available():
        return "torch sees no CUDA device"
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
        unavailable = _cuda_unavailable_reason()
        if unavailable is not None:
            self._stopped = True
            _report(
                "TensorStorageBufferPublishingSource",
                lambda: {"cuda_unavailable": unavailable},
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
            self._producer_observations.append(
                {
                    "surface_id": surface_id,
                    "stated_shape": tensor_surface.shape,
                    "stated_dtype": tensor_surface.dtype,
                    "tensor_device": str(written.device),
                    "tensor_shape": list(written.shape),
                    "tensor_dtype": str(written.dtype),
                    # Two exports of one surface address the same memory: the
                    # capsule is the engine's allocation, never a copy of it.
                    "exports_share_memory": written.data_ptr() == reread.data_ptr(),
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


def _read_a_published_tensor(ctx, surface_id: str, shape, dtype, frame_index) -> dict:
    import torch

    with ctx.gpu_limited_access.resolve_surface(surface_id) as tensor_surface:
        read = torch.from_dlpack(tensor_surface)
        expected = expected_tensor_values(torch, shape, dtype, frame_index).to(read.device)
        return {
            "surface_id": surface_id,
            "stated_shape": tensor_surface.shape,
            "stated_dtype": tensor_surface.dtype,
            "tensor_device": str(read.device),
            "tensor_shape": list(read.shape),
            "values_equal": bool(torch.equal(read, expected)),
        }


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
        if self._config.resolve_the_previous_frames_id:
            previous_surface_id = self._previous_surface_id
            self._previous_surface_id = bag["surface_id"]
            if previous_surface_id is None:
                return
            resolved_surface_id = previous_surface_id

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

    def process(self, ctx) -> None:
        import torch

        bag = ctx.inputs.read("tensors_from_upstream")
        if bag is None:
            return
        if self._held_tensor_surface is None:
            self._held_tensor_surface = ctx.gpu_limited_access.resolve_surface(
                bag["surface_id"]
            )
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
