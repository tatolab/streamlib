# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`GlslPixelEffect`'s own refusals and packing, with the capabilities stood in for.

Everything here is decided in the wheel before any engine call: the body's
signature, the dial declarations, the push-constant layout, and the dials an
apply supplies. The pixels, the compiler's diagnostic line and the copy
refusal need a GPU and are proven in `test_glsl_pixel_effect.py`.
"""

from __future__ import annotations

import struct
from typing import Any, cast

import pytest

from tatolab.stream import (
    GlslPixelEffect,
    GpuContextFullAccess,
    GpuContextLimitedAccess,
    VideoFrame,
)

INVERT_GLSL = "vec4 effect(vec4 source, ivec2 at) { return vec4(1.0 - source.rgb, source.a); }"


class ComputeKernelStandIn:
    def __init__(self) -> None:
        self.dispatches: "list[dict[str, Any]]" = []

    def dispatch(self, **dispatch: Any) -> None:
        self.dispatches.append(dispatch)


class GpuFullAccessStandIn:
    """Records the kernel the effect asks for and answers a stand-in."""

    def __init__(self) -> None:
        self.kernel_requests: "list[dict[str, Any]]" = []
        self.kernel = ComputeKernelStandIn()

    def create_compute_kernel(self, **request: Any) -> ComputeKernelStandIn:
        self.kernel_requests.append(request)
        return self.kernel


class SurfaceHandleStandIn:
    def __init__(self, surface_id: str) -> None:
        self.surface_id = surface_id


class GpuLimitedAccessStandIn:
    """Answers numbered textures and records every engine call in order."""

    def __init__(self) -> None:
        self.calls: "list[str]" = []

    def acquire_texture_from_node_output_pool(
        self, pool_key: str, rotation_depth: int, width: int, height: int,
        texture_format: str, usage: "list[str]",
    ) -> SurfaceHandleStandIn:
        self.calls.append(f"acquire {texture_format} {width}x{height}")
        return SurfaceHandleStandIn(f"texture#{len(self.calls)}")

    def copy_surface_to_surface(
        self, source_surface_id: str, destination_surface: SurfaceHandleStandIn
    ) -> None:
        self.calls.append(f"copy {source_surface_id} -> {destination_surface.surface_id}")


def compiled(
    effect_glsl: str = INVERT_GLSL, dials: "dict[str, Any] | None" = None
) -> "tuple[GlslPixelEffect, GpuFullAccessStandIn]":
    gpu = GpuFullAccessStandIn()
    effect = GlslPixelEffect.compile(
        cast(GpuContextFullAccess, gpu), effect_glsl=effect_glsl, dials=dials
    )
    return effect, gpu


def refusal_of_compile(effect_glsl: str = INVERT_GLSL, dials: "dict[str, Any] | None" = None) -> str:
    gpu = GpuFullAccessStandIn()
    with pytest.raises(ValueError) as refusal:
        GlslPixelEffect.compile(
            cast(GpuContextFullAccess, gpu), effect_glsl=effect_glsl, dials=dials
        )
    assert gpu.kernel_requests == [], "a refusal must fire before the engine is asked"
    return str(refusal.value)


def frame(width: int = 20, height: int = 12) -> VideoFrame:
    return VideoFrame(
        surface_id="camera#7",
        width=width,
        height=height,
        timestamp_ns=123_456,
        color_info={"primaries": "bt709", "range": "full"},
    )


def apply(
    effect: GlslPixelEffect, dials: "dict[str, Any] | None" = None
) -> "tuple[dict[str, Any], GpuLimitedAccessStandIn]":
    gpu = GpuLimitedAccessStandIn()
    bag = effect.apply_to_frame(cast(GpuContextLimitedAccess, gpu), frame(), dials=dials)
    return bag, gpu


def refusal_of_apply(effect: GlslPixelEffect, dials: "dict[str, Any]") -> str:
    gpu = GpuLimitedAccessStandIn()
    with pytest.raises(ValueError) as refusal:
        effect.apply_to_frame(cast(GpuContextLimitedAccess, gpu), frame(), dials=dials)
    assert gpu.calls == [], "a dial refusal must fire before any GPU work"
    return str(refusal.value)


def test_a_body_without_the_effect_function_is_refused_before_compiling() -> None:
    message = refusal_of_compile("vec4 shade(vec4 source) { return source; }")
    assert "vec4 effect(vec4 source, ivec2 at)" in message


def test_an_effect_signature_only_in_a_comment_is_refused() -> None:
    message = refusal_of_compile(
        "// vec4 effect(vec4 source, ivec2 at)\nvec4 shade(vec4 s) { return s; }"
    )
    assert "vec4 effect(vec4 source, ivec2 at)" in message


def test_the_signature_check_accepts_any_parameter_names_and_qualifiers() -> None:
    compiled("vec4 effect(const in vec4 pixel, in ivec2 where) { return pixel; }")


def test_a_vec3_dial_is_refused_by_name() -> None:
    message = refusal_of_compile(dials={"strength": "float", "tint": "vec3"})
    assert "'tint'" in message
    assert "vec3" in message
    assert "vec4" in message


def test_a_dial_of_an_unknown_type_is_refused_listing_the_types() -> None:
    message = refusal_of_compile(dials={"warp": "mat4"})
    assert "'warp'" in message
    assert "float, int, vec2, vec4" in message


@pytest.mark.parametrize("name", ["streamlib_extent", "gl_Position", "two__words", "2fast", "a-b"])
def test_a_dial_name_the_body_cannot_read_is_refused(name: str) -> None:
    assert repr(name) in refusal_of_compile(dials={name: "float"})


def test_a_push_constant_block_over_128_bytes_is_refused_at_the_dial_that_crosses() -> None:
    thirty_one_floats_fill_the_block = {f"dial_{index}": "float" for index in range(31)}
    compiled(dials=thirty_one_floats_fill_the_block)

    message = refusal_of_compile(dials={**thirty_one_floats_fill_the_block, "one_too_many": "float"})
    assert "'one_too_many'" in message
    assert "132" in message
    assert "128" in message


def test_dials_lay_out_std430_after_the_elapsed_seconds() -> None:
    """float at 4, vec4 aligned up to 16, vec2 at 32, int at 40 — the size is
    the last member's end, which is what the engine's reflection reports."""
    effect, gpu = compiled(
        dials={"strength": "float", "tint": "vec4", "center": "vec2", "steps": "int"}
    )
    assert gpu.kernel_requests[0]["push_constant_size"] == 44

    apply(effect, {"strength": 0.5, "tint": (1, 2, 3, 4), "center": [5.0, 6.0], "steps": -7})
    push_constants = gpu.kernel.dispatches[0]["push_constants"]
    assert len(push_constants) == 44
    assert struct.unpack_from("<f", push_constants, 4) == (0.5,)
    assert struct.unpack_from("<4f", push_constants, 16) == (1.0, 2.0, 3.0, 4.0)
    assert struct.unpack_from("<2f", push_constants, 32) == (5.0, 6.0)
    assert struct.unpack_from("<i", push_constants, 40) == (-7,)


def test_the_first_apply_is_at_zero_elapsed_seconds() -> None:
    effect, gpu = compiled()
    apply(effect)
    assert gpu.kernel.dispatches[0]["push_constants"] == struct.pack("<f", 0.0)


def test_the_body_follows_line_one_so_diagnostics_name_its_own_lines() -> None:
    _, gpu = compiled()
    source = gpu.kernel_requests[0]["source"]
    assert f"#line 1\n{INVERT_GLSL}\n" in source


def test_a_dial_not_declared_at_compile_is_refused_at_apply() -> None:
    effect, _ = compiled(dials={"strength": "float"})
    message = refusal_of_apply(effect, {"strength": 1.0, "strenght": 1.0})
    assert "'strenght'" in message
    assert "compile" in message


def test_a_declared_dial_not_supplied_is_refused_at_apply() -> None:
    effect, _ = compiled(dials={"strength": "float"})
    message = refusal_of_apply(effect, {})
    assert "'strength'" in message
    assert "every apply" in message


@pytest.mark.parametrize(
    ("glsl_type", "value"),
    [
        ("float", "1.0"),
        ("float", True),
        ("int", 1.5),
        ("int", 2**31),
        ("vec2", (1.0, 2.0, 3.0)),
        ("vec4", "abcd"),
        ("vec4", (1.0, 2.0, 3.0, None)),
    ],
)
def test_a_dial_supplied_the_wrong_shape_is_refused_naming_it(glsl_type: str, value: object) -> None:
    effect, _ = compiled(dials={"knob": glsl_type})
    message = refusal_of_apply(effect, {"knob": value})
    assert "'knob'" in message
    assert glsl_type in message


def test_an_apply_lands_the_frame_then_dispatches_over_its_extent() -> None:
    effect, full_access = compiled()
    bag, limited_access = apply(effect)

    assert limited_access.calls == [
        "acquire rgba8_unorm 20x12",
        "copy camera#7 -> texture#1",
        "acquire rgba8_unorm 20x12",
    ]
    dispatch = full_access.kernel.dispatches[0]
    assert dispatch["group_count"] == (3, 2, 1)
    assert dispatch["bindings"]["streamlib_source"].surface_id == "texture#1"
    assert bag == {
        "surface_id": "texture#3",
        "width": 20,
        "height": 12,
        "timestamp_ns": 123_456,
        "color_info": {"primaries": "bt709", "range": "full"},
    }
