# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Reads a shader's `push_constant` block back out of its GLSL.

The tests use it to hold each processor's `struct.pack` against the block the
shader actually declares. Nothing at run time reads a shader this way — the
engine reflects the compiled SPIR-V — but a mismatch there surfaces as garbled
pixels or a refused draw at run time, and here it surfaces as a failing test.

Enough of std430 to cover what these shaders use: scalars, `vec2`, and arrays
of scalars. Anything else raises rather than guessing a layout.
"""

from __future__ import annotations

import re

__all__ = ["SCALAR_AND_VECTOR_LAYOUTS", "push_constant_block_size_of"]

# GLSL type → (size in bytes, std430 alignment in bytes).
SCALAR_AND_VECTOR_LAYOUTS = {
    "float": (4, 4),
    "int": (4, 4),
    "uint": (4, 4),
    "vec2": (8, 8),
    "vec4": (16, 16),
}

_PUSH_CONSTANT_BLOCK = re.compile(
    r"layout\s*\(\s*push_constant[^)]*\)\s*uniform\s+\w+\s*\{(?P<body>[^}]*)\}",
    re.DOTALL,
)
_MEMBER = re.compile(r"^(?P<type>\w+)\s+(?P<name>\w+)(?:\[(?P<count>\d+)\])?$")


def _aligned_up(offset: int, alignment: int) -> int:
    return (offset + alignment - 1) // alignment * alignment


def push_constant_block_size_of(shader_source: str) -> int:
    """The byte size of the shader's push-constant block, laid out std430."""
    block = _PUSH_CONSTANT_BLOCK.search(shader_source)
    if block is None:
        raise ValueError("this shader declares no push_constant block")

    # Comments go first, then the split: a comment is free to contain a
    # semicolon, and one that did used to cut a declaration in half.
    body_without_comments = " ".join(
        line.split("//")[0].strip() for line in block.group("body").splitlines()
    )

    offset = 0
    for statement in body_without_comments.split(";"):
        declaration = statement.strip()
        if not declaration:
            continue
        member = _MEMBER.match(declaration)
        if member is None:
            raise ValueError(f"unrecognized push-constant member: {declaration!r}")
        glsl_type = member.group("type")
        if glsl_type not in SCALAR_AND_VECTOR_LAYOUTS:
            raise ValueError(f"no std430 layout known for GLSL type {glsl_type!r}")
        size, alignment = SCALAR_AND_VECTOR_LAYOUTS[glsl_type]
        element_count = int(member.group("count") or 1)
        offset = _aligned_up(offset, alignment) + size * element_count
    return offset
