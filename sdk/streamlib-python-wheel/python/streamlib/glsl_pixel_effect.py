# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A pixel effect written as one GLSL function.

The user writes `vec4 effect(vec4 source, ivec2 at)` and a list of named
dials; this class supplies the rest of an ordinary compute kernel around it —
the source sampler, the output image, the workgroup tile, the push-constant
block — and per frame lands the frame with the engine copy, dispatches, and
hands back the output bag. Wheel grammar over `create_compute_kernel`,
`ProcessorOutputTextureRing`, `copy_surface_to_surface` and `dispatch`; the
engine sees nothing but a kernel and a copy.

Pre-declared for the body, beside `dials.<name>` for each declared dial:

- `ivec2 streamlib_extent` — the frame's width and height.
- `float streamlib_elapsed_seconds` — monotonic seconds since the first apply.
- `vec4 streamlib_source_at(ivec2 at)` — the source texel at `at`, clamped
  to the frame's edge.
- `vec4 streamlib_source_uv(vec2 uv)` — the source bilinearly sampled at
  normalized `uv`.
"""

from __future__ import annotations

import re
import struct
from collections.abc import Mapping, Sequence
from dataclasses import asdict, dataclass
from typing import Any, Literal, Union

from ._engine import (
    ComputeKernel,
    GpuContextFullAccess,
    GpuContextLimitedAccess,
    monotonic_now_ns,
)
from .processor_output_texture_ring import ProcessorOutputTextureRing
from .video_frame import VideoFrame

__all__ = ["GlslPixelEffect"]

GlslPixelEffectDialType = Literal["float", "int", "vec2", "vec4"]

_GpuContextWithSurfaceCopy = Union[GpuContextLimitedAccess, GpuContextFullAccess]

# Vulkan's required minimum for `maxPushConstantsSize`: the one size every
# device on every floor is guaranteed to accept.
_PUSH_CONSTANT_BLOCK_BYTE_LIMIT = 128

# The `local_size` the template declares and the tile the dispatch counts in.
_WORKGROUP_TILE_SIZE_IN_PIXELS = 8

_EFFECT_TEXTURE_FORMAT = "rgba8_unorm"
_SOURCE_LANDING_TEXTURE_USAGE = ["texture_binding"]
_OUTPUT_TEXTURE_USAGE = ["storage_binding", "texture_binding"]

_SOURCE_SAMPLER_BINDING_NAME = "streamlib_source"
_OUTPUT_STORAGE_IMAGE_BINDING_NAME = "streamlib_output"

_ELAPSED_SECONDS_PUSH_CONSTANT_MEMBER_NAME = "streamlib_elapsed_seconds_since_first_apply"


@dataclass(frozen=True)
class _DialLayout:
    """One dial type's std430 footprint — push-constant blocks lay out as
    std430 — and the little-endian `struct` code of one component."""

    byte_alignment: int
    byte_size: int
    component_count: int
    struct_component_code: str


_DIAL_LAYOUT_BY_TYPE: "dict[str, _DialLayout]" = {
    "float": _DialLayout(4, 4, 1, "f"),
    "int": _DialLayout(4, 4, 1, "i"),
    "vec2": _DialLayout(8, 8, 2, "f"),
    "vec4": _DialLayout(16, 16, 4, "f"),
}

_INT32_RANGE = range(-(2**31), 2**31)

_GLSL_IDENTIFIER = re.compile(r"[A-Za-z_][A-Za-z0-9_]*\Z")

_GLSL_COMMENT = re.compile(r"//[^\n]*|/\*.*?\*/", re.DOTALL)

_EFFECT_SIGNATURE = re.compile(
    r"\bvec4\s+effect\s*\(\s*(?:const\s+)?(?:in\s+)?vec4\s+\w+\s*,"
    r"\s*(?:const\s+)?(?:in\s+)?ivec2\s+\w+\s*\)"
)

_REQUIRED_SIGNATURE = "vec4 effect(vec4 source, ivec2 at)"


def _workgroups_covering(pixels: int) -> int:
    return (
        pixels + _WORKGROUP_TILE_SIZE_IN_PIXELS - 1
    ) // _WORKGROUP_TILE_SIZE_IN_PIXELS


def _is_plain_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _is_plain_number(value: Any) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


@dataclass(frozen=True)
class _PushConstantMember:
    name: str
    glsl_type: str
    offset: int

    @property
    def layout(self) -> _DialLayout:
        return _DIAL_LAYOUT_BY_TYPE[self.glsl_type]


@dataclass(frozen=True)
class _PushConstantBlockLayout:
    """The template's elapsed-seconds member, the user's dials after it, and
    the block's size as reflection reports it — the end of its last member."""

    elapsed_seconds_member: _PushConstantMember
    dial_members: "tuple[_PushConstantMember, ...]"
    byte_size: int

    @property
    def members(self) -> "tuple[_PushConstantMember, ...]":
        return (self.elapsed_seconds_member, *self.dial_members)


def _refuse_malformed_dial_declaration(name: Any, glsl_type: Any) -> None:
    if not isinstance(name, str) or not _GLSL_IDENTIFIER.match(name):
        raise ValueError(
            f"GlslPixelEffect.compile: dial {name!r} is not a GLSL identifier — "
            f"the body reads it as `dials.<name>`"
        )
    if name.startswith(("streamlib_", "gl_")) or "__" in name:
        raise ValueError(
            f"GlslPixelEffect.compile: dial {name!r} uses a reserved name — "
            f"`streamlib_` and `gl_` prefixes and `__` belong to the template "
            f"and to GLSL"
        )
    if glsl_type == "vec3":
        raise ValueError(
            f"GlslPixelEffect.compile: dial {name!r} is a vec3, which is refused — "
            f"a vec3 aligns to 16 bytes but packs 12, shifting every dial after "
            f"it; declare it as a vec4 and ignore the fourth component"
        )
    if glsl_type not in _DIAL_LAYOUT_BY_TYPE:
        raise ValueError(
            f"GlslPixelEffect.compile: dial {name!r} has type {glsl_type!r}; a "
            f"dial is one of {', '.join(_DIAL_LAYOUT_BY_TYPE)}"
        )


def _lay_out_push_constant_block(
    dial_types_by_name: Mapping[str, str],
) -> _PushConstantBlockLayout:
    elapsed_seconds_member = _PushConstantMember(
        _ELAPSED_SECONDS_PUSH_CONSTANT_MEMBER_NAME, "float", 0
    )
    end = elapsed_seconds_member.layout.byte_size
    dial_members: "list[_PushConstantMember]" = []
    for name, glsl_type in dial_types_by_name.items():
        _refuse_malformed_dial_declaration(name, glsl_type)
        layout = _DIAL_LAYOUT_BY_TYPE[glsl_type]
        offset = (end + layout.byte_alignment - 1) // layout.byte_alignment * layout.byte_alignment
        end = offset + layout.byte_size
        if end > _PUSH_CONSTANT_BLOCK_BYTE_LIMIT:
            raise ValueError(
                f"GlslPixelEffect.compile: dial {name!r} ends the push-constant "
                f"block at {end} bytes, past the {_PUSH_CONSTANT_BLOCK_BYTE_LIMIT} "
                f"every GPU guarantees — the block holds 4 bytes of elapsed "
                f"seconds and then the dials in declaration order; declare fewer "
                f"or smaller dials"
            )
        dial_members.append(_PushConstantMember(name, glsl_type, offset))
    return _PushConstantBlockLayout(elapsed_seconds_member, tuple(dial_members), end)


def _compute_kernel_glsl(effect_glsl: str, block_layout: _PushConstantBlockLayout) -> str:
    push_constant_block_members = "".join(
        f"    {member.glsl_type} {member.name};\n" for member in block_layout.members
    )
    # `#line 1` sits directly above the body so a compiler diagnostic names
    # the line of `effect_glsl` the user wrote, not the template's.
    #
    # `main` reads the push-constant block on every path, through a guard that
    # never fires: the optimizer strips a block nothing reads, and the engine
    # refuses a kernel whose declared push-constant size is not the one its
    # SPIR-V reflects — so a body reading no dial would otherwise not build.
    return (
        "#version 450\n"
        f"layout(local_size_x = {_WORKGROUP_TILE_SIZE_IN_PIXELS}, "
        f"local_size_y = {_WORKGROUP_TILE_SIZE_IN_PIXELS}) in;\n"
        f"layout(set = 0, binding = 0) uniform sampler2D {_SOURCE_SAMPLER_BINDING_NAME};\n"
        "layout(set = 0, binding = 1, rgba8) uniform writeonly image2D "
        f"{_OUTPUT_STORAGE_IMAGE_BINDING_NAME};\n"
        "layout(push_constant) uniform GlslPixelEffectDials {\n"
        f"{push_constant_block_members}"
        "} dials;\n"
        "ivec2 streamlib_extent;\n"
        "float streamlib_elapsed_seconds;\n"
        "vec4 streamlib_source_at(ivec2 at) {\n"
        f"    return texelFetch({_SOURCE_SAMPLER_BINDING_NAME}, "
        "clamp(at, ivec2(0), streamlib_extent - 1), 0);\n"
        "}\n"
        "vec4 streamlib_source_uv(vec2 uv) {\n"
        f"    return textureLod({_SOURCE_SAMPLER_BINDING_NAME}, uv, 0.0);\n"
        "}\n"
        "#line 1\n"
        f"{effect_glsl}\n"
        "void main() {\n"
        "    ivec2 at = ivec2(gl_GlobalInvocationID.xy);\n"
        f"    streamlib_extent = textureSize({_SOURCE_SAMPLER_BINDING_NAME}, 0);\n"
        "    streamlib_elapsed_seconds = "
        f"dials.{block_layout.elapsed_seconds_member.name};\n"
        "    if (at.x >= streamlib_extent.x || at.y >= streamlib_extent.y\n"
        "            || streamlib_elapsed_seconds < 0.0) {\n"
        "        return;\n"
        "    }\n"
        f"    imageStore({_OUTPUT_STORAGE_IMAGE_BINDING_NAME}, at, "
        f"effect(texelFetch({_SOURCE_SAMPLER_BINDING_NAME}, at, 0), at));\n"
        "}\n"
    )


class GlslPixelEffect:
    """A pixel effect built from one GLSL `effect` function, applied frame by frame."""

    def __init__(
        self, compute_kernel: ComputeKernel, push_constant_block_layout: _PushConstantBlockLayout
    ) -> None:
        self._compute_kernel = compute_kernel
        self._push_constant_block_layout = push_constant_block_layout
        self._declared_dial_names = frozenset(
            member.name for member in push_constant_block_layout.dial_members
        )
        self._source_landing_ring = ProcessorOutputTextureRing(
            _EFFECT_TEXTURE_FORMAT, _SOURCE_LANDING_TEXTURE_USAGE, depth=1
        )
        self._output_ring = ProcessorOutputTextureRing(
            _EFFECT_TEXTURE_FORMAT, _OUTPUT_TEXTURE_USAGE
        )
        self._first_apply_monotonic_ns: "int | None" = None

    @classmethod
    def compile(
        cls,
        gpu_full_access: GpuContextFullAccess,
        effect_glsl: str,
        dials: "Mapping[str, GlslPixelEffectDialType] | None" = None,
    ) -> "GlslPixelEffect":
        """Build the effect's compute kernel around `effect_glsl`, in `setup()`.

        `dials` maps each dial's name to its GLSL type, in the order the push
        constants lay out. Raises naming the fix for a body without the
        `effect` function, a malformed dial, and a push-constant block over
        128 bytes; a compiler diagnostic names the body's own line.
        """
        if not isinstance(effect_glsl, str):
            raise TypeError(
                f"GlslPixelEffect.compile: effect_glsl must be GLSL source as a "
                f"str, got {type(effect_glsl).__name__}"
            )
        if not _EFFECT_SIGNATURE.search(_GLSL_COMMENT.sub("", effect_glsl)):
            raise ValueError(
                f"GlslPixelEffect.compile: effect_glsl defines no "
                f"`{_REQUIRED_SIGNATURE}` — the effect is that one function, "
                f"returning the output pixel for the source pixel at `at`"
            )
        block_layout = _lay_out_push_constant_block(dict(dials or {}))
        compute_kernel = gpu_full_access.create_compute_kernel(
            source=_compute_kernel_glsl(effect_glsl, block_layout),
            push_constant_size=block_layout.byte_size,
            bindings={
                _SOURCE_SAMPLER_BINDING_NAME: "sampled_texture",
                _OUTPUT_STORAGE_IMAGE_BINDING_NAME: "storage_image",
            },
        )
        return cls(compute_kernel, block_layout)

    def apply_to_frame(
        self,
        gpu_limited_access: _GpuContextWithSurfaceCopy,
        frame: VideoFrame,
        dials: "Mapping[str, float | Sequence[float]] | None" = None,
    ) -> "dict[str, Any]":
        """Run the effect over `frame` and return the output frame's bag, in `process()`.

        Every declared dial is supplied on every apply; none persists. The
        output is an `rgba8_unorm` frame at the source's extent, carrying its
        timestamp and colour description. Raises naming the dial for an
        undeclared, missing or ill-typed one, and naming the frame when the
        engine copy refuses to land it.
        """
        push_constants = self._pack_push_constants(dict(dials or {}))

        source_landing_texture = self._source_landing_ring.next_texture_for_this_frame(
            gpu_limited_access, frame.width, frame.height
        )
        try:
            gpu_limited_access.copy_surface_to_surface(
                frame.surface_id, source_landing_texture
            )
        except RuntimeError as copy_refusal:
            raise ValueError(
                f"GlslPixelEffect.apply_to_frame: frame {frame.surface_id!r} "
                f"({frame.width}x{frame.height}) could not land in the effect's "
                f"{_EFFECT_TEXTURE_FORMAT} source — the effect takes one single-plane RGBA "
                f"frame: {copy_refusal}"
            ) from copy_refusal

        output_texture = self._output_ring.next_texture_for_this_frame(
            gpu_limited_access, frame.width, frame.height
        )
        self._compute_kernel.dispatch(
            bindings={
                _SOURCE_SAMPLER_BINDING_NAME: source_landing_texture,
                _OUTPUT_STORAGE_IMAGE_BINDING_NAME: output_texture,
            },
            group_count=(
                _workgroups_covering(frame.width),
                _workgroups_covering(frame.height),
                1,
            ),
            push_constants=push_constants,
        )

        output_bag: "dict[str, Any]" = {
            "surface_id": output_texture.surface_id,
            "width": frame.width,
            "height": frame.height,
            # The capture's timestamp, not the effect's: downstream orders by it.
            "timestamp_ns": frame.timestamp_ns,
        }
        if frame.color_info is not None:
            output_bag["color_info"] = {
                axis: value
                for axis, value in asdict(frame.color_info).items()
                if value is not None
            }
        return output_bag

    def _pack_push_constants(self, supplied_dials: "dict[str, Any]") -> bytes:
        for name in supplied_dials:
            if name not in self._declared_dial_names:
                raise ValueError(
                    f"GlslPixelEffect.apply_to_frame: dial {name!r} was not declared "
                    f"— add it to `dials=` in GlslPixelEffect.compile"
                )
        components_by_dial_member = [
            (member, _dial_components(member, supplied_dials))
            for member in self._push_constant_block_layout.dial_members
        ]

        now_ns = monotonic_now_ns()
        if self._first_apply_monotonic_ns is None:
            self._first_apply_monotonic_ns = now_ns
        elapsed_seconds = (now_ns - self._first_apply_monotonic_ns) / 1e9

        block = bytearray(self._push_constant_block_layout.byte_size)
        elapsed_seconds_member = self._push_constant_block_layout.elapsed_seconds_member
        _pack_member_into(block, elapsed_seconds_member, [elapsed_seconds])
        for member, components in components_by_dial_member:
            _pack_member_into(block, member, components)
        return bytes(block)


def _pack_member_into(
    block: bytearray, member: _PushConstantMember, components: "list[float | int]"
) -> None:
    struct.pack_into(
        f"<{member.layout.component_count}{member.layout.struct_component_code}",
        block,
        member.offset,
        *components,
    )


def _dial_components(
    member: _PushConstantMember, supplied_dials: "dict[str, Any]"
) -> "list[float | int]":
    if member.name not in supplied_dials:
        raise ValueError(
            f"GlslPixelEffect.apply_to_frame: dial {member.name!r} was not "
            f"supplied — every apply supplies every declared dial; none "
            f"persists from the last frame"
        )
    value = supplied_dials[member.name]
    if member.glsl_type == "int":
        if not _is_plain_int(value) or value not in _INT32_RANGE:
            raise ValueError(
                f"GlslPixelEffect.apply_to_frame: dial {member.name!r} is an int and "
                f"was supplied {value!r} — supply a 32-bit int"
            )
        return [value]
    if member.glsl_type == "float":
        if not _is_plain_number(value):
            raise ValueError(
                f"GlslPixelEffect.apply_to_frame: dial {member.name!r} is a float and "
                f"was supplied {value!r} — supply a number"
            )
        return [float(value)]
    component_count = member.layout.component_count
    if (
        isinstance(value, (str, bytes))
        or not isinstance(value, Sequence)
        or len(value) != component_count
        or not all(_is_plain_number(component) for component in value)
    ):
        raise ValueError(
            f"GlslPixelEffect.apply_to_frame: dial {member.name!r} is a "
            f"{member.glsl_type} and was supplied {value!r} — supply "
            f"{component_count} numbers"
        )
    return [float(component) for component in value]
