# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probes for named-binding graphics draws, from where a kernel really runs.

A kernel is an object: built in `setup()` where the capability is Full, drawn
per frame in `process()`. Every probe runs in its own helper process and
reports one `MARKER:PROBE_RESULT` JSON line.

What is worth breaking a build over is that a Python processor can render a
pass — a fullscreen triangle sampling an acquired texture into an acquired
colour target, with no application-supplied bridge and no vertex buffer
anywhere — and that every way of getting the bindings wrong is refused by name:
the stage ones before a kernel exists, the rest before any GPU work is
submitted.
"""

import json
import os
import traceback
from collections.abc import Sequence

from streamlib import (
    GpuContextFullAccess,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    log,
    processor,
)

SURFACE_WIDTH = 64
SURFACE_HEIGHT = 64

COLOR_ATTACHMENT_FORMAT = "rgba8_unorm"

# A colour target must carry RENDER_ATTACHMENT; a sampled input must not have
# to, which is the whole difference between the two acquires.
SAMPLED_INPUT_TEXTURE_USAGE = [
    "texture_binding",
    "storage_binding",
    "copy_src",
    "copy_dst",
]
COLOR_TARGET_TEXTURE_USAGE = [
    "render_attachment",
    "texture_binding",
    "copy_src",
    "copy_dst",
]

RESULT_MARKER = "MARKER:PROBE_RESULT "

# The fragment stage's own name for the texture it samples. Nothing but the
# shader declares it, and a draw resolves against it.
SOURCE_BINDING = "source_image"

# The vertices are the shaders' own: no escalate op mints a vertex buffer, so
# the positions come out of `gl_VertexIndex`.
FULL_SCREEN_TRIANGLE_VERTEX_GLSL = """\
#version 450
void main() {
    vec2 corner = vec2((gl_VertexIndex << 1) & 2, gl_VertexIndex & 2);
    gl_Position = vec4(corner * 2.0 - 1.0, 0.0, 1.0);
}
"""

INVERT_SAMPLED_INPUT_FRAGMENT_GLSL = """\
#version 450
layout(set = 0, binding = 0) uniform sampler2D source_image;
layout(location = 0) out vec4 painted_colour;
void main() {
    vec4 source = texelFetch(source_image, ivec2(gl_FragCoord.xy), 0);
    painted_colour = vec4(vec3(1.0) - source.rgb, source.a);
}
"""

# A second binding no draw can ever name a surface for: the only by-surface-id
# resolution the engine has is texture-shaped.
TINTED_SAMPLED_INPUT_FRAGMENT_GLSL = """\
#version 450
layout(set = 0, binding = 0) uniform sampler2D source_image;
layout(set = 0, binding = 1) uniform TintBlock { vec4 tint; } tint_block;
layout(location = 0) out vec4 painted_colour;
void main() {
    vec4 source = texelFetch(source_image, ivec2(gl_FragCoord.xy), 0);
    painted_colour = source * tint_block.tint;
}
"""

# The fragment stage is the only one that reads the texture, which is what a
# declaration naming the vertex stage contradicts. Spelled out because a dict's
# value type is invariant: the shape has to be the parameter's own, not the
# narrower one this literal would otherwise infer.
DECLARED_BINDINGS: dict[str, str | tuple[str, Sequence[str]]] = {
    SOURCE_BINDING: ("sampled_texture", ["fragment"])
}


def _report(probe_body) -> None:
    """One result line per probe, success or failure — the failure carries the
    traceback so the test fails on the cause rather than a missing marker."""
    try:
        observation = probe_body()
    except BaseException:  # noqa: BLE001 — re-raised by the asserting test
        observation = {"failure": traceback.format_exc()}
    log.info(RESULT_MARKER + json.dumps({"pid": os.getpid(), **observation}))


def _refusal_of(refused_call) -> str:
    """The message a wrong call raises, or a failure if it did not raise."""
    try:
        refused_call()
    except Exception as refusal:  # noqa: BLE001 — the refusal is the subject
        return str(refusal)
    raise AssertionError("the call was accepted; it should have been refused")


def _refusal_traceback_of(refused_call) -> str:
    """The traceback a wrong call raises, so a test can assert *which line*
    refused — construction or dispatch — rather than only what it said."""
    try:
        refused_call()
    except Exception:  # noqa: BLE001 — the refusal is the subject
        return traceback.format_exc()
    raise AssertionError("the call was accepted; it should have been refused")


def _invert_sampled_input_graphics_kernel(
    gpu: GpuContextFullAccess,
    bindings: dict[str, str | tuple[str, Sequence[str]]],
):
    """The conformance pass: a fullscreen triangle sampling one texture into
    one colour target."""
    return gpu.create_graphics_kernel(
        color_attachment_formats=[COLOR_ATTACHMENT_FORMAT],
        vertex_source=FULL_SCREEN_TRIANGLE_VERTEX_GLSL,
        fragment_source=INVERT_SAMPLED_INPUT_FRAGMENT_GLSL,
        bindings=bindings,
        label="python-fullscreen-triangle",
    )


class _GraphicsKernelProbeBase:
    """Builds the conformance kernel in `setup`, reports from `setup`.

    Nothing upstream is needed: the probe acquires both surfaces itself, which
    is the point — a draw's colour target is an engine-owned texture the
    processor names, not something handed to it.
    """

    # Declared, not merely assigned: `setup` assigns it inside a nested
    # closure, which a type checker does not walk for attribute inference.
    gpu_full_access: GpuContextFullAccess

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        def observe() -> dict:
            gpu = ctx.gpu_full_access
            # Held for probes whose observation needs the capability itself
            # (a refusal at construction is observed by constructing).
            self.gpu_full_access = gpu
            kernel = _invert_sampled_input_graphics_kernel(gpu, DECLARED_BINDINGS)
            source = gpu.acquire_texture(
                SURFACE_WIDTH,
                SURFACE_HEIGHT,
                COLOR_ATTACHMENT_FORMAT,
                SAMPLED_INPUT_TEXTURE_USAGE,
            )
            color_target = gpu.acquire_texture(
                SURFACE_WIDTH,
                SURFACE_HEIGHT,
                COLOR_ATTACHMENT_FORMAT,
                COLOR_TARGET_TEXTURE_USAGE,
            )
            return self.observe(kernel, source, color_target)

        _report(observe)

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        pass

    def draw_the_conformance_pass(self, kernel, source, color_target) -> None:
        """The one draw every probe here spells the same way."""
        kernel.draw(
            bindings={SOURCE_BINDING: source},
            color_targets=[color_target],
            extent=(SURFACE_WIDTH, SURFACE_HEIGHT),
            vertex_count=3,
        )

    def observe(self, kernel, source, color_target) -> dict:
        raise NotImplementedError


@processor(
    execution="manual",
    description="Draws a fullscreen triangle sampling one texture into another",
)
class FullscreenTriangleDrawProbe(_GraphicsKernelProbeBase):
    """The demo: a Python processor renders a pass with named bindings.

    No bridge is installed anywhere, no vertex buffer exists, and the colour
    target is a texture this processor acquired rather than one an application
    handed it.
    """

    def observe(self, kernel, source, color_target) -> dict:
        self.draw_the_conformance_pass(kernel, source, color_target)
        # Twice, over the same surfaces: the kernel keeps no binding state, so
        # a second draw that needed the first one's descriptors would fail
        # here rather than in whatever runs next.
        self.draw_the_conformance_pass(kernel, source, color_target)
        return {
            "drew": True,
            "binding_names": list(kernel.binding_names),
            "source_surface_id": source.surface_id,
            "color_target_surface_id": color_target.surface_id,
            "surfaces_are_distinct": source.surface_id != color_target.surface_id,
        }


@processor(
    execution="manual",
    description="Every way of getting a draw's bindings wrong, refused by name",
)
class GraphicsBindingRefusalProbe(_GraphicsKernelProbeBase):
    """Unknown, missing, kind-mismatched and unresolvable, each raising with a
    message naming what the shaders actually declare."""

    def observe(self, kernel, source, color_target) -> dict:
        gpu = self.gpu_full_access

        unknown_at_construction = _refusal_of(
            lambda: _invert_sampled_input_graphics_kernel(
                gpu, {"tint_amount": ("sampled_texture", ["fragment"])}
            )
        )
        kind_mismatch_at_construction = _refusal_of(
            lambda: _invert_sampled_input_graphics_kernel(
                gpu, {SOURCE_BINDING: ("storage_image", ["fragment"])}
            )
        )
        # A declaration is total: leaving one of the shaders' bindings out is
        # how a draw silently binds nothing.
        undeclared_at_construction = _refusal_of(
            lambda: gpu.create_graphics_kernel(
                color_attachment_formats=[COLOR_ATTACHMENT_FORMAT],
                vertex_source=FULL_SCREEN_TRIANGLE_VERTEX_GLSL,
                fragment_source=TINTED_SAMPLED_INPUT_FRAGMENT_GLSL,
                bindings=DECLARED_BINDINGS,
            )
        )

        unknown_at_draw = _refusal_of(
            lambda: kernel.draw(
                bindings={SOURCE_BINDING: source, "tint_amount": source},
                color_targets=[color_target],
                extent=(SURFACE_WIDTH, SURFACE_HEIGHT),
                vertex_count=3,
            )
        )
        missing_at_draw = _refusal_of(
            lambda: kernel.draw(
                bindings={},
                color_targets=[color_target],
                extent=(SURFACE_WIDTH, SURFACE_HEIGHT),
                vertex_count=3,
            )
        )
        unregistered_surface_at_draw = _refusal_of(
            lambda: kernel.draw(
                bindings={SOURCE_BINDING: "no-such-surface"},
                color_targets=[color_target],
                extent=(SURFACE_WIDTH, SURFACE_HEIGHT),
                vertex_count=3,
            )
        )

        # The kernel still draws: every refusal above raised before anything
        # was submitted, so none of them left it holding half a draw's state.
        self.draw_the_conformance_pass(kernel, source, color_target)
        return {
            "unknown_at_construction": unknown_at_construction,
            "kind_mismatch_at_construction": kind_mismatch_at_construction,
            "undeclared_at_construction": undeclared_at_construction,
            "unknown_at_draw": unknown_at_draw,
            "missing_at_draw": missing_at_draw,
            "unregistered_surface_at_draw": unregistered_surface_at_draw,
            "drew_after_the_refusals": True,
            "binding_names": list(kernel.binding_names),
        }


@processor(
    execution="manual",
    description="A binding declared for a stage the shaders do not read it in",
)
class GraphicsStageMismatchProbe(_GraphicsKernelProbeBase):
    """The ticket's named validation case, at the line it belongs to.

    A graphics kernel is always built from both stages, so the stage claim a
    declaration can get wrong is *which* of them reads the binding. A draw
    never revisits that, so the mistake has to refuse at construction — and the
    traceback proves it did, because there is no kernel object to draw with
    afterwards.
    """

    def observe(self, kernel, source, color_target) -> dict:
        gpu = self.gpu_full_access

        def declare_the_texture_for_a_stage_that_does_not_read_it() -> None:
            gpu.create_graphics_kernel(
                color_attachment_formats=[COLOR_ATTACHMENT_FORMAT],
                vertex_source=FULL_SCREEN_TRIANGLE_VERTEX_GLSL,
                fragment_source=INVERT_SAMPLED_INPUT_FRAGMENT_GLSL,
                bindings={SOURCE_BINDING: ("sampled_texture", ["vertex"])},
            )

        stage_mismatch = _refusal_of(
            declare_the_texture_for_a_stage_that_does_not_read_it
        )
        stage_mismatch_traceback = _refusal_traceback_of(
            declare_the_texture_for_a_stage_that_does_not_read_it
        )

        # The same shaders with the stage claim corrected build and draw, so
        # what the refusal rejected is the declaration and not the pass.
        self.draw_the_conformance_pass(kernel, source, color_target)
        return {
            "stage_mismatch": stage_mismatch,
            "stage_mismatch_traceback": stage_mismatch_traceback,
            "the_corrected_declaration_drew": True,
        }


@processor(
    execution="manual",
    description="A buffer-kind binding a draw cannot name a surface for",
)
class GraphicsBufferBindingRefusalProbe(_GraphicsKernelProbeBase):
    """A uniform-buffer binding is reflected, declared and refused at the draw.

    The only by-surface-id resolution the engine has is texture-shaped, so a
    draw that accepted a surface here would bind whatever the descriptor last
    held. The name is read back off the kernel rather than spelled here — how
    reflection names a uniform block is the shader's business, and the refusal
    has to name whatever it named.
    """

    def observe(self, kernel, source, color_target) -> dict:
        del kernel
        tinted = self.gpu_full_access.create_graphics_kernel(
            color_attachment_formats=[COLOR_ATTACHMENT_FORMAT],
            vertex_source=FULL_SCREEN_TRIANGLE_VERTEX_GLSL,
            fragment_source=TINTED_SAMPLED_INPUT_FRAGMENT_GLSL,
            label="python-tinted-fullscreen-triangle",
        )
        binding_names = list(tinted.binding_names)
        buffer_binding = binding_names[1]
        buffer_kind_binding = _refusal_of(
            lambda: tinted.draw(
                bindings={SOURCE_BINDING: source, buffer_binding: source},
                color_targets=[color_target],
                extent=(SURFACE_WIDTH, SURFACE_HEIGHT),
                vertex_count=3,
            )
        )
        return {
            "binding_names": binding_names,
            "buffer_binding": buffer_binding,
            "buffer_kind_binding": buffer_kind_binding,
        }


@processor(
    execution="manual",
    description="Pass shapes a Python draw cannot ask for",
)
class GraphicsPassShapeRefusalProbe(_GraphicsKernelProbeBase):
    """Vertex buffers, an index buffer and a depth target are not arguments.

    No escalate op mints a vertex or an index buffer, and the offscreen pass a
    draw runs attaches colour targets only — so rather than accepting the
    argument and dropping it, the surface has no such argument, and the
    attachment count it does take is checked before anything is submitted.
    """

    def observe(self, kernel, source, color_target) -> dict:
        gpu = self.gpu_full_access

        def draw_with(**unsupported) -> None:
            kernel.draw(
                bindings={SOURCE_BINDING: source},
                color_targets=[color_target],
                extent=(SURFACE_WIDTH, SURFACE_HEIGHT),
                vertex_count=3,
                **unsupported,
            )

        two_color_targets = _refusal_of(
            lambda: kernel.draw(
                bindings={SOURCE_BINDING: source},
                color_targets=[color_target, color_target],
                extent=(SURFACE_WIDTH, SURFACE_HEIGHT),
                vertex_count=3,
            )
        )
        no_color_target = _refusal_of(
            lambda: kernel.draw(
                bindings={SOURCE_BINDING: source},
                color_targets=[],
                extent=(SURFACE_WIDTH, SURFACE_HEIGHT),
                vertex_count=3,
            )
        )
        two_attachment_formats = _refusal_of(
            lambda: gpu.create_graphics_kernel(
                color_attachment_formats=[
                    COLOR_ATTACHMENT_FORMAT,
                    COLOR_ATTACHMENT_FORMAT,
                ],
                vertex_source=FULL_SCREEN_TRIANGLE_VERTEX_GLSL,
                fragment_source=INVERT_SAMPLED_INPUT_FRAGMENT_GLSL,
                bindings=DECLARED_BINDINGS,
            )
        )
        return {
            "vertex_buffers": _refusal_of(lambda: draw_with(vertex_buffers=[])),
            "index_buffer": _refusal_of(lambda: draw_with(index_buffer=None)),
            "depth_target": _refusal_of(lambda: draw_with(depth_target=color_target)),
            "two_color_targets": two_color_targets,
            "no_color_target": no_color_target,
            "two_attachment_formats": two_attachment_formats,
        }
