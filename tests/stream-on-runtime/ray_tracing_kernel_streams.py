# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One stream per ray-tracing-kernel probe, each the probe alone.

A kernel probe needs no upstream: it builds its own acceleration structures,
acquires its own storage image and reports from `setup`, so its stream is one
node.
"""

from tatolab.stream import StreamBuilder, stream

from ray_tracing_kernel_probes import (
    AccelerationStructureHandleRefusalProbe,
    RayTracingBindingRefusalProbe,
    RayTracingBufferBindingRefusalProbe,
    RayTracingStageMismatchProbe,
    RayTracingTierAbsentRefusalProbe,
    TracedTriangleProbe,
)


@stream
def traced_triangle_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(TracedTriangleProbe)


@stream
def ray_tracing_binding_refusal_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(RayTracingBindingRefusalProbe)


@stream
def ray_tracing_stage_mismatch_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(RayTracingStageMismatchProbe)


@stream
def ray_tracing_buffer_binding_refusal_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(RayTracingBufferBindingRefusalProbe)


@stream
def acceleration_structure_handle_refusal_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(AccelerationStructureHandleRefusalProbe)


@stream
def ray_tracing_tier_absent_refusal_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(RayTracingTierAbsentRefusalProbe)
