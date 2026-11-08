# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One stream per compute-kernel probe, each the probe alone.

A kernel probe needs no upstream: it acquires both surfaces itself and reports
from `setup`, so its stream is one node.
"""

from tatolab.stream import StreamBuilder, graph_generator

from compute_kernel_probes import (
    AcquiredTextureImpliesCopyUsageProbe,
    BatchExceptionProbe,
    BatchRefusalProbe,
    BindingRefusalProbe,
    ReadOneWriteAnotherProbe,
    ShaderSourceRefusalProbe,
    TextureBackedPixelsReachTheCpuProbe,
    TextureCpuDoorRaiseProbe,
    TwoPassBatchProbe,
)


@graph_generator
def read_one_write_another_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(ReadOneWriteAnotherProbe)


@graph_generator
def binding_refusal_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(BindingRefusalProbe)


@graph_generator
def texture_backed_pixels_reach_the_cpu_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(TextureBackedPixelsReachTheCpuProbe)


@graph_generator
def texture_cpu_door_raise_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(TextureCpuDoorRaiseProbe)


@graph_generator
def acquired_texture_implies_copy_usage_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(AcquiredTextureImpliesCopyUsageProbe)


@graph_generator
def shader_source_refusal_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(ShaderSourceRefusalProbe)


@graph_generator
def two_pass_batch_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(TwoPassBatchProbe)


@graph_generator
def batch_exception_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(BatchExceptionProbe)


@graph_generator
def batch_refusal_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(BatchRefusalProbe)
