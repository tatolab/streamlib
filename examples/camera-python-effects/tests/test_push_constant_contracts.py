# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Every processor's push-constant payload against its shader's own block.

The one contract in this app that nothing else checks. A wrong size is refused
at the draw, but a *right* size with the members in the wrong order is not —
the engine sees bytes — so it reaches the screen as an effect quietly reading
somebody else's number. These lock both.
"""

from __future__ import annotations

import struct

import pytest

from camera_python_effects.gpu_surface_conventions import read_shader_source
from camera_python_effects.processors.breaking_news_compositor import (
    COMPOSITE_PUSH_CONSTANT_FORMAT,
    COMPOSITE_PUSH_CONSTANT_SIZE,
)
from camera_python_effects.processors.crt_film_grain import (
    CRT_PUSH_CONSTANT_FORMAT,
    CRT_PUSH_CONSTANT_SIZE,
)
from camera_python_effects.processors.cyberpunk_glitch import (
    GLITCH_PUSH_CONSTANT_FORMAT,
    GLITCH_PUSH_CONSTANT_SIZE,
)

from .glsl_push_constant_block import push_constant_block_size_of

# The floor Vulkan guarantees for a push-constant range.
GUARANTEED_PUSH_CONSTANT_BYTES = 128

EVERY_PUSH_CONSTANT_CONTRACT = [
    pytest.param(
        "cyberpunk_glitch.frag",
        GLITCH_PUSH_CONSTANT_FORMAT,
        GLITCH_PUSH_CONSTANT_SIZE,
        id="cyberpunk_glitch",
    ),
    pytest.param(
        "crt_film_grain.frag",
        CRT_PUSH_CONSTANT_FORMAT,
        CRT_PUSH_CONSTANT_SIZE,
        id="crt_film_grain",
    ),
    pytest.param(
        "breaking_news_composite.frag",
        COMPOSITE_PUSH_CONSTANT_FORMAT,
        COMPOSITE_PUSH_CONSTANT_SIZE,
        id="breaking_news_composite",
    ),
]


@pytest.mark.parametrize(
    ("shader_file_name", "packing_format", "declared_size"),
    EVERY_PUSH_CONSTANT_CONTRACT,
)
def test_the_payload_is_exactly_the_block_the_shader_declares(
    shader_file_name: str, packing_format: str, declared_size: int
) -> None:
    shader_block_size = push_constant_block_size_of(read_shader_source(shader_file_name))
    assert struct.calcsize(packing_format) == declared_size
    assert declared_size == shader_block_size, (
        f"{shader_file_name} declares a {shader_block_size}-byte push-constant block "
        f"and the processor packs {declared_size} bytes for it"
    )


@pytest.mark.parametrize(
    ("shader_file_name", "packing_format", "declared_size"),
    EVERY_PUSH_CONSTANT_CONTRACT,
)
def test_the_payload_fits_the_range_vulkan_guarantees(
    shader_file_name: str, packing_format: str, declared_size: int
) -> None:
    del shader_file_name, packing_format
    assert declared_size <= GUARANTEED_PUSH_CONSTANT_BYTES


@pytest.mark.parametrize(
    ("shader_file_name", "packing_format", "declared_size"),
    EVERY_PUSH_CONSTANT_CONTRACT,
)
def test_the_payload_is_a_multiple_of_four_bytes(
    shader_file_name: str, packing_format: str, declared_size: int
) -> None:
    """Vulkan refuses a push-constant range whose size is not."""
    del shader_file_name, packing_format
    assert declared_size % 4 == 0


def test_the_block_reader_refuses_a_layout_it_cannot_compute() -> None:
    """The reader guessing would make every test above vacuous."""
    with pytest.raises(ValueError, match="no std430 layout known"):
        push_constant_block_size_of(
            "layout(push_constant, std430) uniform Block { mat4 unsupported; };"
        )
