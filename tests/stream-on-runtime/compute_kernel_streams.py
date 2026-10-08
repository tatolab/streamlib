# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One stream per compute-kernel probe, each the probe alone.

A kernel probe needs no upstream: it acquires both surfaces itself and reports
from `setup`, so its stream is one node.
"""

from tatolab.stream import StreamBuilder, stream

from compute_kernel_probes import (
    AcquiredTextureImpliesCopyUsageProbe,
    BindingRefusalProbe,
    ReadOneWriteAnotherProbe,
    ShaderSourceRefusalProbe,
    TextureBackedPixelsReachTheCpuProbe,
    TextureCpuDoorRaiseProbe,
)


@stream
def read_one_write_another_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(ReadOneWriteAnotherProbe)


@stream
def binding_refusal_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(BindingRefusalProbe)


@stream
def texture_backed_pixels_reach_the_cpu_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(TextureBackedPixelsReachTheCpuProbe)


@stream
def texture_cpu_door_raise_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(TextureCpuDoorRaiseProbe)


@stream
def acquired_texture_implies_copy_usage_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(AcquiredTextureImpliesCopyUsageProbe)


@stream
def shader_source_refusal_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(ShaderSourceRefusalProbe)
