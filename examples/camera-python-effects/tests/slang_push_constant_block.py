# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Reads a shader's push-constant block back out of its Slang.

The tests use it to hold each node's `struct.pack` against the block the
shader actually declares. Nothing at run time reads a shader this way — the
engine reflects the compiled SPIR-V — but a mismatch there surfaces as garbled
pixels or a refused draw at run time, and here it surfaces as a failing test.

The block is the `struct` a `[[vk::push_constant]] ConstantBuffer<…>` names,
laid out as SPIR-V lays out a push-constant block. Enough of that layout to
cover what these shaders use: scalars, `float2`, `float4`, and arrays of
scalars. Anything else raises rather than guessing a layout.
"""

from __future__ import annotations

import re

__all__ = ["SCALAR_AND_VECTOR_LAYOUTS", "push_constant_block_size_of"]

# Slang type → (size in bytes, alignment in bytes).
SCALAR_AND_VECTOR_LAYOUTS = {
    "float": (4, 4),
    "int": (4, 4),
    "uint": (4, 4),
    "float2": (8, 8),
    "float4": (16, 16),
}

_PUSH_CONSTANT_BUFFER = re.compile(
    r"\[\[vk::push_constant\]\]\s*ConstantBuffer\s*<\s*(?P<struct_name>\w+)\s*>"
)
_MEMBER = re.compile(r"^(?P<type>\w+)\s+(?P<name>\w+)(?:\[(?P<count>\d+)\])?$")


def _aligned_up(offset: int, alignment: int) -> int:
    return (offset + alignment - 1) // alignment * alignment


def _struct_body(shader_source: str, struct_name: str) -> str:
    struct = re.search(
        rf"struct\s+{struct_name}\s*\{{(?P<body>[^}}]*)\}}", shader_source, re.DOTALL
    )
    if struct is None:
        raise ValueError(f"this shader names push-constant struct {struct_name!r} but never declares it")
    return struct.group("body")


def push_constant_block_size_of(shader_source: str) -> int:
    """The byte size of the shader's push-constant block."""
    push_constant_buffer = _PUSH_CONSTANT_BUFFER.search(shader_source)
    if push_constant_buffer is None:
        raise ValueError("this shader declares no push-constant block")
    body = _struct_body(shader_source, push_constant_buffer.group("struct_name"))

    # Comments go first, then the split: a comment is free to contain a
    # semicolon, and one that did would cut a declaration in half.
    body_without_comments = " ".join(
        line.split("//")[0].strip() for line in body.splitlines()
    )

    offset = 0
    for statement in body_without_comments.split(";"):
        declaration = statement.strip()
        if not declaration:
            continue
        member = _MEMBER.match(declaration)
        if member is None:
            raise ValueError(f"unrecognized push-constant member: {declaration!r}")
        slang_type = member.group("type")
        if slang_type not in SCALAR_AND_VECTOR_LAYOUTS:
            raise ValueError(f"no push-constant layout known for Slang type {slang_type!r}")
        size, alignment = SCALAR_AND_VECTOR_LAYOUTS[slang_type]
        element_count = int(member.group("count") or 1)
        offset = _aligned_up(offset, alignment) + size * element_count
    return offset
